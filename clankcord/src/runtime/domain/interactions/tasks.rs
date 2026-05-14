use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde_json::{Map, Value, json};

use crate::Result;
use crate::adapters::codex::codex_response_text;
use crate::adapters::discord::api::send_message;
use crate::config::{
    MESSAGE_CHUNK_LIMIT, durable_dir, non_empty, split_message_chunks, string_field, write_json,
};
use crate::runtime::agents::{AgentInfrastructureError, AgentInvocationRequest, AgentRole};
use crate::runtime::jobs::{
    AgentInvocationMetadata, AgentPreflightCheck, AgentPreflightMetadata, AgentTaskMetadata,
    BinaryPayload, DiscordPostMetadata, DiscordPostedMessageMetadata,
};
use crate::runtime::timeline::isoformat_z;
use crate::runtime::util::{
    agent_task_timeout_seconds, first_non_empty, job_cancel_requested, log, preview,
};
use crate::runtime::{Job, JobKind, JobPayload, JobState, Runtime};

#[derive(Debug, Clone)]
struct AgentTaskPacket {
    job_id: String,
    kind: String,
    requested_by_user_id: String,
    guild_id: String,
    voice_channel_id: String,
    payload: JobPayload,
    preflight: AgentPreflightMetadata,
    storage: AgentTaskStorage,
    manuals: AgentTaskManuals,
    policy: AgentTaskPolicy,
    tools: AgentTaskTools,
}

#[derive(Debug, Clone)]
struct AgentTaskStorage {
    voice_memory_root: String,
    sqlite_path: String,
}

#[derive(Debug, Clone)]
struct AgentTaskManuals {
    tools_manual: String,
}

#[derive(Debug, Clone)]
struct AgentTaskPolicy {
    may_create_linear_without_confirmation: bool,
    may_publish_to_discord: bool,
    cross_channel_reads_require_explicit_scope_or_context_reason: bool,
}

#[derive(Debug, Clone)]
struct AgentTaskTools {
    get_job: String,
    status: String,
    timeline_tail: String,
    timeline_range: String,
    list_conversations: String,
    resolve_context: String,
    render_transcript_range: String,
    search_transcripts: String,
    participant_trace: String,
    search_messages: String,
    read_messages: String,
}

impl Runtime {
    pub fn dispatch_next_due_agent_task_job(&self) -> Result<Value> {
        let Some(job) = self.next_queued_job(JobKind::AgentTask)? else {
            return Ok(json!({"dispatched": false, "reason": "no queued agent task jobs"}));
        };
        let job_id = job.id.clone();
        let attempts = job
            .metadata
            .agent_task()
            .map(|task| task.dispatch_attempts)
            .unwrap_or(0);
        if attempts >= 3 {
            let mut failed = job.clone();
            failed.set_state(JobState::AgentDispatchFailed);
            self.timeline_store.update_job(&failed)?;
            return Ok(
                json!({"dispatched": false, "job": failed.to_value(), "reason": "agent task dispatch attempts exhausted"}),
            );
        }

        let mut running = job.clone();
        running.mark_running();
        self.timeline_store.update_job(&running)?;

        match self.dispatch_agent_task(&running) {
            Ok(dispatch_result) => {
                match self.complete_agent_task_job(job_id.clone(), running.clone(), dispatch_result)
                {
                    Ok(value) => Ok(value),
                    Err(error) => self.fail_agent_task_job(job_id, running, attempts, error),
                }
            }
            Err(error) => self.fail_agent_task_job(job_id, running, attempts, error),
        }
    }

