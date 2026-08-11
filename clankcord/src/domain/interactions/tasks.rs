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
use crate::adapters::codex::{
    codex_linear_mcp_config_args, codex_response_text, extract_codex_usage,
};
use crate::config;
use crate::domain::Ctx;
use crate::domain::agents::{
    AgentInfrastructureError, AgentInvocationRequest, AgentRole, AgentRuntime,
};
use crate::domain::interactions::agent_sessions;
use crate::engine::dispatcher;
use crate::model::agents::AgentSessionRouteKind;
use crate::model::job::{
    AgentInvocationMetadata, AgentPreflightCheck, AgentPreflightMetadata, AgentTaskMetadata,
    AgentTaskOutcome, AgentTaskPhase, BinaryPayload,
};
use crate::model::job::{
    DiscordTypingAction, DiscordTypingIndicatorPayload, Job, JobKind, JobState, TextDeliveryKind,
    TextDeliveryPayload, TextTarget, TextTargetKind,
};
use crate::model::scope::RuntimeScopeKind;
use crate::store::{JobVisibility, event_text};
use crate::time::{isoformat_z, parse_instant, utc_now};
use crate::util::set;
use crate::util::{first_non_empty, first_value_string, log, non_empty, preview};

use super::linear_mcp::insert_linear_mcp_env;

const AGENT_UNAVAILABLE_MESSAGE: &str =
    "It looks like ChatGPT is unavailable right now. Try again later.";

pub(crate) async fn recover_interrupted_agent_tasks(ctx: &Ctx) -> Result<Vec<Value>> {
    let mut recovered = Vec::new();
    for job in ctx
        .store
        .list_jobs_with_visibility(None, Some(JobState::Running), JobVisibility::Visible)
        .await?
        .into_iter()
        .filter(|job| job.kind == JobKind::AgentTask)
    {
        let delivered = agent_task_delivery_children(ctx, &job)
            .await?
            .into_iter()
            .any(|child| child.state == JobState::Complete);
        if delivered {
            // The reply genuinely reached its target before the restart; the
            // completed delivery child is the evidence.
            let mut completed = job.clone();
            completed.mark_complete();
            completed.metadata.agent_task_mut().outcome = AgentTaskOutcome::ResponseSubmitted;
            ctx.store.update_job(&completed).await?;
            recovered.push(json!({
                "dispatched": true,
                "job": completed.to_value(),
                "recovered": true,
            }));
            continue;
        }
        let mut interrupted = job.clone();
        interrupted.set_state(JobState::Failed);
        let error_text = "agent task was interrupted by runtime restart".to_string();
        interrupted.metadata.error = error_text.clone();
        interrupted.metadata.agent_task_mut().outcome = AgentTaskOutcome::Interrupted;
        interrupted.metadata.agent_task_mut().dispatch_error = error_text;
        ctx.store.update_job(&interrupted).await?;
        recovered.push(json!({
            "dispatched": false,
            "job": interrupted.to_value(),
            "interrupted": true,
        }));
    }
    Ok(recovered)
}

/// How long a finished agent run waits for its text delivery child before
/// the outcome is classified from the raw response text. The `clankcord
/// responses send` POST normally lands while the agent process is still
/// running, so this deadline is only reached when the agent never submitted.
const AWAIT_DELIVERY_GRACE_SECONDS: i64 = 30;
const AWAIT_DELIVERY_POLL_MS: i64 = 2000;

/// The agent task state machine. The phase register is explicit and
/// persisted: `Dispatch` runs the agent subprocess; `AwaitDelivery` waits
/// (bounded) for the reply to arrive as a text delivery child. Typing
/// indicators are lineage-only children and never gate completion.
pub(crate) async fn dispatch_claimed_agent_task_job(ctx: &Ctx, job: Job) -> Result<Value> {
    let job_id = job.id.clone();
    let latest = ctx.store.get_job(&job_id).await?;
    let task = latest
        .metadata
        .agent_task()
        .cloned()
        .unwrap_or_else(AgentTaskMetadata::default);
    match task.phase {
        AgentTaskPhase::Dispatch => run_agent_task_dispatch_phase(ctx, latest, task).await,
        AgentTaskPhase::AwaitDelivery => resolve_agent_task_delivery(ctx, latest, task).await,
    }
}

