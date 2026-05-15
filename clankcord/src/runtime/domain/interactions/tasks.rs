use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde_json::{Map, Value, json};

use crate::Result;
use crate::adapters::codex::{codex_response_text, extract_codex_usage};
use crate::config::{non_empty, write_json};
use crate::runtime::agents::{
    AgentInfrastructureError, AgentInvocationRequest, AgentRole, AgentRuntime,
};
use crate::runtime::jobs::{
    AgentInvocationMetadata, AgentPreflightCheck, AgentPreflightMetadata, AgentTaskMetadata,
    BinaryPayload,
};
use crate::runtime::timeline::{event_text, first_value_string, isoformat_z};
use crate::runtime::util::{
    agent_task_timeout_seconds, first_non_empty, job_cancel_requested, log, preview,
};
use crate::runtime::{CommandArguments, Job, JobKind, JobState, Runtime};

#[derive(Debug, Clone)]
struct AgentTaskPacket {
    schema: String,
    job_id: String,
    kind: String,
    root_job_id: String,
    parent_job_id: String,
    requested_by_user_id: String,
    guild_id: String,
    voice_channel_id: String,
    request: Value,
    manuals: AgentTaskManuals,
    policy: AgentTaskPolicy,
    tools: AgentTaskTools,
}

#[derive(Debug, Clone)]
struct AgentTaskManuals {
    automation_spec: String,
    automation_spec_path: String,
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
    submit_response: String,
    automation_spec: String,
    validate_automation: String,
    create_automation: String,
}

impl Runtime {
    pub(crate) async fn dispatch_claimed_agent_task_job(&self, job: Job) -> Result<Value> {
        let job_id = job.id.clone();
        let attempts = job
            .metadata
            .agent_task()
            .map(|task| task.dispatch_attempts)
            .unwrap_or(0);
        if attempts >= 3 {
            let mut failed = job.clone();
            failed.set_state(JobState::AgentDispatchFailed);
            self.timeline_store.update_job(&failed).await?;
            return Ok(
                json!({"dispatched": false, "job": failed.to_value(), "reason": "agent task dispatch attempts exhausted"}),
            );
        }

        match self.dispatch_agent_task(&job).await {
            Ok(dispatch_result) => {
                match self
                    .complete_agent_task_job(job_id.clone(), dispatch_result)
                    .await
                {
                    Ok(value) => Ok(value),
                    Err(error) => self.fail_agent_task_job(job_id, attempts, error).await,
                }
            }
            Err(error) => self.fail_agent_task_job(job_id, attempts, error).await,
        }
    }

