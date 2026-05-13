use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde_json::{Map, Value, json};

use crate::Result;
use crate::adapters::codex::{CodexRunRequest, codex_response_text};
use crate::config::{durable_dir, non_empty, write_json};
use crate::runtime::agents::{AgentInfrastructureError, AgentRuntime};
use crate::runtime::jobs::{
    BinaryPayload, WorkerAgentMetadata, WorkerJobMetadata, WorkerPreflightCheck,
    WorkerPreflightMetadata,
};
use crate::runtime::timeline::isoformat_z;
use crate::runtime::util::{first_non_empty, preview, worker_agent_timeout_seconds};
use crate::runtime::{Job, JobPayload};

#[derive(Debug, Clone)]
pub(crate) struct WorkerAgentRequest {
    pub job: Job,
    pub job_dir: PathBuf,
}

#[derive(Debug, Clone)]
struct WorkerAgentPacket {
    job_id: String,
    kind: String,
    requested_by_user_id: String,
    guild_id: String,
    voice_channel_id: String,
    payload: JobPayload,
    preflight: WorkerPreflightMetadata,
    storage: WorkerStorage,
    manuals: WorkerManuals,
    policy: WorkerPolicy,
    tools: WorkerTools,
}

#[derive(Debug, Clone)]
struct WorkerStorage {
    voice_memory_root: String,
    sqlite_path: String,
}

#[derive(Debug, Clone)]
struct WorkerManuals {
    clawcord_worker_tools: String,
}

#[derive(Debug, Clone)]
struct WorkerPolicy {
    may_create_linear_without_confirmation: bool,
    may_publish_to_discord: bool,
    cross_channel_reads_require_explicit_scope_or_context_reason: bool,
}