async fn run_agent_task_dispatch_phase(
    ctx: &Ctx,
    latest: Job,
    task: AgentTaskMetadata,
) -> Result<Value> {
    let job_id = latest.id.clone();
    let attempts = task.dispatch_attempts;
    if attempts >= 3 {
        let mut failed = latest.clone();
        failed.set_state(JobState::Failed);
        failed.metadata.error = "agent task dispatch attempts exhausted".to_string();
        ctx.store.update_job(&failed).await?;
        return Ok(json!({
            "dispatched": false,
            "job": failed.to_value(),
            "reason": "agent task dispatch attempts exhausted",
        }));
    }
    submit_lineage_typing_job(ctx, &latest, DiscordTypingAction::Start, attempts).await;
    let dispatch = dispatch_agent_task(ctx, &latest).await;
    submit_lineage_typing_job(ctx, &latest, DiscordTypingAction::Stop, attempts).await;
    match dispatch {
        Ok(mut dispatched_task) => {
            let mut prepared = ctx.store.get_job(&job_id).await?;
            dispatched_task.phase = AgentTaskPhase::AwaitDelivery;
            dispatched_task.await_delivery_until = isoformat_z(Some(
                utc_now() + chrono::Duration::seconds(AWAIT_DELIVERY_GRACE_SECONDS),
            ));
            dispatched_task.dispatch_attempts = attempts;
            prepared.metadata.set_agent_task(dispatched_task.clone());
            ctx.store.update_job(&prepared).await?;
            resolve_agent_task_delivery(ctx, prepared, dispatched_task).await
        }
        Err(error) => {
            let preflight = error
                .downcast_ref::<AgentInfrastructureError>()
                .and_then(AgentInfrastructureError::preflight)
                .cloned();
            if let Some(preflight) = preflight {
                let mut failed = ctx.store.get_job(&job_id).await?;
                failed.metadata.agent_task_mut().preflight = Some(preflight);
                ctx.store.update_job(&failed).await?;
            }
            fail_agent_task_job(ctx, job_id, attempts, error).await
        }
    }
}

/// The rendezvous with the loopback reply. The delivery arrives as a child
/// of this task (created by the /v1/responses handler), so the standard
/// parent/child resolution machinery drives resumption; the poll below is
/// only the bounded fallback for an agent that never submitted.
async fn resolve_agent_task_delivery(
    ctx: &Ctx,
    latest: Job,
    task: AgentTaskMetadata,
) -> Result<Value> {
    let job_id = latest.id.clone();
    if latest.cancel_requested() {
        return cancel_agent_task_job(ctx, latest).await;
    }
    let deliveries = agent_task_delivery_children(ctx, &latest).await?;
    if deliveries
        .iter()
        .any(|child| child.state == JobState::Complete)
    {
        let mut completed = latest;
        completed.metadata.agent_task_mut().outcome = AgentTaskOutcome::ResponseSubmitted;
        completed.mark_complete();
        ctx.store.update_job(&completed).await?;
        return Ok(json!({
            "dispatched": true,
            "job": completed.to_value(),
            "outcome": AgentTaskOutcome::ResponseSubmitted.as_str(),
        }));
    }
    if deliveries.iter().any(|child| !child.state.is_terminal()) {
        return dispatcher::wait_dispatched_job(ctx, &job_id, Vec::new()).await;
    }
    let deadline = parse_instant(&task.await_delivery_until);
    if deadline.is_some_and(|deadline| utc_now() < deadline) {
        let mut polling = latest;
        polling.set_state(JobState::Queued);
        polling.next_run_at = Some(isoformat_z(Some(
            utc_now() + chrono::Duration::milliseconds(AWAIT_DELIVERY_POLL_MS),
        )));
        ctx.store.update_job(&polling).await?;
        return Ok(json!({
            "dispatched": true,
            "job": polling.to_value(),
            "awaiting_delivery": true,
        }));
    }
    if !task.dispatch_error.trim().is_empty() {
        return fail_agent_task_job(
            ctx,
            job_id,
            task.dispatch_attempts,
            anyhow::anyhow!(task.dispatch_error),
        )
        .await;
    }
    let outcome = classify_undelivered_agent_response(&task.response_text);
    complete_agent_task_without_delivery(ctx, latest, outcome).await
}