    async fn dispatch_agent_task(&self, job: &Job) -> Result<AgentTaskMetadata> {
        let latest = self.timeline_store.get_job(&job.id).await?;
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
        let packet = AgentTaskPacket::from_job(&latest);
        let packet_path = job_dir.join(format!("{}.packet.json", latest.id));
        let packet_value = packet.to_json();
        write_json(&packet_path, &packet_value)?;

        let prompt_path = job_dir.join(format!("{}.agent-prompt.txt", latest.id));
        let result_path = job_dir.join(format!("{}.agent-result.txt", latest.id));
        let raw_result_path = job_dir.join(format!("{}.codex.jsonl", latest.id));
        let session_key =
            AgentRuntime::task_session_key(&latest.guild_id, &latest.voice_channel_id);
        let prior_session_id = non_empty(
            latest
                .metadata
                .agent_task()
                .map(|task| task.agent.session_id.clone())
                .unwrap_or_default(),
            self.latest_agent_session_id(&latest).await?,
        );
        let include_master_prompt = prior_session_id.trim().is_empty();
        let prompt = build_agent_task_message_for_session(
            &packet_path,
            &packet_value,
            include_master_prompt,
        );
        fs::write(&prompt_path, &prompt)?;
        let mut prepared = latest.clone();
        let mut task_metadata = prepared
            .metadata
            .agent_task()
            .cloned()
            .unwrap_or_else(AgentTaskMetadata::default);
        task_metadata.packet_path = packet_path.display().to_string();
        task_metadata.prompt_path = prompt_path.display().to_string();
        task_metadata.result_path = result_path.display().to_string();
        task_metadata.raw_result_path = raw_result_path.display().to_string();
        task_metadata.preflight = Some(preflight.clone());
        prepared.metadata.set_agent_task(task_metadata);
        self.timeline_store.update_job(&prepared).await?;
        let invocation = self.agents.invoke(AgentInvocationRequest {
            role: AgentRole::Task,
            session_key,
            job_id: latest.id.clone(),
            guild_id: latest.guild_id.clone(),
            voice_channel_id: latest.voice_channel_id.clone(),
            prior_session_id,
            prompt,
            cwd: agent_task_cwd(),
            model: agent_task_model(),
            timeout: Duration::from_secs(agent_task_timeout_seconds()),
            env: agent_env,
            result_path: result_path.clone(),
            raw_result_path: raw_result_path.clone(),
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
            prompt_path: prompt_path.display().to_string(),
            result_path: result_path.display().to_string(),
            raw_result_path: raw_result_path.display().to_string(),
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
                usage: BinaryPayload::from_json(&extract_codex_usage(&invocation.stdout))
                    .unwrap_or_else(|_| BinaryPayload::empty()),
            },
            preflight: Some(preflight),
            response_text,
            command: invocation.command_display,
            ..AgentTaskMetadata::default()
        })
    }

    async fn complete_agent_task_job(
        &self,
        job_id: String,
        dispatch_result: AgentTaskMetadata,
    ) -> Result<Value> {
        let mut latest = self.timeline_store.get_job(&job_id).await?;
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
            self.timeline_store.update_job(&latest).await?;
            self.timeline_store
                .append_event(
                    &latest.guild_id,
                    &latest.voice_channel_id,
                    json!({
                        "event_kind": "agent_task_result_suppressed",
                        "kind": "agent_task_result_suppressed",
                        "job_id": job_id,
                        "job_kind": latest.kind.as_str(),
                        "reason": "job was cancelled before the agent task result was posted",
                    }),
                )
                .await?;
            return Ok(json!({"dispatched": true, "job": latest.to_value(), "cancelled": true}));
        }
        let submitted_responses = self.response_jobs_for_source(&latest.id).await?;
        if !submitted_responses.is_empty() {
            latest.mark_complete();
            self.timeline_store.update_job(&latest).await?;
            return Ok(json!({
                "dispatched": true,
                "job": latest.to_value(),
                "submitted_responses": submitted_responses.into_iter().map(|job| job.to_value()).collect::<Vec<_>>(),
            }));
        }
        let response_text = latest
            .metadata
            .agent_task()
            .map(|task| task.response_text.clone())
            .unwrap_or_default();
        let response_text = response_text.trim();
        if response_text == "RESPONSE_SUBMITTED" {
            anyhow::bail!(
                "agent task reported RESPONSE_SUBMITTED but no response job exists for source job {job_id}"
            );
        }
        if response_text.is_empty() {
            anyhow::bail!("agent task completed without submitting a response job");
        }
        anyhow::bail!(
            "agent task returned final text instead of submitting a response job through `clankcord responses submit`"
        )
    }

    async fn response_jobs_for_source(&self, source_job_id: &str) -> Result<Vec<Job>> {
        self.timeline_store
            .list_response_jobs_for_source(source_job_id)
            .await
    }

    async fn latest_agent_session_id(&self, job: &Job) -> Result<String> {
        let mut jobs = self
            .timeline_store
            .list_jobs_by_scope_kind(&job.guild_id, &job.voice_channel_id, JobKind::AgentTask)
            .await?;
        jobs.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.id.cmp(&right.id))
        });
        Ok(jobs
            .into_iter()
            .rev()
            .filter(|candidate| candidate.id != job.id)
            .filter_map(|candidate| {
                candidate
                    .metadata
                    .agent_task()
                    .map(|task| task.agent.session_id.clone())
            })
            .find(|session_id| !session_id.trim().is_empty())
            .unwrap_or_default())
    }

    async fn fail_agent_task_job(
        &self,
        job_id: String,
        attempts: i64,
        error: anyhow::Error,
    ) -> Result<Value> {
        let infrastructure_error = error.downcast_ref::<AgentInfrastructureError>();
        let is_infrastructure_error = infrastructure_error.is_some();
        let error_text = error.to_string();
        let mut latest = self.timeline_store.get_job(&job_id).await?;
        if job_cancel_requested(&latest) {
            let cancelled_at = non_empty(
                latest.cancelled_at.clone().unwrap_or_default(),
                isoformat_z(None),
            );
            latest.mark_cancelled();
            latest.cancelled_at = Some(cancelled_at);
            latest.metadata.agent_task_mut().dispatch_error_after_cancel = error_text;
            self.timeline_store.update_job(&latest).await?;
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
        self.timeline_store.update_job(&latest).await?;
        log(&format!(
            "agent task dispatch failed for {job_id}: {error_text}"
        ));
        Ok(json!({"dispatched": false, "job": latest.to_value(), "error": error_text}))
    }
}

