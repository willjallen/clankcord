use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::Path;
use std::process::Command;

use serde_json::{Value, json};

use crate::Result;
use crate::adapters::discord::api::send_message;
use crate::config::{
    MESSAGE_CHUNK_LIMIT, durable_dir, non_empty, split_message_chunks, string_field, write_json,
};
use crate::runtime::jobs::{
    DiscordPostMetadata, DiscordPostedMessageMetadata, WorkerAgentMetadata, WorkerJobMetadata,
    WorkerPreflightCheck, WorkerPreflightMetadata,
};
use crate::runtime::timeline::isoformat_z;
use crate::runtime::{Job, JobKind, JobState};

use crate::runtime::Runtime;
use crate::runtime::agents::AgentInfrastructureError;
use crate::runtime::util::{
    first_non_empty, job_cancel_requested, log, preview, voice_worker_agent_timeout_seconds,
};

const WORKER_COMMAND_DISPLAY: &str = "openclaw agent --local --agent clanky-voice-worker --json";

impl Runtime {
    pub fn dispatch_next_due_worker_job(&self) -> Result<Value> {
        let Some(job) = self.next_queued_job(JobKind::VoiceAgentTask)? else {
            return Ok(json!({"dispatched": false, "reason": "no queued worker jobs"}));
        };
        let job_id = job.id.clone();
        let attempts = job
            .metadata
            .worker()
            .map(|worker| worker.dispatch_attempts)
            .unwrap_or(0);
        if attempts >= 3 {
            let mut failed = job.clone();
            failed.set_state(JobState::WorkerDispatchFailed);
            self.timeline_store.update_job(&failed)?;
            return Ok(
                json!({"dispatched": false, "job": failed.to_value(), "reason": "worker dispatch attempts exhausted"}),
            );
        }

        let mut running = job.clone();
        running.mark_running();
        self.timeline_store.update_job(&running)?;

        match self.dispatch_openclaw_worker_job(&running) {
            Ok(dispatch_result) => {
                match self.complete_worker_job(job_id.clone(), running.clone(), dispatch_result) {
                    Ok(value) => Ok(value),
                    Err(error) => self.fail_worker_job(job_id, running, attempts, error),
                }
            }
            Err(error) => self.fail_worker_job(job_id, running, attempts, error),
        }
    }

    fn complete_worker_job(
        &self,
        job_id: String,
        fallback_job: Job,
        dispatch_result: WorkerJobMetadata,
    ) -> Result<Value> {
        let mut latest = self
            .timeline_store
            .get_job(&job_id)
            .unwrap_or_else(|_| fallback_job.clone());
        latest.metadata.set_worker(dispatch_result);
        if job_cancel_requested(&latest) {
            let cancelled_at = non_empty(
                latest.cancelled_at.clone().unwrap_or_default(),
                isoformat_z(None),
            );
            latest.mark_cancelled();
            latest.cancelled_at = Some(cancelled_at);
            latest.completed_at = Some(isoformat_z(None));
            latest.metadata.worker_mut().result_suppressed = true;
            self.timeline_store.update_job(&latest)?;
            self.timeline_store.append_event(
                &latest.guild_id,
                &latest.voice_channel_id,
                json!({
                    "event_kind": "worker_result_suppressed",
                    "kind": "worker_result_suppressed",
                    "job_id": job_id,
                    "job_kind": latest.kind.as_str(),
                    "reason": "job was cancelled before the worker result was posted",
                }),
            )?;
            return Ok(json!({"dispatched": true, "job": latest.to_value(), "cancelled": true}));
        }
        let response_text = latest
            .metadata
            .worker()
            .map(|worker| worker.response_text.clone())
            .unwrap_or_default();
        if !response_text.trim().is_empty() {
            let post_result = self.post_worker_job_result(&latest, &response_text)?;
            latest.metadata.worker_mut().discord_post = Some(post_result);
        }
        latest.mark_complete();
        self.timeline_store.update_job(&latest)?;
        Ok(json!({"dispatched": true, "job": latest.to_value()}))
    }