#[derive(Debug, Clone)]
struct WorkerTools {
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

pub(crate) fn dispatch_worker_job(
    agents: &AgentRuntime,
    request: WorkerAgentRequest,
) -> Result<WorkerJobMetadata> {
    let job = request.job;
    validate_worker_job(&job)?;
    if job.cancel_requested() {
        anyhow::bail!("worker job was cancelled before the worker process started");
    }

    let worker_env = worker_agent_env();
    let preflight = run_worker_preflight(Some(&worker_env));
    if !preflight.ok {
        let detail = preflight.failed_check_summary();
        return Err(AgentInfrastructureError::with_preflight(
            format!("worker preflight failed: {detail}"),
            preflight,
        )
        .into());
    }

    fs::create_dir_all(&request.job_dir)?;
    let packet = WorkerAgentPacket::from_job(job.clone(), preflight.clone());
    let packet_path = request.job_dir.join(format!("{}.packet.json", job.id));
    let packet_value = packet.to_json();
    write_json(&packet_path, &packet_value)?;

    let message = build_worker_agent_message(&packet_path, &packet_value);
    let result_path = request.job_dir.join(format!("{}.agent-result.txt", job.id));
    let raw_result_path = request.job_dir.join(format!("{}.codex.jsonl", job.id));
    let session = agents.begin_worker_invocation(&job.guild_id, &job.voice_channel_id, &job.id);
    let codex_result = match agents.codex().run(CodexRunRequest {
        prompt: message,
        session_id: non_empty_option(session.session_id.clone()),
        cwd: worker_cwd(),
        model: worker_model(),
        timeout: Duration::from_secs(worker_agent_timeout_seconds()),
        env: worker_env,
        output_last_message_path: result_path.clone(),
    }) {
        Ok(result) => result,
        Err(error) => {
            agents.fail_invocation(&session.key, error.to_string());
            return Err(AgentInfrastructureError::new(format!(
                "codex worker invocation failed: {error}"
            ))
            .into());
        }
    };
    fs::write(
        &raw_result_path,
        format!("{}\n", codex_result.stdout.trim()),
    )?;
    let completed_session = if codex_result.success {
        agents.complete_invocation(&session.key, codex_result.session_id.clone())
    } else {
        agents.fail_invocation(&session.key, codex_result.stderr.clone())
    };

    if !codex_result.success {
        let detail = first_non_empty([
            codex_result.stderr.trim().to_string(),
            codex_result.stdout.trim().to_string(),
            if codex_result.timed_out {
                "codex worker invocation timed out".to_string()
            } else {
                String::new()
            },
            format!(
                "codex exited {}",
                codex_result
                    .returncode
                    .map(|code| code.to_string())
                    .unwrap_or_else(|| "without a status code".to_string())
            ),
        ]);
        anyhow::bail!("{detail}");
    }

    let response_text = codex_response_text(&codex_result.stdout, &codex_result.final_message);
    Ok(WorkerJobMetadata {
        packet_path: packet_path.display().to_string(),
        result_path: result_path.display().to_string(),
        dispatch_stdout_preview: preview(&response_text, 1000),
        dispatch_stderr: preview(&codex_result.stderr, 1000),
        agent: WorkerAgentMetadata {
            session_id: non_empty(
                completed_session.session_id,
                codex_result.session_id.clone(),
            ),
            provider: "codex".to_string(),
            model: codex_result.model,
            usage: BinaryPayload::empty(),
        },
        preflight: Some(preflight),
        response_text,
        command: codex_result.command_display,
        ..WorkerJobMetadata::default()
    })
}

pub fn build_worker_agent_message(packet_path: &Path, packet: &Value) -> String {
    let compact_packet = serde_json::to_string_pretty(packet).unwrap_or_else(|_| "{}".to_string());
    [
        "You are handling a Clawcord job.",
        "",
        &format!("Job packet path: {}", packet_path.display()),
        "",
        "The full job packet is included below. Use it as the source of truth.",
        "Do not post to Discord yourself; Clawcord will post your final visible answer.",
        "Return only the message that should be posted to agent-chat. Do not wrap it in JSON.",
        "",
        "Preserve the job lane abstraction and only perform side effects authorized by this job.",
        "",
        "Use the job payload as request evidence: requester, room, source events, router context, and raw activated speech.",
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

fn validate_worker_job(job: &Job) -> Result<()> {
    if job.id.trim().is_empty()
        || job.guild_id.trim().is_empty()
        || job.voice_channel_id.trim().is_empty()
    {
        anyhow::bail!("worker job is missing job/guild/channel identity");
    }
    Ok(())
}

fn worker_agent_env() -> BTreeMap<String, String> {
    let mut vars = env::vars().collect::<BTreeMap<_, _>>();
    vars.entry("CLAWCORD_API_BASE_URL".to_string())
        .or_insert_with(|| "http://127.0.0.1:8091".to_string());
    vars
}

fn worker_cwd() -> Option<PathBuf> {
    env::var("CLAWCORD_CODEX_WORKDIR")
        .ok()
        .map(PathBuf::from)
        .or_else(|| env::current_dir().ok())
}

fn worker_model() -> Option<String> {
    env::var("CLAWCORD_WORKER_MODEL")
        .or_else(|_| env::var("CLAWCORD_CODEX_MODEL"))
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn run_worker_preflight(envs: Option<&BTreeMap<String, String>>) -> WorkerPreflightMetadata {
    let worker_env = envs.cloned().unwrap_or_else(worker_agent_env);
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

impl WorkerAgentPacket {
    fn from_job(job: Job, preflight: WorkerPreflightMetadata) -> Self {
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
            storage: WorkerStorage {
                voice_memory_root: voice_memory_root.clone(),
                sqlite_path: format!("{voice_memory_root}/voice.sqlite3"),
            },
            manuals: WorkerManuals {
                clawcord_worker_tools: env::var("CLAWCORD_WORKER_TOOLS_MANUAL").unwrap_or_default(),
            },
            policy: WorkerPolicy {
                may_create_linear_without_confirmation: false,
                may_publish_to_discord: true,
                cross_channel_reads_require_explicit_scope_or_context_reason: true,
            },
            tools: WorkerTools::for_job(&job.id),
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

impl WorkerStorage {
    fn to_json(&self) -> Value {
        json!({
            "voice_memory_root": self.voice_memory_root,
            "timeline_store": "SQLite-backed TimelineStore",
            "sqlite_path": self.sqlite_path,
        })
    }
}

impl WorkerManuals {
    fn to_json(&self) -> Value {
        json!({
            "clawcord_worker_tools": self.clawcord_worker_tools,
        })
    }
}

impl WorkerPolicy {
    fn to_json(&self) -> Value {
        json!({
            "may_create_linear_without_confirmation": self.may_create_linear_without_confirmation,
            "may_publish_to_discord": self.may_publish_to_discord,
            "cross_channel_reads_require_explicit_scope_or_context_reason": self.cross_channel_reads_require_explicit_scope_or_context_reason,
        })
    }
}

impl WorkerTools {
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

fn non_empty_option(value: String) -> Option<String> {
    if value.trim().is_empty() {
        None
    } else {
        Some(value)
    }
}