pub fn build_agent_task_message(packet_path: &Path, packet: &Value) -> String {
    build_agent_task_message_for_session(packet_path, packet, true)
}

pub fn build_agent_task_message_for_session(
    packet_path: &Path,
    packet: &Value,
    include_master_prompt: bool,
) -> String {
    let compact_packet = serde_json::to_string_pretty(packet).unwrap_or_else(|_| "{}".to_string());
    let mut sections = Vec::new();
    if include_master_prompt {
        sections.push(agent_master_prompt());
    }
    sections.push([
        "JOB_CONTEXT:",
        "This is a Clankcord agent-task job for the active Discord channel session.",
        &format!("Job packet path: {}", packet_path.display()),
        "A compact job packet is included below. Use it as the starting point, then query Clankcord when you need more timeline, transcript, room, job, or participant context.",
        "Do not post to Discord yourself. Submit visible answers through `clankcord responses submit --job <job-id> --sink agent-chat --stdin`.",
        "You must use that response submission path for visible answers. After a successful submission, return only RESPONSE_SUBMITTED as your final message. Do not use final text as a publication channel.",
        "Preserve the job lane abstraction and only perform side effects authorized by this job.",
        "Use the compact request as evidence for requester, room, source events, wake activation context, and activated speech. Fetch raw details with the listed CLI tools only when they are relevant.",
        "Choose the relevant Clankcord tools yourself for timeline, transcript, search, conversation, participant, job, and control queries.",
        "Resolve named rooms, date phrases, and time ranges with tools and available context instead of assuming the current channel is always correct.",
        "If the listed tools are insufficient, you may inspect the local Postgres-backed voice memory directly and explain why.",
        "Shell exec may use /bin/sh; wrap commands in bash -lc only when using bash-specific syntax. jq and rg are installed and useful for inspecting JSON and searching text.",
        "Select the workflow and final answer from the available evidence.",
        "",
        "JOB_PACKET_JSON:",
        &compact_packet,
    ]
    .join("\n"));
    sections.join("\n\n")
}