    fn fail_worker_job(
        &self,
        job_id: String,
        fallback_job: Job,
        attempts: i64,
        error: anyhow::Error,
    ) -> Result<Value> {
        let is_infrastructure_error = error.downcast_ref::<AgentInfrastructureError>().is_some();
        let error_text = error.to_string();
        let mut latest = self
            .timeline_store
            .get_job(&job_id)
            .unwrap_or_else(|_| fallback_job.clone());
        if job_cancel_requested(&latest) {
            let cancelled_at = non_empty(
                latest.cancelled_at.clone().unwrap_or_default(),
                isoformat_z(None),
            );
            latest.mark_cancelled();
            latest.cancelled_at = Some(cancelled_at);
            latest.metadata.worker_mut().dispatch_error_after_cancel = error_text;
            self.timeline_store.update_job(&latest)?;
            return Ok(json!({"dispatched": false, "job": latest.to_value(), "cancelled": true}));
        }
        let next_attempts = attempts + 1;
        latest.metadata.worker_mut().dispatch_attempts = if is_infrastructure_error {
            next_attempts.max(3)
        } else {
            next_attempts
        };
        latest.metadata.worker_mut().dispatch_error = error_text.clone();
        latest.set_state(if is_infrastructure_error || next_attempts >= 3 {
            JobState::WorkerDispatchFailed
        } else {
            JobState::Queued
        });
        self.timeline_store.update_job(&latest)?;
        log(&format!(
            "worker dispatch failed for {job_id}: {error_text}"
        ));
        Ok(json!({"dispatched": false, "job": latest.to_value(), "error": error_text}))
    }

    pub fn parse_openclaw_agent_stdout(stdout: &str) -> Value {
        let raw = stdout.trim();
        if raw.is_empty() {
            return json!({});
        }
        if let Ok(payload) = serde_json::from_str::<Value>(raw) {
            return payload;
        }
        let Some(start) = raw.find('{') else {
            return Value::String(raw.to_string());
        };
        let Some(end) = raw.rfind('}') else {
            return Value::String(raw.to_string());
        };
        if end <= start {
            return Value::String(raw.to_string());
        }
        serde_json::from_str::<Value>(&raw[start..=end])
            .unwrap_or_else(|_| Value::String(raw.to_string()))
    }

    pub fn openclaw_agent_response_text(stdout: &str) -> String {
        let payload = Self::parse_openclaw_agent_stdout(stdout);
        if let Some(payloads) = payload.get("payloads").and_then(Value::as_array) {
            let parts = payloads
                .iter()
                .filter_map(|entry| entry.get("text").and_then(Value::as_str))
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>();
            if !parts.is_empty() {
                return parts.join("\n\n").trim().to_string();
            }
        }
        if let Some(meta) = payload.get("meta").and_then(Value::as_object) {
            for key in ["finalAssistantVisibleText", "finalAssistantRawText"] {
                if let Some(text) = meta.get(key).and_then(Value::as_str).map(str::trim)
                    && !text.is_empty()
                {
                    return text.to_string();
                }
            }
        }
        stdout.trim().to_string()
    }

    pub fn voice_worker_env() -> BTreeMap<String, String> {
        let mut vars = env::vars().collect::<BTreeMap<_, _>>();
        vars.entry("CLAWCORD_API_BASE_URL".to_string())
            .or_insert_with(|| "http://127.0.0.1:8091".to_string());
        vars
    }

    pub(crate) fn run_worker_preflight(
        &self,
        envs: Option<&BTreeMap<String, String>>,
    ) -> WorkerPreflightMetadata {
        let worker_env = envs.cloned().unwrap_or_else(Self::voice_worker_env);
        let checks: Vec<Vec<&str>> = vec![
            vec!["jq", "--version"],
            vec!["sqlite3", "--version"],
            vec!["clawcord", "transcripts", "render", "--help"],
            vec!["clawcord", "transcripts", "search", "--help"],
            vec!["clawcord", "timeline", "range", "--help"],
            vec!["clawcord", "conversations", "list", "--help"],
            vec!["clawcord", "context", "resolve", "--help"],
            vec!["clawcord", "participants", "trace", "--help"],
            vec!["clawcord", "jobs", "get", "--help"],
        ];
        let mut results = Vec::new();
        for command in checks {
            let display = command.join(" ");
            match Command::new(command[0])
                .args(&command[1..])
                .envs(&worker_env)
                .output()
            {
                Ok(output) => {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    results.push(WorkerPreflightCheck {
                        command: display,
                        returncode: output.status.code(),
                        ok: output.status.success(),
                        stdout_preview: preview(&stdout, 500),
                        stderr_preview: preview(&stderr, 500),
                        error: String::new(),
                    });
                }
                Err(error) => {
                    results.push(WorkerPreflightCheck {
                        command: display,
                        returncode: None,
                        ok: false,
                        stdout_preview: String::new(),
                        stderr_preview: String::new(),
                        error: error.to_string(),
                    });
                }
            }
        }
        WorkerPreflightMetadata {
            ok: results.iter().all(|result| result.ok),
            checked_at: isoformat_z(None),
            checks: results,
        }
    }

