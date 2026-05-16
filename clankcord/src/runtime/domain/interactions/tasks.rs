use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use crate::Result;
use crate::adapters::codex::{codex_response_text, extract_codex_usage};
use crate::config::non_empty;
use crate::runtime::agents::{AgentInfrastructureError, AgentInvocationRequest, AgentRole};
use crate::runtime::jobs::{
    AgentInvocationMetadata, AgentPreflightCheck, AgentPreflightMetadata, AgentTaskMetadata,
    BinaryPayload,
};
use crate::runtime::timeline::{
    JobVisibility, event_text, isoformat_z, parse_instant, set, utc_now,
};
use crate::runtime::util::{first_non_empty, first_value_string, log, preview};
use crate::runtime::{
    Job, JobKind, JobState, Runtime, TextDeliveryKind, TextDeliveryPayload, TextTarget,
};

const AGENT_UNAVAILABLE_MESSAGE: &str =
    "It looks like ChatGPT is unavailable right now. Try again later.";

impl Runtime {
    pub(crate) async fn recover_interrupted_agent_tasks(&self) -> Result<Vec<Value>> {
        let mut recovered = Vec::new();
        for job in self
            .timeline_store
            .list_jobs_with_visibility(None, Some(JobState::Running), JobVisibility::Visible)
            .await?
            .into_iter()
            .filter(|job| job.kind == JobKind::AgentTask)
        {
            let submitted_text_deliveries = self.text_delivery_jobs_for_source(&job.id).await?;
            if !submitted_text_deliveries.is_empty() {
                let mut completed = job.clone();
                completed.mark_complete();
                completed.metadata.agent_task_mut().response_text =
                    "RESPONSE_SUBMITTED".to_string();
                self.timeline_store.update_job(&completed).await?;
                recovered.push(json!({
                    "dispatched": true,
                    "job": completed.to_value(),
                    "submitted_text_deliveries": submitted_text_deliveries.into_iter().map(|job| job.to_value()).collect::<Vec<_>>(),
                    "recovered": true,
                }));
                continue;
            }
            let mut interrupted = job.clone();
            interrupted.set_state(JobState::AgentDispatchFailed);
            interrupted.metadata.agent_task_mut().dispatch_error =
                "agent task was interrupted by runtime restart".to_string();
            self.timeline_store.update_job(&interrupted).await?;
            let result = json!({
                "dispatched": false,
                "job": interrupted.to_value(),
                "interrupted": true,
            });
            recovered.push(result);
        }
        Ok(recovered)
    }

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

        let workdir = agent_task_workdir(&latest);
        fs::create_dir_all(&workdir)?;
        let repo_dir = agent_repo_dir();
        let agent_env = agent_task_env(&latest, &workdir, repo_dir.as_ref());
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