pub fn agent_master_prompt() -> String {
    [
        "SESSION_INSTRUCTIONS:",
        "You are Clanky, a helpful and rigorous Discord server assistant for the people using this server, especially participants in voice rooms.",
        "Your job is to help them understand, remember, research, coordinate, and act on conversations.",
        "You can answer questions, inspect prior discussion, fact-check claims, research outside information, set reminders, create automations, ask clarifying questions, and report useful results back to Discord through Clankcord.",
        "",
        "Clankcord is the local system that connects you to Discord. It captures voice, turns speech into transcript events, stores those events in a Postgres-backed timeline, manages runtime jobs and automations, stores transcript artifacts, and publishes responses.",
        "The timeline is the authoritative memory of what happened in the server: who spoke, what was said, what jobs ran, what automations fired, and what was published.",
        "Use Clankcord tools to inspect that memory instead of guessing from the user's latest sentence alone.",
        "Clankcord voice bots such as clanky-vc1 and clanky-vc2 capture audio; they are not you.",
        "",
        "Use the `clankcord` CLI commands to inspect timeline history, render transcript windows, resolve participants, inspect room state, register automations, ask clarifying questions, and submit user-visible responses.",
        "The CLI is the supported way to ask Clankcord to do work. Do not post to Discord directly. Do not mutate Clankcord state by editing files or databases directly.",
        "",
        "When a user asks for immediate information, gather enough context to answer well. Use timeline, transcript, participant, room, message, and external research tools as needed.",
        "Submit visible answers through `clankcord responses submit --job <job-id> --sink agent-chat --stdin`. After a successful submission, return only RESPONSE_SUBMITTED as your final message. Final text is not a publication path.",
        "",
        "You may search the web and should use web research when it would materially improve the answer, especially for current facts, unfamiliar topics, fact-checking, product or technical details, or anything where the transcript alone is not enough.",
        "Do not invent facts when research is possible.",
        "",
        "When a user asks for runtime work such as transcript creation, room control, sound playback, reminders, or publication, use the corresponding `clankcord` command.",
        "When a user asks for future, conditional, or recurring behavior, read `clankcord automations spec`, validate with `clankcord automations validate --stdin`, then register with `clankcord automations create --stdin`. Automations default to one shot unless the user clearly asks for recurring behavior. Give automations reasonable expiries. Resolve named people to Discord user IDs before storing durable conditions whenever possible.",
        "When the request is underspecified, ask a focused clarifying question through Clankcord. Keep the ongoing channel context in mind after the user answers.",
        "",
        "Be useful, complete, and intellectually honest. Do not choose a weak answer merely because it is shorter.",
        "Do not be sycophantic. If a user asks for your view on something said in a transcript, do not just repeat the transcript back to them.",
        "Analyze it, check the assumptions, identify what matters, and say something genuinely useful.",
        "If your first answer would be obvious, shallow, or uninteresting, work harder: inspect more context, research where helpful, compare alternatives, and produce the strongest answer you can within the job's authority boundaries.",
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
    vars.entry("CLANKCORD_API_BASE_URL".to_string())
        .or_insert_with(|| "http://127.0.0.1:8091".to_string());
    vars
}

fn agent_task_cwd() -> Option<PathBuf> {
    env::var("CLANKCORD_CODEX_WORKDIR")
        .ok()
        .map(PathBuf::from)
        .or_else(|| env::current_dir().ok())
}

fn agent_task_model() -> Option<String> {
    env::var("CLANKCORD_AGENT_TASK_MODEL")
        .or_else(|_| env::var("CLANKCORD_CODEX_MODEL"))
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn run_agent_task_preflight(envs: Option<&BTreeMap<String, String>>) -> AgentPreflightMetadata {
    let agent_env = envs.cloned().unwrap_or_else(agent_task_env);
    let codex_bin = env::var("CLANKCORD_CODEX_BIN")
        .or_else(|_| env::var("CODEX_BIN"))
        .unwrap_or_else(|_| "codex".to_string());
    let checks: Vec<Vec<String>> = vec![
        vec![codex_bin, "--version".to_string()],
        vec!["rg".to_string(), "--version".to_string()],
        vec!["jq".to_string(), "--version".to_string()],
        vec!["psql".to_string(), "--version".to_string()],
        vec![
            "clankcord".to_string(),
            "transcripts".to_string(),
            "render".to_string(),
            "--help".to_string(),
        ],
        vec![
            "clankcord".to_string(),
            "transcripts".to_string(),
            "search".to_string(),
            "--help".to_string(),
        ],
        vec![
            "clankcord".to_string(),
            "timeline".to_string(),
            "range".to_string(),
            "--help".to_string(),
        ],
        vec![
            "clankcord".to_string(),
            "conversations".to_string(),
            "list".to_string(),
            "--help".to_string(),
        ],
        vec![
            "clankcord".to_string(),
            "context".to_string(),
            "resolve".to_string(),
            "--help".to_string(),
        ],
        vec![
            "clankcord".to_string(),
            "participants".to_string(),
            "trace".to_string(),
            "--help".to_string(),
        ],
        vec![
            "clankcord".to_string(),
            "jobs".to_string(),
            "get".to_string(),
            "--help".to_string(),
        ],
        vec![
            "clankcord".to_string(),
            "responses".to_string(),
            "submit".to_string(),
            "--help".to_string(),
        ],
        vec![
            "clankcord".to_string(),
            "automations".to_string(),
            "spec".to_string(),
        ],
        vec![
            "clankcord".to_string(),
            "automations".to_string(),
            "create".to_string(),
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
    fn from_job(job: &Job) -> Self {
        Self {
            schema: "clankcord.agent_task.v0".to_string(),
            job_id: job.id.clone(),
            kind: job.kind.as_str().to_string(),
            root_job_id: job.root_job_id.clone(),
            parent_job_id: job.parent_job_id.clone().unwrap_or_default(),
            requested_by_user_id: job.requested_by_user_id.clone(),
            guild_id: job.guild_id.clone(),
            voice_channel_id: job.voice_channel_id.clone(),
            request: compact_request_context(job),
            manuals: AgentTaskManuals {
                automation_spec: "clankcord automations spec".to_string(),
                automation_spec_path: "clankcord/docs/AUTOMATION_SPEC.md".to_string(),
            },
            policy: AgentTaskPolicy {
                may_create_linear_without_confirmation: false,
                may_publish_to_discord: true,
                cross_channel_reads_require_explicit_scope_or_context_reason: true,
            },
            tools: AgentTaskTools::for_job(job),
        }
    }

    fn to_json(&self) -> Value {
        json!({
            "schema": self.schema,
            "job_id": self.job_id,
            "kind": self.kind,
            "root_job_id": self.root_job_id,
            "parent_job_id": self.parent_job_id,
            "requested_by_user_id": self.requested_by_user_id,
            "guild_id": self.guild_id,
            "voice_channel_id": self.voice_channel_id,
            "request": self.request,
            "manuals": self.manuals.to_json(),
            "policy": self.policy.to_json(),
            "tools": self.tools.to_json(),
        })
    }
}

impl AgentTaskManuals {
    fn to_json(&self) -> Value {
        json!({
            "automation_spec": self.automation_spec,
            "automation_spec_path": self.automation_spec_path,
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

fn compact_request_context(job: &Job) -> Value {
    let Some(command) = job.command() else {
        return json!({});
    };
    let arguments = compact_command_arguments(&command.arguments);
    let raw_arguments = command.arguments.to_json();
    let mut object = Map::new();
    insert_string(&mut object, "command_kind", command.command_kind.as_str());
    insert_string(&mut object, "action", command.action.as_str());
    insert_string(&mut object, "text", &command.arguments.request_text());
    insert_string(
        &mut object,
        "requested_by_user_id",
        &command.requested_by_user_id,
    );
    insert_string(
        &mut object,
        "requested_by_speaker_label",
        &command.requested_by_speaker_label,
    );
    insert_string(&mut object, "target_room_id", &command.target_room_id);
    insert_string(
        &mut object,
        "target_voice_channel_id",
        &command.target_voice_channel_id,
    );
    insert_string(&mut object, "target_job_id", &command.target_job_id);
    if !command.target_job_ids.is_empty() {
        object.insert("target_job_ids".to_string(), json!(command.target_job_ids));
    }
    if command.requires_confirmation {
        object.insert("requires_confirmation".to_string(), Value::Bool(true));
    }
    if !arguments.as_object().is_none_or(Map::is_empty) {
        object.insert("arguments".to_string(), arguments);
    }
    let source_event_ids = string_array_field(&raw_arguments, "source_event_ids");
    if !source_event_ids.is_empty() {
        object.insert("source_event_ids".to_string(), json!(source_event_ids));
    }
    if let Some(activation) = raw_arguments.get("activation") {
        let activation = compact_activation_context(activation);
        if !activation.as_object().is_none_or(Map::is_empty) {
            object.insert("activation".to_string(), activation);
        }
    }
    Value::Object(object)
}

fn compact_command_arguments(arguments: &CommandArguments) -> Value {
    let mut object = Map::new();
    insert_string(&mut object, "query", &arguments.query);
    insert_string(&mut object, "question", &arguments.question);
    insert_string(&mut object, "request", &arguments.request);
    insert_string(&mut object, "instruction_text", &arguments.instruction_text);
    insert_string(&mut object, "relative_start", &arguments.relative_start);
    insert_string(&mut object, "window_id", &arguments.window_id);
    insert_string(&mut object, "from", &arguments.from);
    insert_string(&mut object, "to", &arguments.to);
    insert_string(&mut object, "room", &arguments.room);
    insert_string(&mut object, "channel", &arguments.channel);
    insert_string(&mut object, "target_room", &arguments.target_room);
    insert_string(&mut object, "target_channel", &arguments.target_channel);
    insert_string(&mut object, "publish", &arguments.publish);
    if let Some(refine) = arguments.refine {
        object.insert("refine".to_string(), Value::Bool(refine));
    }
    if let Some(duration_seconds) = arguments.duration_seconds {
        object.insert("duration_seconds".to_string(), json!(duration_seconds));
    }
    if let Some(unpublished_only) = arguments.unpublished_only {
        object.insert(
            "unpublished_only".to_string(),
            Value::Bool(unpublished_only),
        );
    }
    Value::Object(object)
}

fn compact_activation_context(value: &Value) -> Value {
    let mut object = Map::new();
    for key in ["activation_id", "wake_event_id", "latest_wake_event_id"] {
        insert_string(&mut object, key, &first_value_string(value, &[key]));
    }
    let amended_wake_event_ids = string_array_field(value, "amended_wake_event_ids");
    if !amended_wake_event_ids.is_empty() {
        object.insert(
            "amended_wake_event_ids".to_string(),
            json!(amended_wake_event_ids),
        );
    }
    let source_event_ids = string_array_field(value, "source_event_ids");
    if !source_event_ids.is_empty() {
        object.insert("source_event_ids".to_string(), json!(source_event_ids));
    }
    if let Some(room) = value.get("room_snapshot") {
        let room = compact_room_snapshot(room);
        if !room.as_object().is_none_or(Map::is_empty) {
            object.insert("room".to_string(), room);
        }
    }
    if let Some(events) = value.get("prior_to_activation").and_then(Value::as_array) {
        object.insert(
            "prior_to_activation".to_string(),
            Value::Array(events.iter().take(8).map(compact_timeline_event).collect()),
        );
    }
    if let Some(events) = value.get("post_activation_turn").and_then(Value::as_array) {
        object.insert(
            "post_activation_turn".to_string(),
            Value::Array(events.iter().take(12).map(compact_timeline_event).collect()),
        );
    }
    Value::Object(object)
}

fn compact_room_snapshot(value: &Value) -> Value {
    let mut object = Map::new();
    for (output, keys) in [
        ("room_id", &["room_id", "roomId"][..]),
        ("guild_id", &["guild_id", "guildId"][..]),
        ("voice_channel_id", &["voice_channel_id", "channelId"][..]),
        (
            "voice_channel_name",
            &["voice_channel_name", "channelName"][..],
        ),
        ("channel_slug", &["channel_slug", "channelSlug"][..]),
        (
            "active_session_id",
            &["active_session_id", "activeSessionId"][..],
        ),
    ] {
        insert_string(&mut object, output, &first_value_string(value, keys));
    }
    if let Some(occupancy) = value.get("occupancy") {
        let mut compact = Map::new();
        for (output, keys) in [
            (
                "effective_human_count",
                &["effective_human_count", "effectiveHumanCount"][..],
            ),
            ("human_count", &["human_count", "humanCount"][..]),
            ("last_speech_at", &["last_speech_at", "lastSpeechAt"][..]),
        ] {
            insert_value_string(&mut compact, output, occupancy, keys);
        }
        if !compact.is_empty() {
            object.insert("occupancy".to_string(), Value::Object(compact));
        }
    }
    Value::Object(object)
}

fn compact_timeline_event(event: &Value) -> Value {
    let mut object = Map::new();
    for (output, keys) in [
        ("event_id", &["event_id", "eventId"][..]),
        ("kind", &["kind", "event_kind"][..]),
        ("started_at", &["started_at", "startedAt", "timestamp"][..]),
        ("ended_at", &["ended_at", "endedAt"][..]),
        (
            "speaker_user_id",
            &["speaker_user_id", "speakerId", "user_id"][..],
        ),
        ("speaker_label", &["speaker_label", "speakerLabel"][..]),
        ("voice_channel_id", &["voice_channel_id", "channelId"][..]),
    ] {
        insert_string(&mut object, output, &first_value_string(event, keys));
    }
    insert_string(&mut object, "text", &event_text(event));
    if let Some(wake) = event.get("wake") {
        let mut compact = Map::new();
        for (output, keys) in [
            ("wake", &["wake"][..]),
            ("score", &["score"][..]),
            ("threshold", &["threshold"][..]),
            ("model_label", &["model_label", "modelLabel"][..]),
        ] {
            insert_value_string(&mut compact, output, wake, keys);
        }
        if !compact.is_empty() {
            object.insert("wake".to_string(), Value::Object(compact));
        }
    }
    Value::Object(object)
}

fn insert_string(object: &mut Map<String, Value>, key: &str, value: &str) {
    if !value.trim().is_empty() {
        object.insert(key.to_string(), Value::String(value.to_string()));
    }
}

fn insert_value_string(
    object: &mut Map<String, Value>,
    output: &str,
    source: &Value,
    keys: &[&str],
) {
    let value = first_value_string(source, keys);
    if !value.trim().is_empty() {
        object.insert(output.to_string(), Value::String(value));
    }
}

fn string_array_field(value: &Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

impl AgentTaskTools {
    fn for_job(job: &Job) -> Self {
        let guild_id = &job.guild_id;
        let voice_channel_id = &job.voice_channel_id;
        Self {
            get_job: format!("clankcord jobs get {}", job.id),
            status: format!("clankcord status --guild {guild_id} --channel {voice_channel_id}"),
            timeline_tail: format!(
                "clankcord timeline tail --guild {guild_id} --channel {voice_channel_id} --since -1h"
            ),
            timeline_range: format!(
                "clankcord timeline range --guild {guild_id} --channel {voice_channel_id} --from <time> --to <time>"
            ),
            list_conversations: format!(
                "clankcord conversations list --guild {guild_id} --channel {voice_channel_id} --since -2d"
            ),
            resolve_context: format!(
                "clankcord context resolve --guild {guild_id} --channel {voice_channel_id} --reference <natural-language-reference>"
            ),
            render_transcript_range: format!(
                "clankcord transcripts render --guild {guild_id} --channel {voice_channel_id} --from <time> --to <time> --format markdown"
            ),
            search_transcripts: format!(
                "clankcord transcripts search --guild {guild_id} --channel {voice_channel_id} --query <query> --since -7d"
            ),
            participant_trace: format!(
                "clankcord participants trace --guild {guild_id} --user <user-id> --from <time> --to <time> --include-speech-snippets"
            ),
            search_messages: format!(
                "clankcord messages search --guild-id {guild_id} --query <query>"
            ),
            read_messages: "clankcord messages read --target <channel-or-thread-id>".to_string(),
            submit_response: format!(
                "clankcord responses submit --job {} --sink agent-chat --stdin",
                job.id
            ),
            automation_spec: "clankcord automations spec".to_string(),
            validate_automation: "clankcord automations validate --stdin".to_string(),
            create_automation: "clankcord automations create --stdin".to_string(),
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
        object.insert("submit_response".to_string(), json!(self.submit_response));
        object.insert("automation_spec".to_string(), json!(self.automation_spec));
        object.insert(
            "validate_automation".to_string(),
            json!(self.validate_automation),
        );
        object.insert(
            "create_automation".to_string(),
            json!(self.create_automation),
        );
        Value::Object(object)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::CommandRequest;

    #[test]
    fn agent_task_packet_keeps_activation_context_compact() {
        let command = CommandRequest::from_json(&json!({
            "action": "dispatch_now",
            "command_kind": "agent_task",
            "guild_id": "guild",
            "voice_channel_id": "voice",
            "requested_by_user_id": "user",
            "requested_by_speaker_label": "Will",
            "arguments": {
                "request": "summarize the floating point discussion",
                "source_event_ids": ["evt_1", "evt_2"],
                "activation": {
                    "activation_id": "act_1",
                    "wake_event_id": "evt_1",
                    "source_event_ids": ["evt_1", "evt_2"],
                    "prior_to_activation": [{
                        "event_id": "evt_0",
                        "event_kind": "speech_segment",
                        "speaker_label": "Will",
                        "text": "we were talking about floats",
                        "token_logprobs": [{"token": "we", "logprob": -0.1}],
                        "words": [{"word": "we", "start": 0.0, "end": 0.1}]
                    }],
                    "post_activation_turn": [{
                        "event_id": "evt_2",
                        "event_kind": "speech_segment",
                        "speaker_label": "Will",
                        "text": "hey clanky summarize this"
                    }]
                }
            }
        }))
        .unwrap();
        let job = Job::agent_task("guild", "voice", "user", command);

        let packet = AgentTaskPacket::from_job(&job).to_json();
        let text = serde_json::to_string(&packet).unwrap();

        assert!(text.contains("summarize the floating point discussion"));
        assert!(text.contains("we were talking about floats"));
        assert!(text.contains("hey clanky summarize this"));
        assert!(text.contains("clankcord automations spec"));
        assert!(!text.contains("token_logprobs"));
        assert!(!text.contains("logprob"));
        assert!(!text.contains("words"));
    }
}