fn classify_undelivered_agent_response(response_text: &str) -> AgentTaskOutcome {
    let response_text = response_text.trim();
    if response_text == "RESPONSE_SUBMITTED" {
        return AgentTaskOutcome::SubmittedWithoutDelivery;
    }
    if agent_task_no_response_reason(response_text).is_some() {
        return AgentTaskOutcome::NoResponseNeeded;
    }
    if response_text.is_empty() {
        return AgentTaskOutcome::EmptyResponse;
    }
    AgentTaskOutcome::FinalTextSuppressed
}

async fn cancel_agent_task_job(ctx: &Ctx, mut latest: Job) -> Result<Value> {
    let cancelled_at = non_empty(
        latest.cancelled_at.clone().unwrap_or_default(),
        isoformat_z(None),
    );
    latest.mark_cancelled();
    latest.cancelled_at = Some(cancelled_at);
    latest.completed_at = Some(isoformat_z(None));
    latest.metadata.agent_task_mut().result_suppressed = true;
    ctx.store.update_job(&latest).await?;
    ctx.store
        .append_scope_event(
            &latest.scope(),
            json!({
                "event_kind": "agent_task_result_suppressed",
                "kind": "agent_task_result_suppressed",
                "job_id": latest.id.clone(),
                "job_kind": latest.kind.as_str(),
                "reason": "job was cancelled before the agent task result was posted",
            }),
        )
        .await?;
    Ok(json!({"dispatched": true, "job": latest.to_value(), "cancelled": true}))
}

async fn complete_agent_task_without_delivery(
    ctx: &Ctx,
    mut job: Job,
    outcome: AgentTaskOutcome,
) -> Result<Value> {
    job.mark_complete();
    job.metadata.agent_task_mut().outcome = outcome;
    job.metadata.agent_task_mut().result_suppressed = true;
    ctx.store.update_job(&job).await?;
    ctx.store
        .append_scope_event(
            &job.scope(),
            json!({
                "event_kind": "agent_task_result_suppressed",
                "kind": "agent_task_result_suppressed",
                "job_id": job.id.clone(),
                "job_kind": job.kind.as_str(),
                "reason": outcome.as_str(),
            }),
        )
        .await?;
    Ok(json!({
        "dispatched": true,
        "job": job.to_value(),
        "outcome": outcome.as_str(),
    }))
}

/// Typing indicators carry lineage (parent_job_id) but no dependency edge:
/// they never park the parent and never gate its completion.
async fn submit_lineage_typing_job(
    ctx: &Ctx,
    parent: &Job,
    action: DiscordTypingAction,
    attempts: i64,
) {
    let mut typing = agent_task_typing_job(parent, action, attempts);
    if let Err(error) = typing.attach_to_parent(parent) {
        log(&format!(
            "typing indicator lineage attach failed for {}: {error}",
            parent.id
        ));
        return;
    }
    if let Err(error) = ctx.store.create_job(typing).await {
        log(&format!(
            "typing indicator submission failed for {}: {error}",
            parent.id
        ));
    }
}

async fn agent_task_delivery_children(ctx: &Ctx, job: &Job) -> Result<Vec<Job>> {
    Ok(ctx
        .store
        .list_child_jobs(&job.id)
        .await?
        .into_iter()
        .filter(|child| child.kind == JobKind::TextDelivery)
        .collect())
}

