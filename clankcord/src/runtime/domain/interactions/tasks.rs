use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::prompts::{
    AgentPromptRequestOrigin, AgentTaskPromptVars, render_agent_task_prompt_from_dir,
    render_configured_agent_task_prompt, render_configured_master_prompt,
    render_master_prompt_from_dir,
};
use crate::Result;
use crate::adapters::codex::{codex_response_text, extract_codex_usage};
use crate::config;
use crate::runtime::agents::{
    AgentInfrastructureError, AgentInvocationRequest, AgentRole, AgentRuntime,
};
use crate::runtime::jobs::{
    AgentInvocationMetadata, AgentPreflightCheck, AgentPreflightMetadata, AgentTaskMetadata,
    BinaryPayload,
};
use crate::runtime::timeline::{
    JobVisibility, event_text, isoformat_z, parse_instant, set, utc_now,
};
use crate::runtime::util::{first_non_empty, first_value_string, log, non_empty, preview};
use crate::runtime::{
    AgentSessionRouteKind, DiscordTypingAction, DiscordTypingIndicatorPayload, Job, JobKind,
    JobState, Runtime, RuntimeScopeKind, TextDeliveryKind, TextDeliveryPayload, TextTarget,
    TextTargetKind,
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
            interrupted.set_state(JobState::Failed);
            let error_text = "agent task was interrupted by runtime restart".to_string();
            interrupted.metadata.error = error_text.clone();
            interrupted.metadata.agent_task_mut().dispatch_error = error_text;
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
        let mut latest = self.timeline_store.get_job(&job_id).await.unwrap_or(job);
        let children = self.timeline_store.list_child_jobs(&latest.id).await?;
        let mut task_metadata = latest
            .metadata
            .agent_task()
            .cloned()
            .unwrap_or_else(AgentTaskMetadata::default);
        if agent_task_retry_after_stopped_error(&task_metadata, &children) {
            latest.metadata.agent_task_mut().dispatch_error.clear();
            self.timeline_store.update_job(&latest).await?;
            task_metadata = latest
                .metadata
                .agent_task()
                .cloned()
                .unwrap_or_else(AgentTaskMetadata::default);
        }
        if agent_task_has_dispatch_outcome(&task_metadata) {
            return self
                .finish_agent_task_after_typing_stop(latest, task_metadata)
                .await;
        }

        let attempts = task_metadata.dispatch_attempts;
        if attempts >= 3 {
            let mut failed = latest.clone();
            failed.set_state(JobState::Failed);
            failed.metadata.error = "agent task dispatch attempts exhausted".to_string();
            self.timeline_store.update_job(&failed).await?;
            return Ok(
                json!({"dispatched": false, "job": failed.to_value(), "reason": "agent task dispatch attempts exhausted"}),
            );
        }

        match agent_task_typing_child(&children, DiscordTypingAction::Start, Some(attempts)) {
            Some(start) if !start.state.is_terminal() => {
                return self.wait_dispatched_job(&job_id, latest, Vec::new()).await;
            }
            Some(start) if start.state != JobState::Complete => {
                return self
                    .fail_dispatched_job(
                        &job_id,
                        latest,
                        anyhow::anyhow!(
                            "agent task typing start dependency {} ended as {}: {}",
                            start.id,
                            start.state,
                            start.metadata.error
                        ),
                    )
                    .await;
            }
            Some(_) => {}
            None => {
                return self
                    .wait_dispatched_job(
                        &job_id,
                        latest.clone(),
                        vec![agent_task_typing_job(
                            &latest,
                            DiscordTypingAction::Start,
                            attempts,
                        )],
                    )
                    .await;
            }
        }

        match self.dispatch_agent_task(&latest).await {
            Ok(dispatch_result) => {
                let mut prepared = self.timeline_store.get_job(&job_id).await?;
                prepared.metadata.set_agent_task(dispatch_result);
                self.timeline_store.update_job(&prepared).await?;
                self.wait_for_agent_task_typing_stop(prepared, attempts)
                    .await
            }
            Err(error) => {
                let preflight = error
                    .downcast_ref::<AgentInfrastructureError>()
                    .and_then(AgentInfrastructureError::preflight)
                    .cloned();
                let error_text = error.to_string();
                let mut failed = self.timeline_store.get_job(&job_id).await?;
                if let Some(preflight) = preflight {
                    failed.metadata.agent_task_mut().preflight = Some(preflight);
                }
                failed.metadata.agent_task_mut().dispatch_error = error_text;
                self.timeline_store.update_job(&failed).await?;
                self.wait_for_agent_task_typing_stop(failed, attempts).await
            }
        }
    }

    async fn finish_agent_task_after_typing_stop(
        &self,
        job: Job,
        task_metadata: AgentTaskMetadata,
    ) -> Result<Value> {
        let job_id = job.id.clone();
        let children = self.timeline_store.list_child_jobs(&job_id).await?;
        if let Some(stop) = agent_task_typing_child(&children, DiscordTypingAction::Stop, None) {
            if !stop.state.is_terminal() {
                return self.wait_dispatched_job(&job_id, job, Vec::new()).await;
            }
            if stop.state != JobState::Complete {
                return self
                    .fail_dispatched_job(
                        &job_id,
                        job,
                        anyhow::anyhow!(
                            "agent task typing stop dependency {} ended as {}: {}",
                            stop.id,
                            stop.state,
                            stop.metadata.error
                        ),
                    )
                    .await;
            }
            let attempts = stop
                .discord_typing_indicator_payload()
                .map(|payload| payload.agent_task_attempt)
                .unwrap_or(task_metadata.dispatch_attempts);
            if !task_metadata.dispatch_error.trim().is_empty() {
                return self
                    .fail_agent_task_job(
                        job_id,
                        attempts,
                        anyhow::anyhow!(task_metadata.dispatch_error),
                    )
                    .await;
            }
            return match self
                .complete_agent_task_job(job_id.clone(), task_metadata)
                .await
            {
                Ok(value) => Ok(value),
                Err(error) => self.fail_agent_task_job(job_id, attempts, error).await,
            };
        }

        let attempts = agent_task_typing_child(&children, DiscordTypingAction::Start, None)
            .and_then(|start| start.discord_typing_indicator_payload())
            .map(|payload| payload.agent_task_attempt)
            .unwrap_or(task_metadata.dispatch_attempts);
        self.wait_for_agent_task_typing_stop(job, attempts).await
    }

    async fn wait_for_agent_task_typing_stop(&self, job: Job, attempts: i64) -> Result<Value> {
        let job_id = job.id.clone();
        self.wait_dispatched_job(
            &job_id,
            job.clone(),
            vec![agent_task_typing_job(
                &job,
                DiscordTypingAction::Stop,
                attempts,
            )],
        )
        .await
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
            .channel_dir(&latest.guild_id, &latest.scope_id)
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
        let invocation = AgentRuntime::default().invoke(AgentInvocationRequest {
            role: AgentRole::Task,
            session_key,
            job_id: latest.id.clone(),
            guild_id: latest.guild_id.clone(),
            scope_id: latest.scope_id.clone(),
            prior_session_id,
            prompt,
            cwd: Some(workdir.clone()),
            model: agent_task_model(),
            reasoning_effort: config::codex_reasoning_effort(),
            fast_mode: config::codex_fast_mode(),
            env: agent_env,
            result_path: result_path.clone(),
            raw_result_path: raw_result_path.clone(),
        })?;

        self.append_agent_invocation_warning_events(
            &latest,
            &[invocation.stdout.as_str(), invocation.stderr.as_str()],
        )
        .await?;

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
                reasoning_effort: invocation.reasoning_effort.as_str().to_string(),
                fast_mode: invocation.fast_mode,
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
        self.timeline_store.update_job(&latest).await?;
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
                    &latest.scope_id,
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
        if let Some(reason) = agent_task_no_response_reason(response_text) {
            latest.mark_complete();
            latest.metadata.agent_task_mut().result_suppressed = true;
            self.timeline_store.update_job(&latest).await?;
            self.timeline_store
                .append_event(
                    &latest.guild_id,
                    &latest.scope_id,
                    json!({
                        "event_kind": "agent_task_result_suppressed",
                        "kind": "agent_task_result_suppressed",
                        "job_id": job_id,
                        "job_kind": latest.kind.as_str(),
                        "reason": reason,
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
        let error_text = error.to_string();
        let infrastructure_error = error.downcast_ref::<AgentInfrastructureError>();
        let is_infrastructure_error =
            infrastructure_error.is_some() || agent_task_error_text_is_infrastructure(&error_text);
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
        if is_infrastructure_error || next_attempts >= 3 {
            latest.set_state(JobState::Failed);
            latest.metadata.error = error_text.clone();
        } else {
            latest.set_state(JobState::Queued);
        }
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
            job.scope(),
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

    async fn append_agent_invocation_warning_events(
        &self,
        job: &Job,
        details: &[&str],
    ) -> Result<()> {
        let mut emitted = std::collections::BTreeSet::new();
        for detail in details {
            let Some(event_kind) = agent_invocation_warning_event_kind(detail) else {
                continue;
            };
            if !emitted.insert(event_kind) {
                continue;
            }
            self.timeline_store
                .append_event(
                    &job.guild_id,
                    &job.scope_id,
                    json!({
                        "event_kind": event_kind,
                        "kind": event_kind,
                        "severity": "warning",
                        "job_id": job.id.clone(),
                        "job_kind": job.kind.as_str(),
                        "message": agent_invocation_warning_message(event_kind),
                    }),
                )
                .await?;
        }
        Ok(())
    }
}

fn agent_task_requester_id(job: &Job) -> String {
    let command_requester = job
        .command()
        .map(|command| command.requested_by_user_id.clone())
        .unwrap_or_default();
    first_non_empty([job.requested_by_user_id.clone(), command_requester])
}

fn agent_task_typing_job(job: &Job, action: DiscordTypingAction, attempts: i64) -> Job {
    let requested_by_user_id = agent_task_requester_id(job);
    Job::discord_typing_indicator(
        job.scope(),
        requested_by_user_id.clone(),
        DiscordTypingIndicatorPayload {
            action,
            target: TextTarget {
                kind: TextTargetKind::AgentSession,
                channel_id: String::new(),
                user_id: String::new(),
            },
            source_job_id: job.id.clone(),
            requested_by_user_id,
            agent_task_attempt: attempts,
        },
    )
}

fn agent_task_typing_child(
    children: &[Job],
    action: DiscordTypingAction,
    attempts: Option<i64>,
) -> Option<&Job> {
    children.iter().rev().find(|child| {
        child
            .discord_typing_indicator_payload()
            .is_some_and(|payload| {
                payload.action == action
                    && attempts.is_none_or(|attempts| payload.agent_task_attempt == attempts)
            })
    })
}

fn agent_task_has_dispatch_outcome(task: &AgentTaskMetadata) -> bool {
    !task.dispatch_error.trim().is_empty()
        || !task.agent.provider.trim().is_empty()
        || !task.command.trim().is_empty()
}

fn agent_task_retry_after_stopped_error(task: &AgentTaskMetadata, children: &[Job]) -> bool {
    !task.dispatch_error.trim().is_empty()
        && agent_task_typing_child(children, DiscordTypingAction::Stop, None)
            .and_then(Job::discord_typing_indicator_payload)
            .is_some_and(|payload| task.dispatch_attempts > payload.agent_task_attempt)
}

fn agent_task_error_text_is_infrastructure(error_text: &str) -> bool {
    error_text.starts_with("agent task preflight failed:")
        || agent_invocation_infrastructure_failure(error_text)
}

fn agent_task_no_response_reason(response_text: &str) -> Option<&'static str> {
    let normalized = response_text
        .trim()
        .trim_matches('`')
        .trim()
        .trim_end_matches('.')
        .replace(' ', "_")
        .replace('-', "_")
        .to_ascii_uppercase();
    (normalized == "NO_RESPONSE_NEEDED").then_some("agent chose not to produce a visible response")
}

pub fn agent_invocation_infrastructure_failure(detail: &str) -> bool {
    if agent_invocation_warning_event_kind(detail).is_some() {
        return false;
    }
    detail.contains("TokenRefreshFailed")
        || detail.contains("invalid_grant")
        || detail.contains("Auth(")
}

pub fn agent_invocation_warning_event_kind(detail: &str) -> Option<&'static str> {
    let normalized = detail.to_ascii_lowercase();
    let mcp_related = normalized.contains("mcp");
    let token_auth_related = normalized.contains("tokenrefreshfailed")
        || normalized.contains("invalid_grant")
        || normalized.contains("expired")
        || (normalized.contains("token") && normalized.contains("auth"))
        || (normalized.contains("token") && normalized.contains("invalid"));
    (mcp_related && token_auth_related).then_some("agent_mcp_token_warning")
}

fn agent_invocation_warning_message(event_kind: &str) -> &'static str {
    match event_kind {
        "agent_mcp_token_warning" => {
            "Codex reported an MCP authentication token warning during agent invocation."
        }
        _ => "Codex reported an agent invocation warning.",
    }
}

#[derive(Debug, Clone)]
pub struct AgentTaskPromptContext {
    pub job_id: String,
    pub agent_session_id: String,
    pub resumed_from_agent_session_id: String,
    pub route_kind: AgentSessionRouteKind,
    pub request_origin: AgentPromptRequestOrigin,
    pub response_surface: TextTargetKind,
    pub guild_id: String,
    pub scope_id: String,
    pub requested_by_user_id: String,
    pub requested_by: String,
    pub request: String,
    pub workdir: String,
    pub recent_scope_events: Vec<String>,
    pub source_request_events: Vec<String>,
}

impl Runtime {
    async fn build_agent_task_message_for_session(
        &self,
        job: &Job,
        workdir: &std::path::Path,
        include_master_prompt: bool,
    ) -> Result<String> {
        let context = self.agent_task_prompt_context(job, workdir).await?;
        build_agent_task_message_for_session(&context, include_master_prompt)
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
        let source_events = self.agent_task_source_events(&source_event_ids).await?;
        let end = parse_instant(&job.created_at).unwrap_or_else(utc_now);
        let start = end - chrono::Duration::minutes(5);
        let speech_kinds = set(["speech_segment", "transcript", "discord_text_message"]);
        let events = self
            .timeline_store
            .load_scope_events(
                job.scope_kind,
                &job.guild_id,
                &job.scope_id,
                Some(start),
                Some(end + chrono::Duration::minutes(2)),
                Some(&speech_kinds),
                None,
                false,
            )
            .await?;
        let mut recent_scope_events = Vec::new();
        let mut source_request_events = Vec::new();
        for event in events {
            let line = agent_prompt_event_line(&event);
            if line.is_empty() {
                continue;
            }
            let event_id = first_value_string(&event, &["event_id", "eventId"]);
            if source_event_ids.contains(&event_id) {
                source_request_events.push(line);
            } else {
                recent_scope_events.push(line);
            }
        }
        if source_request_events.is_empty() && !request.trim().is_empty() {
            source_request_events.push(format!(
                "[{}] {} ({}): {}",
                job.created_at,
                non_empty(requested_by.clone(), "requester".to_string()),
                job.requested_by_user_id,
                request
            ));
        }
        let agent_session_id = agent_task_session_id(job)?;
        let agent_session = self
            .timeline_store
            .get_agent_session_record(&agent_session_id)
            .await?;
        let parent = self.agent_task_parent_job(job).await?;
        let request_origin = agent_task_request_origin(
            command,
            &agent_session.route_kind,
            &source_events,
            parent.as_ref(),
        );
        Ok(AgentTaskPromptContext {
            job_id: job.id.clone(),
            agent_session_id,
            resumed_from_agent_session_id: agent_session.resumed_from_agent_session_id,
            route_kind: agent_session.route_kind,
            request_origin,
            response_surface: agent_session.text_target.kind,
            guild_id: job.guild_id.clone(),
            scope_id: job.scope_id.clone(),
            requested_by_user_id: job.requested_by_user_id.clone(),
            requested_by,
            request,
            workdir: workdir.display().to_string(),
            recent_scope_events,
            source_request_events,
        })
    }

    async fn agent_task_source_events(
        &self,
        source_event_ids: &std::collections::BTreeSet<String>,
    ) -> Result<Vec<Value>> {
        let mut events = Vec::new();
        for event_id in source_event_ids {
            events.push(self.timeline_store.get_event(event_id).await?);
        }
        Ok(events)
    }

    async fn agent_task_parent_job(&self, job: &Job) -> Result<Option<Job>> {
        let Some(parent_job_id) = job.parent_job_id.as_deref() else {
            return Ok(None);
        };
        Ok(Some(self.timeline_store.get_job(parent_job_id).await?))
    }
}

pub fn build_agent_task_message(context: &AgentTaskPromptContext) -> Result<String> {
    build_agent_task_message_for_session(context, true)
}

pub fn build_agent_task_message_for_session(
    context: &AgentTaskPromptContext,
    include_master_prompt: bool,
) -> Result<String> {
    let mut sections = Vec::new();
    if include_master_prompt {
        sections.push(render_configured_master_prompt()?);
    }
    sections.push(render_configured_agent_task_prompt(
        &agent_task_prompt_vars(context),
    )?);
    Ok(sections.join("\n\n"))
}

pub fn build_agent_task_message_from_template_dir(
    context: &AgentTaskPromptContext,
    include_master_prompt: bool,
    prompt_dir: &Path,
) -> Result<String> {
    let mut sections = Vec::new();
    if include_master_prompt {
        sections.push(render_master_prompt_from_dir(prompt_dir)?);
    }
    sections.push(render_agent_task_prompt_from_dir(
        prompt_dir,
        &agent_task_prompt_vars(context),
    )?);
    Ok(sections.join("\n\n"))
}

fn agent_task_prompt_vars(context: &AgentTaskPromptContext) -> AgentTaskPromptVars {
    AgentTaskPromptVars {
        job_id: context.job_id.clone(),
        agent_session_id: context.agent_session_id.clone(),
        resumed_from_agent_session_id: context.resumed_from_agent_session_id.clone(),
        route_kind: context.route_kind,
        request_origin: context.request_origin,
        response_surface: context.response_surface,
        guild_id: context.guild_id.clone(),
        scope_id: context.scope_id.clone(),
        requested_by_user_id: context.requested_by_user_id.clone(),
        requested_by: context.requested_by.clone(),
        request: context.request.clone(),
        workdir: context.workdir.clone(),
        recent_scope_events: context.recent_scope_events.clone(),
        source_request_events: context.source_request_events.clone(),
    }
}

fn validate_agent_task_job(job: &Job) -> Result<()> {
    if job.id.trim().is_empty() || job.scope_id.trim().is_empty() {
        anyhow::bail!("agent task job is missing job/scope identity");
    }
    if job.scope_kind == RuntimeScopeKind::VoiceChannel && job.guild_id.trim().is_empty() {
        anyhow::bail!("voice agent task job is missing guild identity");
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
    config::agent_workspaces_root()
}

fn agent_task_env(
    job: &Job,
    workdir: &std::path::Path,
    repo_dir: Option<&PathBuf>,
) -> BTreeMap<String, String> {
    let mut vars = BTreeMap::new();
    vars.insert("CLANKCORD_API_BASE_URL".to_string(), config::api_base_url());
    vars.insert(
        "CODEX_HOME".to_string(),
        config::codex_home().display().to_string(),
    );
    vars.insert(
        "HOME".to_string(),
        config::codex_home().display().to_string(),
    );
    vars.insert(
        "CLANKCORD_AGENT_WORKDIR".to_string(),
        workdir.display().to_string(),
    );
    vars.insert("CLANKCORD_AGENT_JOB_ID".to_string(), job.id.clone());
    if let Ok(agent_session_id) = agent_task_session_id(job) {
        vars.insert("CLANKCORD_AGENT_SESSION_ID".to_string(), agent_session_id);
    }
    vars.insert("CLANKCORD_AGENT_GUILD_ID".to_string(), job.guild_id.clone());
    vars.insert("CLANKCORD_AGENT_SCOPE_ID".to_string(), job.scope_id.clone());
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
    Some(config::codex_workdir())
}

fn agent_task_model() -> Option<String> {
    config::codex_model()
}

fn run_agent_task_preflight(envs: Option<&BTreeMap<String, String>>) -> AgentPreflightMetadata {
    let agent_env = envs.cloned().unwrap_or_default();
    let codex_bin = config::codex_bin();
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
            "agent-sessions".to_string(),
            "search".to_string(),
            "--help".to_string(),
        ],
        vec![
            "clankcord".to_string(),
            "agent-sessions".to_string(),
            "sunset".to_string(),
            "--help".to_string(),
        ],
        vec![
            "clankcord".to_string(),
            "agent-sessions".to_string(),
            "resume".to_string(),
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
            "feedback".to_string(),
            "submit".to_string(),
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

fn agent_task_request_origin(
    command: Option<&crate::runtime::CommandRequest>,
    route_kind: &crate::runtime::AgentSessionRouteKind,
    source_events: &[Value],
    parent: Option<&Job>,
) -> AgentPromptRequestOrigin {
    if command
        .map(|command| command.arguments.to_json().get("activation").is_some())
        .unwrap_or(false)
    {
        return AgentPromptRequestOrigin::Voice;
    }
    if source_events
        .iter()
        .any(|event| first_value_string(event, &["event_kind", "kind"]) == "discord_text_message")
    {
        return AgentPromptRequestOrigin::Text;
    }
    if *route_kind == crate::runtime::AgentSessionRouteKind::Dm {
        return AgentPromptRequestOrigin::Text;
    }
    if parent.is_some_and(|job| {
        matches!(
            &job.payload,
            crate::runtime::JobPayload::AgentSessionResume(payload)
                if !payload.message.trim().is_empty()
        )
    }) {
        return AgentPromptRequestOrigin::Text;
    }
    AgentPromptRequestOrigin::Internal
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