    pub fn build_worker_agent_message(packet_path: &Path, packet: &Value) -> String {
        let compact_packet =
            serde_json::to_string_pretty(packet).unwrap_or_else(|_| "{}".to_string());
        [
            "You are handling a Clawcord job.",
            "",
            &format!("Job packet path: {}", packet_path.display()),
            "",
            "The full job packet is included below. Use it as the source of truth.",
            "Do not post to Discord yourself; Clawcord will post your final visible answer.",
            "Return only the message that should be posted to agent-chat. Do not wrap it in JSON.",
            "",
            "Follow the clanky-voice-worker role boundary from AGENTS.md: preserve the job lane abstraction and only perform side effects authorized by this job.",
            "",
            "Use the job payload as request evidence: requester, room, source events, router context, and raw activated speech.",
            "Read and rely on the Clawcord worker manual at /openclaw/state/workspaces/clanky-voice-worker/TOOLS.md for tool usage.",
            "Choose the relevant Clawcord tools yourself for timeline, transcript, search, conversation, participant, job, and control queries.",
            "Resolve named rooms, date phrases, and time ranges with tools and available context instead of assuming the current channel is always correct.",
            "If the listed tools are insufficient, you may inspect the local SQLite-backed voice memory directly and explain why.",
            "Shell exec may use /bin/sh; wrap commands in bash -lc only when using bash-specific syntax. jq is installed and useful for inspecting JSON.",
            "Select the workflow and final answer from the available evidence.",
            "",
            "JOB_PACKET_JSON:",
            &compact_packet,
        ]
        .join("\n")
    }