        let prompt_path = job_dir.join(format!("{}.agent-prompt.txt", latest.id));
        let result_path = job_dir.join(format!("{}.agent-result.txt", latest.id));
        let raw_result_path = job_dir.join(format!("{}.codex.jsonl", latest.id));
        let agent_session_id = agent_task_session_id(&latest)?;
        let agent_session = self
            .timeline_store
            .get_agent_session_record(&agent_session_id)
            .await?;
        let session_key = agent_session.invocation_key();
        let prior_session_id = non_empty(
            latest
                .metadata
                .agent_task()
                .map(|task| task.agent.session_id.clone())
                .unwrap_or_default(),
            agent_session.codex_session_id.clone(),
        );
        let include_master_prompt = prior_session_id.trim().is_empty();
        let prompt = self
            .build_agent_task_message_for_session(&latest, &workdir, include_master_prompt)
            .await?;
        fs::write(&prompt_path, &prompt)?;
        let mut prepared = latest.clone();
        let mut task_metadata = prepared
            .metadata
            .agent_task()
            .cloned()
            .unwrap_or_else(AgentTaskMetadata::default);
        task_metadata.workdir_path = workdir.display().to_string();
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
            cwd: Some(workdir.clone()),
            model: agent_task_model(),
            env: agent_env,
            result_path: result_path.clone(),
            raw_result_path: raw_result_path.clone(),
        })?;

        if !invocation.success {
            let detail = first_non_empty([
                invocation.stderr.trim().to_string(),
                invocation.stdout.trim().to_string(),
                format!(
                    "codex exited {}",
                    invocation
                        .returncode
                        .map(|code| code.to_string())
                        .unwrap_or_else(|| "without a status code".to_string())
                ),
            ]);
            if agent_invocation_infrastructure_failure(&detail) {
                return Err(AgentInfrastructureError::new(detail).into());
            }
            anyhow::bail!("{detail}");
        }

        let response_text = codex_response_text(&invocation.stdout, &invocation.final_message);
        let completed_session = self
            .set_agent_session_codex_session(
                &agent_session_id,
                non_empty(
                    invocation
                        .session
                        .as_ref()
                        .map(|session| session.session_id.clone())
                        .unwrap_or_default(),
                    invocation.session_id.clone(),
                ),
            )
            .await?;
        Ok(AgentTaskMetadata {
            workdir_path: workdir.display().to_string(),
            prompt_path: prompt_path.display().to_string(),
            result_path: result_path.display().to_string(),
            raw_result_path: raw_result_path.display().to_string(),
            dispatch_stdout_preview: preview(&response_text, 1000),
            dispatch_stderr: preview(&invocation.stderr, 1000),
            agent: AgentInvocationMetadata {
                session_id: completed_session.codex_session_id,
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
        if latest.cancel_requested() {
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
        let submitted_text_deliveries = self.text_delivery_jobs_for_source(&latest.id).await?;
        if !submitted_text_deliveries.is_empty() {
            latest.mark_complete();
            self.timeline_store.update_job(&latest).await?;
            return Ok(json!({
                "dispatched": true,
                "job": latest.to_value(),
                "submitted_text_deliveries": submitted_text_deliveries.into_iter().map(|job| job.to_value()).collect::<Vec<_>>(),
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
                "agent task reported RESPONSE_SUBMITTED but no text delivery job exists for source job {job_id}"
            );
        }
        if response_text == "NO_RESPONSE_NEEDED" {
            latest.mark_complete();
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
                        "reason": "agent determined no visible response was needed",
                    }),
                )
                .await?;
            return Ok(json!({"dispatched": true, "job": latest.to_value(), "response": "none"}));
        }
        if response_text.is_empty() {
            anyhow::bail!("agent task completed without submitting a text delivery job");
        }
        anyhow::bail!(
            "agent task returned final text instead of submitting a text delivery job or NO_RESPONSE_NEEDED"
        )
    }

    async fn text_delivery_jobs_for_source(&self, source_job_id: &str) -> Result<Vec<Job>> {
        self.timeline_store
            .list_text_delivery_jobs_for_source(source_job_id)
            .await
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
        let publish_unavailable_text =
            is_infrastructure_error && agent_invocation_infrastructure_failure(&error_text);
        let mut latest = self.timeline_store.get_job(&job_id).await?;
        if latest.cancel_requested() {
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
        let submitted_text_deliveries = self.text_delivery_jobs_for_source(&job_id).await?;
        if !submitted_text_deliveries.is_empty() {
            latest.mark_complete();
            latest.metadata.agent_task_mut().response_text = "RESPONSE_SUBMITTED".to_string();
            latest.metadata.agent_task_mut().dispatch_error = error_text.clone();
            self.timeline_store.update_job(&latest).await?;
            return Ok(json!({
                "dispatched": true,
                "job": latest.to_value(),
                "submitted_text_deliveries": submitted_text_deliveries.into_iter().map(|job| job.to_value()).collect::<Vec<_>>(),
                "error_after_response": error_text,
            }));
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
        let text_delivery_job = if publish_unavailable_text {
            self.agent_unavailable_text_delivery_job(&latest).await?
        } else {
            None
        };
        self.timeline_store.update_job(&latest).await?;
        log(&format!(
            "agent task dispatch failed for {job_id}: {error_text}"
        ));
        Ok(json!({
            "dispatched": false,
            "job": latest.to_value(),
            "error": error_text,
            "text_delivery_job": text_delivery_job.map(|job| job.to_value()),
        }))
    }

    async fn agent_unavailable_text_delivery_job(&self, job: &Job) -> Result<Option<Job>> {
        if !self
            .text_delivery_jobs_for_source(&job.id)
            .await?
            .is_empty()
        {
            return Ok(None);
        }
        let requested_by_user_id = agent_task_requester_id(job);
        let response = Job::text_delivery(
            job.guild_id.clone(),
            job.voice_channel_id.clone(),
            requested_by_user_id.clone(),
            TextDeliveryPayload::new(
                TextDeliveryKind::Message,
                TextTarget::default(),
                AGENT_UNAVAILABLE_MESSAGE,
                job.id.clone(),
                requested_by_user_id,
                false,
            ),
        );
        self.timeline_store.create_job(response).await.map(Some)
    }
}

fn agent_task_requester_id(job: &Job) -> String {
    let command_requester = job
        .command()
        .map(|command| command.requested_by_user_id.clone())
        .unwrap_or_default();
    first_non_empty([job.requested_by_user_id.clone(), command_requester])
}

pub fn agent_invocation_infrastructure_failure(detail: &str) -> bool {
    detail.contains("TokenRefreshFailed")
        || detail.contains("invalid_grant")
        || detail.contains("Auth(")
}

#[derive(Debug, Clone, Default)]
pub struct AgentTaskPromptContext {
    pub job_id: String,
    pub agent_session_id: String,
    pub guild_id: String,
    pub voice_channel_id: String,
    pub requested_by_user_id: String,
    pub requested_by: String,
    pub request: String,
    pub workdir: String,
    pub previous_context: Vec<String>,
    pub question: Vec<String>,
}

impl Runtime {
    async fn build_agent_task_message_for_session(
        &self,
        job: &Job,
        workdir: &std::path::Path,
        include_master_prompt: bool,
    ) -> Result<String> {
        let context = self.agent_task_prompt_context(job, workdir).await?;
        Ok(build_agent_task_message_for_session(
            &context,
            include_master_prompt,
        ))
    }

    async fn agent_task_prompt_context(
        &self,
        job: &Job,
        workdir: &std::path::Path,
    ) -> Result<AgentTaskPromptContext> {
        let command = job.command();
        let request = command
            .map(|command| command.arguments.request_text())
            .unwrap_or_default();
        let requested_by = command
            .map(|command| command.requested_by_speaker_label.clone())
            .unwrap_or_default();
        let source_event_ids = agent_task_source_event_ids(job);
        let end = parse_instant(&job.created_at).unwrap_or_else(utc_now);
        let start = end - chrono::Duration::minutes(5);
        let speech_kinds = set(["speech_segment", "transcript", "discord_text_message"]);
        let events = self
            .timeline_store
            .load_events(
                &job.guild_id,
                &job.voice_channel_id,
                Some(start),
                Some(end + chrono::Duration::minutes(2)),
                Some(&speech_kinds),
                None,
                false,
            )
            .await?;
        let mut previous_context = Vec::new();
        let mut question = Vec::new();
        for event in events {
            let line = agent_prompt_event_line(&event);
            if line.is_empty() {
                continue;
            }
            let event_id = first_value_string(&event, &["event_id", "eventId"]);
            if source_event_ids.contains(&event_id) {
                question.push(line);
            } else {
                previous_context.push(line);
            }
        }
        if question.is_empty() && !request.trim().is_empty() {
            question.push(format!(
                "[{}] {} ({}): {}",
                job.created_at,
                non_empty(requested_by.clone(), "requester".to_string()),
                job.requested_by_user_id,
                request
            ));
        }
        Ok(AgentTaskPromptContext {
            job_id: job.id.clone(),
            agent_session_id: agent_task_session_id(job)?,
            guild_id: job.guild_id.clone(),
            voice_channel_id: job.voice_channel_id.clone(),
            requested_by_user_id: job.requested_by_user_id.clone(),
            requested_by,
            request,
            workdir: workdir.display().to_string(),
            previous_context,
            question,
        })
    }
}

pub fn build_agent_task_message(context: &AgentTaskPromptContext) -> String {
    build_agent_task_message_for_session(context, true)
}

pub fn build_agent_task_message_for_session(
    context: &AgentTaskPromptContext,
    include_master_prompt: bool,
) -> String {
    let mut sections = Vec::new();
    if include_master_prompt {
        sections.push(agent_master_prompt());
    }
    sections.push(agent_job_prompt(context));
    sections.join("\n\n")
}

fn agent_job_prompt(context: &AgentTaskPromptContext) -> String {
    let previous = if context.previous_context.is_empty() {
        "(no prior user-visible speech in the captured 5-minute window)".to_string()
    } else {
        context.previous_context.join("\n")
    };
    let question = if context.question.is_empty() {
        "(no activation transcript lines were captured; use the request field and fetch more context if needed)".to_string()
    } else {
        context.question.join("\n")
    };
    [
        "JOB:",
        &format!("job_id: {}", context.job_id),
        &format!("agent_session_id: {}", context.agent_session_id),
        &format!("guild_id: {}", context.guild_id),
        &format!("voice_channel_id: {}", context.voice_channel_id),
        &format!(
            "requested_by_user_id: {}",
            context.requested_by_user_id
        ),
        &format!("requested_by: {}", context.requested_by),
        &format!("request: {}", context.request),
        "",
        "WORKDIR:",
        &format!("CLANKCORD_AGENT_WORKDIR={}", context.workdir),
        "",
        "===== PREVIOUS CONTEXT =====",
        &previous,
        "",
        "===== QUESTION / ACTIVATION =====",
        &question,
        "",
        "CONTEXT NOTE:",
        "The transcript above is only a compact 5-minute local window. It may omit prior discussion, missing speaker turns, ambiguous references, and broader room history.",
        "If the request appears to depend on anything outside this local window, use Clankcord CLI commands to search or render more user messages before answering.",
        "Prefer explicit file output for large transcript, timeline, search, or job results: `--file result.json --format json`, then inspect the file from your workdir with jq, rg, and sed.",
    ]
    .join("\n")
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
        "Use `clankcord --help`, `clankcord responses --help`, and subcommand `--help` to discover the command surface. For visible responses in the current agent session, use `clankcord responses send`; for explicitly private replies, use `clankcord responses dm --to ...`.",
        "",
        "ENVIRONMENT:",
        "You run from $CLANKCORD_AGENT_WORKDIR, a writable working directory for notes, temp files, command outputs, and intermediate artifacts. The Clankcord source checkout is at $CLANKCORD_REPO_DIR.",
        "Current job context is available in CLANKCORD_AGENT_JOB_ID, CLANKCORD_AGENT_SESSION_ID, CLANKCORD_AGENT_GUILD_ID, CLANKCORD_AGENT_VOICE_CHANNEL_ID, and CLANKCORD_AGENT_REQUESTED_BY_USER_ID.",
        "For large transcript, timeline, search, or job outputs, prefer explicit file output like `--file result.json --format json`, then inspect files with jq, rg, and sed. Large files may be very large; avoid printing them into your conversation context.",
        "",
        "RESPONSE BEHAVIOR:",
        "You do not have to publish a visible response for every job.",
        "If the wake word appears to be a false activation, cross-talk, an accidental invocation, or the captured question is not actually directed at Clankcord, do not respond visibly. Finish with NO_RESPONSE_NEEDED.",
        "If the user requested a straightforward action where a visible answer would add noise, perform the action through Clankcord and finish with NO_RESPONSE_NEEDED unless the action failed or the user clearly expects confirmation.",
        "If a user asks you to DM them about something, treat the request and the answer as private. Use `clankcord responses dm` for the substantive response, and do not publish the topic, answer, summary, result, or confirmation to a public channel unless the user explicitly asks for public disclosure.",
        "If you publish a visible response, use `clankcord responses send` for the current session surface or `clankcord responses dm` for explicit DMs. After successful submission, finish with RESPONSE_SUBMITTED. Final text is not a publication path.",
        "",
        "You may search the web and should use web research when it would materially improve the answer, especially for current facts, unfamiliar topics, fact-checking, product or technical details, or anything where the transcript alone is not enough.",
        "Do not invent facts when research is possible.",
        "",
        "When a user asks for runtime work such as transcript creation, room control, sound playback, reminders, or publication, use the corresponding `clankcord` command.",
        "When a user asks for future, conditional, or recurring behavior, read `clankcord automations spec`, validate with `clankcord automations validate --stdin`, then register with `clankcord automations create --stdin`. Use the Clankcord CLI for automations, not the runtime HTTP endpoints. Automations default to one shot unless the user clearly asks for recurring behavior. Give automations reasonable expiries. Resolve named people to Discord user IDs before storing durable conditions whenever possible.",
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
    agent_task_session_id(job)?;
    Ok(())
}

fn agent_task_session_id(job: &Job) -> Result<String> {
    let crate::runtime::JobPayload::AgentTask(payload) = &job.payload else {
        anyhow::bail!("job {} is not an agent task", job.id);
    };
    if payload.agent_session_id.trim().is_empty() {
        anyhow::bail!("agent task job {} is missing agent_session_id", job.id);
    }
    Ok(payload.agent_session_id.clone())
}

pub fn agent_task_workdir(job: &Job) -> PathBuf {
    let agent_session_id = match &job.payload {
        crate::runtime::JobPayload::AgentTask(payload) => payload.agent_session_id.clone(),
        _ => job.id.clone(),
    };
    agent_workspace_root().join("task").join(agent_session_id)
}

fn agent_workspace_root() -> PathBuf {
    env::var("CLANKCORD_AGENT_WORKSPACES_ROOT")
        .ok()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/clankcord/state/agent-workspaces"))
}

fn agent_task_env(
    job: &Job,
    workdir: &std::path::Path,
    repo_dir: Option<&PathBuf>,
) -> BTreeMap<String, String> {
    let mut vars = env::vars().collect::<BTreeMap<_, _>>();
    vars.entry("CLANKCORD_API_BASE_URL".to_string())
        .or_insert_with(|| "http://127.0.0.1:8091".to_string());
    vars.insert(
        "CLANKCORD_AGENT_WORKDIR".to_string(),
        workdir.display().to_string(),
    );
    vars.insert("CLANKCORD_AGENT_JOB_ID".to_string(), job.id.clone());
    if let Ok(agent_session_id) = agent_task_session_id(job) {
        vars.insert("CLANKCORD_AGENT_SESSION_ID".to_string(), agent_session_id);
    }
    vars.insert("CLANKCORD_AGENT_GUILD_ID".to_string(), job.guild_id.clone());
    vars.insert(
        "CLANKCORD_AGENT_VOICE_CHANNEL_ID".to_string(),
        job.voice_channel_id.clone(),
    );
    vars.insert(
        "CLANKCORD_AGENT_REQUESTED_BY_USER_ID".to_string(),
        job.requested_by_user_id.clone(),
    );
    if let Some(repo_dir) = repo_dir {
        vars.insert(
            "CLANKCORD_REPO_DIR".to_string(),
            repo_dir.display().to_string(),
        );
    }
    vars
}

fn agent_repo_dir() -> Option<PathBuf> {
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
    let agent_env = envs.cloned().unwrap_or_default();
    let codex_bin = env::var("CLANKCORD_CODEX_BIN")
        .or_else(|_| env::var("CODEX_BIN"))
        .unwrap_or_else(|_| "codex".to_string());
    let checks: Vec<Vec<String>> = vec![
        vec![codex_bin, "--version".to_string()],
        vec!["rg".to_string(), "--version".to_string()],
        vec!["jq".to_string(), "--version".to_string()],
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
            "send".to_string(),
            "--help".to_string(),
        ],
        vec![
            "clankcord".to_string(),
            "members".to_string(),
            "resolve".to_string(),
            "--help".to_string(),
        ],
        vec![
            "clankcord".to_string(),
            "rooms".to_string(),
            "occupants".to_string(),
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

fn agent_task_source_event_ids(job: &Job) -> std::collections::BTreeSet<String> {
    let Some(command) = job.command() else {
        return Default::default();
    };
    let arguments = command.arguments.to_json();
    let mut ids = string_array_field(&arguments, "source_event_ids")
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
    if let Some(activation) = arguments.get("activation") {
        ids.extend(string_array_field(activation, "source_event_ids"));
        for key in [
            "wake_event_id",
            "latest_wake_event_id",
            "activation_event_id",
            "latest_activation_event_id",
        ] {
            let value = first_value_string(activation, &[key]);
            if !value.is_empty() {
                ids.insert(value);
            }
        }
    }
    ids
}

fn agent_prompt_event_line(event: &Value) -> String {
    let text = event_text(event);
    if text.trim().is_empty() {
        return String::new();
    }
    let timestamp = first_non_empty([
        first_value_string(event, &["segment_start_time", "startedAt"]),
        first_value_string(event, &["timestamp", "created_at"]),
    ]);
    let speaker = first_non_empty([
        first_value_string(event, &["speaker_label", "speakerLabel"]),
        first_value_string(event, &["speaker_username", "speakerUsername"]),
        "unknown".to_string(),
    ]);
    let speaker_user_id = first_value_string(event, &["speaker_user_id", "speakerId"]);
    format!("[{timestamp}] {speaker} ({speaker_user_id}): {text}")
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