    fn dispatch_agent_task(&self, job: &Job) -> Result<AgentTaskMetadata> {
        let latest = self.timeline_store.get_job(&job.id)?;
        validate_agent_task_job(&latest)?;
        if latest.cancel_requested() {
            anyhow::bail!("agent task was cancelled before the agent process started");
        }

        let agent_env = agent_task_env();
        let preflight = run_agent_task_preflight(Some(&agent_env));
        if !preflight.ok {
            let detail = preflight.failed_check_summary();
            return Err(AgentInfrastructureError::with_preflight(
                format!("agent task preflight failed: {detail}"),
                preflight,
            )
            .into());
        }

        let job_dir = self
            .timeline_store
            .channel_dir(&latest.guild_id, &latest.voice_channel_id)
            .join("jobs");
        fs::create_dir_all(&job_dir)?;
        let packet = AgentTaskPacket::from_job(latest.clone(), preflight.clone());
        let packet_path = job_dir.join(format!("{}.packet.json", latest.id));
        let packet_value = packet.to_json();
        write_json(&packet_path, &packet_value)?;

        let result_path = job_dir.join(format!("{}.agent-result.txt", latest.id));
        let raw_result_path = job_dir.join(format!("{}.codex.jsonl", latest.id));
        let invocation = self.agents.invoke(AgentInvocationRequest {
            role: AgentRole::Task,
            session_key: crate::runtime::AgentRuntime::task_session_key(
                &latest.guild_id,
                &latest.voice_channel_id,
            ),
            job_id: latest.id.clone(),
            guild_id: latest.guild_id.clone(),
            voice_channel_id: latest.voice_channel_id.clone(),
            prompt: build_agent_task_message(&packet_path, &packet_value),
            cwd: agent_task_cwd(),
            model: agent_task_model(),
            timeout: Duration::from_secs(agent_task_timeout_seconds()),
            env: agent_env,
            result_path: result_path.clone(),
            raw_result_path,
        })?;

        if !invocation.success {
            let detail = first_non_empty([
                invocation.stderr.trim().to_string(),
                invocation.stdout.trim().to_string(),
                if invocation.timed_out {
                    "codex agent task invocation timed out".to_string()
                } else {
                    String::new()
                },
                format!(
                    "codex exited {}",
                    invocation
                        .returncode
                        .map(|code| code.to_string())
                        .unwrap_or_else(|| "without a status code".to_string())
                ),
            ]);
            anyhow::bail!("{detail}");
        }

        let response_text = codex_response_text(&invocation.stdout, &invocation.final_message);
        Ok(AgentTaskMetadata {
            packet_path: packet_path.display().to_string(),
            result_path: result_path.display().to_string(),
            dispatch_stdout_preview: preview(&response_text, 1000),
            dispatch_stderr: preview(&invocation.stderr, 1000),
            agent: AgentInvocationMetadata {
                session_id: non_empty(
                    invocation
                        .session
                        .as_ref()
                        .map(|session| session.session_id.clone())
                        .unwrap_or_default(),
                    invocation.session_id,
                ),
                provider: "codex".to_string(),
                model: invocation.model,
                usage: BinaryPayload::empty(),
            },
            preflight: Some(preflight),
            response_text,
            command: invocation.command_display,
            ..AgentTaskMetadata::default()
        })
    }

    fn complete_agent_task_job(
        &self,
        job_id: String,
        fallback_job: Job,
        dispatch_result: AgentTaskMetadata,
    ) -> Result<Value> {
        let mut latest = self
            .timeline_store
            .get_job(&job_id)
            .unwrap_or_else(|_| fallback_job.clone());
        latest.metadata.set_agent_task(dispatch_result);
        if job_cancel_requested(&latest) {
            let cancelled_at = non_empty(
                latest.cancelled_at.clone().unwrap_or_default(),
                isoformat_z(None),
            );
            latest.mark_cancelled();
            latest.cancelled_at = Some(cancelled_at);
            latest.completed_at = Some(isoformat_z(None));
            latest.metadata.agent_task_mut().result_suppressed = true;
            self.timeline_store.update_job(&latest)?;
            self.timeline_store.append_event(
                &latest.guild_id,
                &latest.voice_channel_id,
                json!({
                    "event_kind": "agent_task_result_suppressed",
                    "kind": "agent_task_result_suppressed",
                    "job_id": job_id,
                    "job_kind": latest.kind.as_str(),
                    "reason": "job was cancelled before the agent task result was posted",
                }),
            )?;
            return Ok(json!({"dispatched": true, "job": latest.to_value(), "cancelled": true}));
        }
        let response_text = latest
            .metadata
            .agent_task()
            .map(|task| task.response_text.clone())
            .unwrap_or_default();
        if !response_text.trim().is_empty() {
            let post_result = self.post_agent_task_result(&latest, &response_text)?;
            latest.metadata.agent_task_mut().discord_post = Some(post_result);
        }
        latest.mark_complete();
        self.timeline_store.update_job(&latest)?;
        Ok(json!({"dispatched": true, "job": latest.to_value()}))
    }