async fn dispatch_agent_task(ctx: &Ctx, job: &Job) -> Result<AgentTaskMetadata> {
    let latest = ctx.store.get_job(&job.id).await?;
    validate_agent_task_job(&latest)?;
    if latest.cancel_requested() {
        anyhow::bail!("agent task was cancelled before the agent process started");
    }

    let workdir = agent_task_workdir(&latest);
    fs::create_dir_all(&workdir)?;
    let repo_dir = agent_repo_dir();
    let agent_env = agent_task_env(&latest, &workdir, repo_dir.as_ref())?;
    let preflight = run_agent_task_preflight(Some(&agent_env));
    if !preflight.ok {
        let detail = preflight.failed_check_summary();
        return Err(AgentInfrastructureError::with_preflight(
            format!("agent task preflight failed: {detail}"),
            preflight,
        )
        .into());
    }

    let job_dir = ctx
        .store
        .channel_dir(&latest.guild_id, &latest.scope_id)
        .join("jobs");
    fs::create_dir_all(&job_dir)?;

    let prompt_path = job_dir.join(format!("{}.agent-prompt.txt", latest.id));
    let result_path = job_dir.join(format!("{}.agent-result.txt", latest.id));
    let raw_result_path = job_dir.join(format!("{}.codex.jsonl", latest.id));
    let agent_session_id = agent_task_session_id(&latest)?;
    let agent_session = ctx
        .store
        .get_agent_session_record(&agent_session_id)
        .await?;
    let prior_session_id = non_empty(
        latest
            .metadata
            .agent_task()
            .map(|task| task.agent.session_id.clone())
            .unwrap_or_default(),
        agent_session.codex_session_id.clone(),
    );
    let include_master_prompt = prior_session_id.trim().is_empty();
    let prompt =
        build_agent_task_message_for_job(ctx, &latest, &workdir, include_master_prompt).await?;
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
    ctx.store.update_job(&prepared).await?;
    let invocation = AgentRuntime::default().invoke(AgentInvocationRequest {
        role: AgentRole::Task,
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

    append_agent_invocation_warning_events(
        ctx,
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
    let completed_session = agent_sessions::set_agent_session_codex_session(
        ctx,
        &agent_session_id,
        invocation.session_id.clone(),
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

async fn fail_agent_task_job(
    ctx: &Ctx,
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
    let mut latest = ctx.store.get_job(&job_id).await?;
    if latest.cancel_requested() {
        let cancelled_at = non_empty(
            latest.cancelled_at.clone().unwrap_or_default(),
            isoformat_z(None),
        );
        latest.mark_cancelled();
        latest.cancelled_at = Some(cancelled_at);
        latest.metadata.agent_task_mut().dispatch_error_after_cancel = error_text;
        ctx.store.update_job(&latest).await?;
        return Ok(json!({"dispatched": false, "job": latest.to_value(), "cancelled": true}));
    }
    let submitted_text_deliveries = agent_task_delivery_children(ctx, &latest).await?;
    if !submitted_text_deliveries.is_empty() {
        latest.mark_complete();
        latest.metadata.agent_task_mut().outcome = AgentTaskOutcome::ResponseSubmitted;
        latest.metadata.agent_task_mut().dispatch_error = error_text.clone();
        ctx.store.update_job(&latest).await?;
        return Ok(json!({
            "dispatched": true,
            "job": latest.to_value(),
            "submitted_text_deliveries": submitted_text_deliveries.into_iter().map(|job| job.to_value()).collect::<Vec<_>>(),
            "error_after_response": error_text,
        }));
    }
    if let Some(preflight) = infrastructure_error.and_then(AgentInfrastructureError::preflight) {
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
        agent_unavailable_text_delivery_job(ctx, &latest).await?
    } else {
        None
    };
    ctx.store.update_job(&latest).await?;
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

async fn agent_unavailable_text_delivery_job(ctx: &Ctx, job: &Job) -> Result<Option<Job>> {
    if !agent_task_delivery_children(ctx, job).await?.is_empty() {
        return Ok(None);
    }
    let requested_by_user_id = agent_task_requester_id(job);
    let mut response = Job::text_delivery(
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
    response.attach_to_parent(job)?;
    ctx.store.create_job(response).await.map(Some)
}

async fn append_agent_invocation_warning_events(
    ctx: &Ctx,
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
        ctx.store
            .append_scope_event(
                &job.scope(),
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
        .replace([' ', '-'], "_")
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

async fn build_agent_task_message_for_job(
    ctx: &Ctx,
    job: &Job,
    workdir: &std::path::Path,
    include_master_prompt: bool,
) -> Result<String> {
    let context = agent_task_prompt_context(ctx, job, workdir).await?;
    build_agent_task_message_for_session(&context, include_master_prompt)
}

async fn agent_task_prompt_context(
    ctx: &Ctx,
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
    let source_events = agent_task_source_events(ctx, &source_event_ids).await?;
    let end = parse_instant(&job.created_at).unwrap_or_else(utc_now);
    let start = end - chrono::Duration::minutes(5);
    let speech_kinds = set(["speech_segment", "transcript", "discord_text_message"]);
    let events = ctx
        .store
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
            "[{}] {}: {}",
            job.created_at,
            non_empty(requested_by.clone(), "requester".to_string()),
            request
        ));
    }
    let agent_session_id = agent_task_session_id(job)?;
    let agent_session = ctx
        .store
        .get_agent_session_record(&agent_session_id)
        .await?;
    let parent = agent_task_parent_job(ctx, job).await?;
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
    ctx: &Ctx,
    source_event_ids: &std::collections::BTreeSet<String>,
) -> Result<Vec<Value>> {
    let mut events = Vec::new();
    for event_id in source_event_ids {
        events.push(ctx.store.get_event(event_id).await?);
    }
    Ok(events)
}

async fn agent_task_parent_job(ctx: &Ctx, job: &Job) -> Result<Option<Job>> {
    let Some(parent_job_id) = job.parent_job_id.as_deref() else {
        return Ok(None);
    };
    Ok(Some(ctx.store.get_job(parent_job_id).await?))
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
    let crate::model::job::JobPayload::AgentTask(payload) = &job.payload else {
        anyhow::bail!("job {} is not an agent task", job.id);
    };
    if payload.agent_session_id.trim().is_empty() {
        anyhow::bail!("agent task job {} is missing agent_session_id", job.id);
    }
    Ok(payload.agent_session_id.clone())
}

pub fn agent_task_workdir(job: &Job) -> PathBuf {
    let agent_session_id = match &job.payload {
        crate::model::job::JobPayload::AgentTask(payload) => payload.agent_session_id.clone(),
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
) -> Result<BTreeMap<String, String>> {
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
    insert_linear_mcp_env(&mut vars)?;
    Ok(vars)
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
    let mut checks: Vec<Vec<String>> = vec![
        vec![codex_bin, "--version".to_string()],
        vec!["rg".to_string(), "--version".to_string()],
        vec!["jq".to_string(), "--version".to_string()],
        vec!["clang".to_string(), "--version".to_string()],
        vec!["python".to_string(), "--version".to_string()],
        vec!["zip".to_string(), "--version".to_string()],
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
            "coding".to_string(),
            "spec".to_string(),
        ],
        vec![
            "clankcord".to_string(),
            "automations".to_string(),
            "create".to_string(),
            "--help".to_string(),
        ],
    ];
    if config::codex_linear_mcp_enabled() {
        let mut command = vec![config::codex_bin()];
        command.extend(codex_linear_mcp_config_args());
        command.extend(["mcp".to_string(), "list".to_string(), "--json".to_string()]);
        checks.push(command);
    }
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
    let mut ids = command
        .arguments
        .source_event_ids
        .iter()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    if let Some(activation) = &command.arguments.activation {
        ids.extend(activation.source_event_ids.iter().cloned());
        for value in [&activation.wake_event_id, &activation.latest_wake_event_id] {
            if !value.is_empty() {
                ids.insert(value.clone());
            }
        }
    }
    ids
}

fn agent_task_request_origin(
    command: Option<&crate::model::job::CommandRequest>,
    route_kind: &crate::model::agents::AgentSessionRouteKind,
    source_events: &[Value],
    parent: Option<&Job>,
) -> AgentPromptRequestOrigin {
    if command
        .map(|command| command.arguments.activation.is_some())
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
    if *route_kind == crate::model::agents::AgentSessionRouteKind::Dm {
        return AgentPromptRequestOrigin::Text;
    }
    if parent.is_some_and(|job| {
        matches!(
            &job.payload,
            crate::model::job::JobPayload::AgentSessionResume(payload)
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
    format!("[{timestamp}] {speaker}: {text}")
}