    pub(crate) fn dispatch_openclaw_worker_job(&self, job: &Job) -> Result<WorkerJobMetadata> {
        let job_id = job.id.clone();
        let guild_id = job.guild_id.clone();
        let channel_id = job.voice_channel_id.clone();
        if job_id.is_empty() || guild_id.is_empty() || channel_id.is_empty() {
            anyhow::bail!("worker job is missing job/guild/channel identity");
        }
        let latest = self.timeline_store.get_job(&job_id)?;
        if job_cancel_requested(&latest) {
            anyhow::bail!("worker job was cancelled before the worker process started");
        }
        let worker_env = Self::voice_worker_env();
        let preflight = self.run_worker_preflight(Some(&worker_env));
        if !preflight.ok {
            let mut failed = latest.clone();
            failed.metadata.worker_mut().preflight = Some(preflight.clone());
            failed.metadata.worker_mut().dispatch_error = "worker preflight failed".to_string();
            self.timeline_store.update_job(&failed)?;
            let detail = preflight.failed_check_summary();
            return Err(AgentInfrastructureError::new(format!(
                "worker preflight failed: {detail}"
            ))
            .into());
        }
        let latest = self.timeline_store.get_job(&job_id).unwrap_or(latest);
        let voice_memory_root = env::var("CLAWCORD_VOICE_MEMORY_ROOT")
            .or_else(|_| env::var("VOICE_MEMORY_ROOT"))
            .unwrap_or_else(|_| {
                durable_dir()
                    .join("clawcord")
                    .join("voice")
                    .display()
                    .to_string()
            });
        let packet = json!({
            "job_id": job_id,
            "kind": latest.kind.as_str(),
            "requested_by_user_id": non_empty(
                latest.requested_by_user_id.clone(),
                job.requested_by_user_id.clone()
            ),
            "guild_id": non_empty(latest.guild_id.clone(), guild_id.clone()),
            "voice_channel_id": non_empty(latest.voice_channel_id.clone(), channel_id.clone()),
            "payload": latest.payload_value(),
            "preflight": preflight.to_json(),
            "storage": {
                "voice_memory_root": voice_memory_root.clone(),
                "timeline_store": "SQLite-backed TimelineStore",
                "sqlite_path": format!("{voice_memory_root}/voice.sqlite3"),
            },
            "manuals": {
                "clawcord_worker_tools": "/openclaw/state/workspaces/clanky-voice-worker/TOOLS.md",
            },
            "policy": {
                "may_create_linear_without_confirmation": false,
                "may_publish_to_discord": true,
                "cross_channel_reads_require_explicit_scope_or_context_reason": true,
            },
            "tools": {
                "get_job": format!("clawcord jobs get {job_id}"),
                "status": "clawcord status --guild <guild-id> --channel <room-or-channel>",
                "timeline_tail": "clawcord timeline tail --guild <guild-id> --channel <room-or-channel> --since <relative-time>",
                "timeline_range": "clawcord timeline range --guild <guild-id> --channel <room-or-channel> --from <time> --to <time>",
                "list_conversations": "clawcord conversations list --guild <guild-id> --channel <room-or-channel> --since <relative-time>",
                "resolve_context": "clawcord context resolve --guild <guild-id> --channel <room-or-channel> --reference <natural-language-reference>",
                "render_transcript_range": "clawcord transcripts render --guild <guild-id> --channel <room-or-channel> --from <time> --to <time> --format markdown",
                "search_transcripts": "clawcord transcripts search --guild <guild-id> --channel <room-or-channel> --query <query> --since -7d",
                "participant_trace": "clawcord participants trace --guild <guild-id> --user <user-id> --from <time> --to <time> --include-speech-snippets",
                "search_messages": "clawcord messages search --guild-id <guild-id> --query <query>",
                "read_messages": "clawcord messages read --target <channel-or-thread-id>",
            },
        });
        let packet_guild_id = string_field(&packet, "guild_id");
        let packet_channel_id = string_field(&packet, "voice_channel_id");
        let packet_path = self
            .timeline_store
            .channel_dir(&packet_guild_id, &packet_channel_id)
            .join("jobs")
            .join(format!("{job_id}.packet.json"));
        write_json(&packet_path, &packet)?;
        let message = Self::build_worker_agent_message(&packet_path, &packet);
        let openclaw_bin = env::var("OPENCLAW_BIN").unwrap_or_else(|_| "openclaw".to_string());
        let timeout = voice_worker_agent_timeout_seconds().to_string();
        let output = Command::new(&openclaw_bin)
            .args([
                "agent",
                "--local",
                "--agent",
                "clanky-voice-worker",
                "--session-id",
                &format!("clawcord-worker-{job_id}"),
                "--message",
                &message,
                "--json",
                "--timeout",
                &timeout,
            ])
            .envs(&worker_env)
            .output()?;
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let result_path = self
            .timeline_store
            .channel_dir(&packet_guild_id, &packet_channel_id)
            .join("jobs")
            .join(format!("{job_id}.agent-result.json"));
        if let Some(parent) = result_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&result_path, format!("{}\n", stdout.trim()))?;
        if !output.status.success() {
            let detail = first_non_empty([stderr.trim().to_string(), stdout.trim().to_string()]);
            anyhow::bail!(
                "{}",
                non_empty(
                    detail,
                    format!(
                        "openclaw agent exited {}",
                        output
                            .status
                            .code()
                            .map(|code| code.to_string())
                            .unwrap_or_else(|| "without a status code".to_string())
                    )
                )
            );
        }
        let parsed_result = Self::parse_openclaw_agent_stdout(&stdout);
        let response_text = Self::openclaw_agent_response_text(&stdout);
        let agent_meta_value = parsed_result
            .get("meta")
            .and_then(|meta| meta.get("agentMeta"))
            .unwrap_or(&Value::Null);
        Ok(WorkerJobMetadata {
            packet_path: packet_path.display().to_string(),
            result_path: result_path.display().to_string(),
            dispatch_stdout_preview: preview(&response_text, 1000),
            dispatch_stderr: preview(&stderr, 1000),
            agent: WorkerAgentMetadata::from_json(Some(agent_meta_value))?,
            preflight: Some(preflight),
            response_text,
            command: WORKER_COMMAND_DISPLAY.to_string(),
            ..WorkerJobMetadata::default()
        })
    }

    pub(crate) fn post_worker_job_result(
        &self,
        job: &Job,
        response_text: &str,
    ) -> Result<DiscordPostMetadata> {
        let channel_id = self.control_config.bots_channel_id.clone();
        if channel_id.is_empty() {
            anyhow::bail!("botsChannelId is not configured");
        }
        let requested_by = job.requested_by_user_id.clone();
        let content = if requested_by.is_empty() {
            response_text.trim().to_string()
        } else {
            format!("<@{requested_by}> {}", response_text.trim())
        };
        let mut posted = Vec::new();
        for chunk in split_message_chunks(&content, MESSAGE_CHUNK_LIMIT) {
            let payload = send_message(&channel_id, &chunk)?;
            posted.push(DiscordPostedMessageMetadata {
                channel_id: channel_id.clone(),
                message_id: string_field(&payload, "id"),
            });
        }
        Ok(DiscordPostMetadata {
            channel_id,
            messages: posted,
        })
    }
}