    fn fail_agent_task_job(
        &self,
        job_id: String,
        fallback_job: Job,
        attempts: i64,
        error: anyhow::Error,
    ) -> Result<Value> {
        let infrastructure_error = error.downcast_ref::<AgentInfrastructureError>();
        let is_infrastructure_error = infrastructure_error.is_some();
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
            latest.metadata.agent_task_mut().dispatch_error_after_cancel = error_text;
            self.timeline_store.update_job(&latest)?;
            return Ok(json!({"dispatched": false, "job": latest.to_value(), "cancelled": true}));
        }
        if let Some(preflight) = infrastructure_error.and_then(AgentInfrastructureError::preflight)
        {
            latest.metadata.agent_task_mut().preflight = Some(preflight.clone());
        }
        let next_attempts = attempts + 1;
        latest.metadata.agent_task_mut().dispatch_attempts = if is_infrastructure_error {
            next_attempts.max(3)
        } else {
            next_attempts
        };
        latest.metadata.agent_task_mut().dispatch_error = error_text.clone();
        latest.set_state(if is_infrastructure_error || next_attempts >= 3 {
            JobState::AgentDispatchFailed
        } else {
            JobState::Queued
        });
        self.timeline_store.update_job(&latest)?;
        log(&format!(
            "agent task dispatch failed for {job_id}: {error_text}"
        ));
        Ok(json!({"dispatched": false, "job": latest.to_value(), "error": error_text}))
    }

    pub(crate) fn post_agent_task_result(
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

pub fn build_agent_task_message(packet_path: &Path, packet: &Value) -> String {
    let compact_packet = serde_json::to_string_pretty(packet).unwrap_or_else(|_| "{}".to_string());
    [
        "You are handling a Clawcord agent task.",
        "",
        &format!("Job packet path: {}", packet_path.display()),
        "",
        "The full job packet is included below. Use it as the source of truth.",
        "Do not post to Discord yourself; Clawcord will post your final visible answer.",
        "Return only the message that should be posted to agent-chat. Do not wrap it in JSON.",
        "",
        "Preserve the job lane abstraction and only perform side effects authorized by this job.",
        "",
        "Use the job payload as request evidence: requester, room, source events, wake activation context, and raw activated speech.",
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

fn validate_agent_task_job(job: &Job) -> Result<()> {
    if job.id.trim().is_empty()
        || job.guild_id.trim().is_empty()
        || job.voice_channel_id.trim().is_empty()
    {
        anyhow::bail!("agent task job is missing job/guild/channel identity");
    }
    Ok(())
}

fn agent_task_env() -> BTreeMap<String, String> {
    let mut vars = env::vars().collect::<BTreeMap<_, _>>();
    vars.entry("CLAWCORD_API_BASE_URL".to_string())
        .or_insert_with(|| "http://127.0.0.1:8091".to_string());
    vars
}

fn agent_task_cwd() -> Option<PathBuf> {
    env::var("CLAWCORD_CODEX_WORKDIR")
        .ok()
        .map(PathBuf::from)
        .or_else(|| env::current_dir().ok())
}

fn agent_task_model() -> Option<String> {
    env::var("CLAWCORD_AGENT_TASK_MODEL")
        .or_else(|_| env::var("CLAWCORD_CODEX_MODEL"))
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn run_agent_task_preflight(envs: Option<&BTreeMap<String, String>>) -> AgentPreflightMetadata {
    let agent_env = envs.cloned().unwrap_or_else(agent_task_env);
    let codex_bin = env::var("CLAWCORD_CODEX_BIN")
        .or_else(|_| env::var("CODEX_BIN"))
        .unwrap_or_else(|_| "codex".to_string());
    let checks: Vec<Vec<String>> = vec![
        vec![codex_bin, "--version".to_string()],
        vec!["jq".to_string(), "--version".to_string()],
        vec!["sqlite3".to_string(), "--version".to_string()],
        vec![
            "clawcord".to_string(),
            "transcripts".to_string(),
            "render".to_string(),
            "--help".to_string(),
        ],
        vec![
            "clawcord".to_string(),
            "transcripts".to_string(),
            "search".to_string(),
            "--help".to_string(),
        ],
        vec![
            "clawcord".to_string(),
            "timeline".to_string(),
            "range".to_string(),
            "--help".to_string(),
        ],
        vec![
            "clawcord".to_string(),
            "conversations".to_string(),
            "list".to_string(),
            "--help".to_string(),
        ],
        vec![
            "clawcord".to_string(),
            "context".to_string(),
            "resolve".to_string(),
            "--help".to_string(),
        ],
        vec![
            "clawcord".to_string(),
            "participants".to_string(),
            "trace".to_string(),
            "--help".to_string(),
        ],
        vec![
            "clawcord".to_string(),
            "jobs".to_string(),
            "get".to_string(),
            "--help".to_string(),
        ],
    ];
    let mut results = Vec::new();
    for command in checks {
        let display = command.join(" ");
        match Command::new(&command[0])
            .args(&command[1..])
            .envs(&agent_env)
            .output()
        {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                results.push(AgentPreflightCheck {
                    command: display,
                    returncode: output.status.code(),
                    ok: output.status.success(),
                    stdout_preview: preview(&stdout, 500),
                    stderr_preview: preview(&stderr, 500),
                    error: String::new(),
                });
            }
            Err(error) => {
                results.push(AgentPreflightCheck {
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
    AgentPreflightMetadata {
        ok: results.iter().all(|result| result.ok),
        checked_at: isoformat_z(None),
        checks: results,
    }
}

impl AgentTaskPacket {
    fn from_job(job: Job, preflight: AgentPreflightMetadata) -> Self {
        let voice_memory_root = env::var("CLAWCORD_VOICE_MEMORY_ROOT")
            .or_else(|_| env::var("VOICE_MEMORY_ROOT"))
            .unwrap_or_else(|_| {
                durable_dir()
                    .join("clawcord")
                    .join("voice")
                    .display()
                    .to_string()
            });
        Self {
            job_id: job.id.clone(),
            kind: job.kind.as_str().to_string(),
            requested_by_user_id: job.requested_by_user_id.clone(),
            guild_id: job.guild_id.clone(),
            voice_channel_id: job.voice_channel_id.clone(),
            payload: job.payload,
            preflight,
            storage: AgentTaskStorage {
                voice_memory_root: voice_memory_root.clone(),
                sqlite_path: format!("{voice_memory_root}/voice.sqlite3"),
            },
            manuals: AgentTaskManuals {
                tools_manual: env::var("CLAWCORD_AGENT_TASK_TOOLS_MANUAL").unwrap_or_default(),
            },
            policy: AgentTaskPolicy {
                may_create_linear_without_confirmation: false,
                may_publish_to_discord: true,
                cross_channel_reads_require_explicit_scope_or_context_reason: true,
            },
            tools: AgentTaskTools::for_job(&job.id),
        }
    }

    fn to_json(&self) -> Value {
        json!({
            "job_id": self.job_id,
            "kind": self.kind,
            "requested_by_user_id": self.requested_by_user_id,
            "guild_id": self.guild_id,
            "voice_channel_id": self.voice_channel_id,
            "payload": self.payload.to_json(),
            "preflight": self.preflight.to_json(),
            "storage": self.storage.to_json(),
            "manuals": self.manuals.to_json(),
            "policy": self.policy.to_json(),
            "tools": self.tools.to_json(),
        })
    }
}

impl AgentTaskStorage {
    fn to_json(&self) -> Value {
        json!({
            "voice_memory_root": self.voice_memory_root,
            "timeline_store": "SQLite-backed TimelineStore",
            "sqlite_path": self.sqlite_path,
        })
    }
}

impl AgentTaskManuals {
    fn to_json(&self) -> Value {
        json!({
            "tools_manual": self.tools_manual,
        })
    }
}

impl AgentTaskPolicy {
    fn to_json(&self) -> Value {
        json!({
            "may_create_linear_without_confirmation": self.may_create_linear_without_confirmation,
            "may_publish_to_discord": self.may_publish_to_discord,
            "cross_channel_reads_require_explicit_scope_or_context_reason": self.cross_channel_reads_require_explicit_scope_or_context_reason,
        })
    }
}

impl AgentTaskTools {
    fn for_job(job_id: &str) -> Self {
        Self {
            get_job: format!("clawcord jobs get {job_id}"),
            status: "clawcord status --guild <guild-id> --channel <room-or-channel>".to_string(),
            timeline_tail:
                "clawcord timeline tail --guild <guild-id> --channel <room-or-channel> --since <relative-time>"
                    .to_string(),
            timeline_range:
                "clawcord timeline range --guild <guild-id> --channel <room-or-channel> --from <time> --to <time>"
                    .to_string(),
            list_conversations:
                "clawcord conversations list --guild <guild-id> --channel <room-or-channel> --since <relative-time>"
                    .to_string(),
            resolve_context:
                "clawcord context resolve --guild <guild-id> --channel <room-or-channel> --reference <natural-language-reference>"
                    .to_string(),
            render_transcript_range:
                "clawcord transcripts render --guild <guild-id> --channel <room-or-channel> --from <time> --to <time> --format markdown"
                    .to_string(),
            search_transcripts:
                "clawcord transcripts search --guild <guild-id> --channel <room-or-channel> --query <query> --since -7d"
                    .to_string(),
            participant_trace:
                "clawcord participants trace --guild <guild-id> --user <user-id> --from <time> --to <time> --include-speech-snippets"
                    .to_string(),
            search_messages: "clawcord messages search --guild-id <guild-id> --query <query>"
                .to_string(),
            read_messages: "clawcord messages read --target <channel-or-thread-id>".to_string(),
        }
    }

    fn to_json(&self) -> Value {
        let mut object = Map::new();
        object.insert("get_job".to_string(), json!(self.get_job));
        object.insert("status".to_string(), json!(self.status));
        object.insert("timeline_tail".to_string(), json!(self.timeline_tail));
        object.insert("timeline_range".to_string(), json!(self.timeline_range));
        object.insert(
            "list_conversations".to_string(),
            json!(self.list_conversations),
        );
        object.insert("resolve_context".to_string(), json!(self.resolve_context));
        object.insert(
            "render_transcript_range".to_string(),
            json!(self.render_transcript_range),
        );
        object.insert(
            "search_transcripts".to_string(),
            json!(self.search_transcripts),
        );
        object.insert(
            "participant_trace".to_string(),
            json!(self.participant_trace),
        );
        object.insert("search_messages".to_string(), json!(self.search_messages));
        object.insert("read_messages".to_string(), json!(self.read_messages));
        Value::Object(object)
    }
}
