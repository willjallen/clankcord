use serde_json::{Value, json};

use crate::Result;
use crate::domain::Ctx;
use crate::domain::interactions::tasks;
use crate::domain::transcription::execution as transcription_execution;
use crate::engine::JobDecision;
use crate::engine::routes;
use crate::engine::routes::Routed;
use crate::model::job::{Job, JobOutput, JobState};
use crate::ports::discord::DiscordApi;
use crate::util;

/// Finalization policy for every routed execution. Routing itself is the
/// one exhaustive payload match in [`routes::route`].
pub async fn dispatch_claimed_job<A>(ctx: &Ctx, external_api: &A, running: Job) -> Result<Value>
where
    A: DiscordApi,
{
    let job_id = running.id.clone();
    match routes::route(ctx, &running, external_api).await {
        Routed::Decision(Ok(decision)) => apply_job_decision(ctx, &job_id, decision).await,
        Routed::Decision(Err(error)) => fail_dispatched_job(ctx, &job_id, error).await,
        Routed::Output(Ok(output)) => complete_dispatched_job(ctx, &job_id, output).await,
        Routed::Output(Err(error)) => fail_dispatched_job(ctx, &job_id, error).await,
        Routed::SttOutput(Ok(output)) => complete_dispatched_job(ctx, &job_id, output).await,
        Routed::SttOutput(Err(error))
            if transcription_execution::is_retryable_audio_segment_error(&error) =>
        {
            let retry = transcription_execution::retry_plan(error);
            requeue_dispatched_job(
                ctx,
                &job_id,
                retry.delay_for_attempt,
                retry.error,
                retry.log_prefix,
            )
            .await
        }
        Routed::SttOutput(Err(error)) => fail_dispatched_job(ctx, &job_id, error).await,
        Routed::AgentTask => tasks::dispatch_claimed_agent_task_job(ctx, running).await,
    }
}

pub(crate) async fn apply_job_decision(
    ctx: &Ctx,
    job_id: &str,
    decision: JobDecision,
) -> Result<Value> {
    match decision {
        JobDecision::Complete(output) => complete_dispatched_job(ctx, job_id, output).await,
        JobDecision::Fail(failure) => {
            fail_dispatched_job(ctx, job_id, anyhow::anyhow!(failure.message)).await
        }
        JobDecision::Wait => wait_dispatched_job(ctx, job_id, Vec::new()).await,
        JobDecision::WaitFor(children) => wait_dispatched_job(ctx, job_id, children).await,
    }
}

pub(crate) async fn complete_dispatched_job(
    ctx: &Ctx,
    job_id: &str,
    output: JobOutput,
) -> Result<Value> {
    let mut latest = ctx.store.get_job(job_id).await?;
    latest.metadata.output = Some(output.clone());
    if latest.state != JobState::Waiting
        && latest.state != JobState::Queued
        && latest.state != JobState::ConfirmationPending
    {
        latest.mark_complete();
    }
    ctx.store.update_job(&latest).await?;
    Ok(json!({"dispatched": true, "job": latest.to_value(), "result": output.to_json()}))
}

pub(crate) async fn wait_dispatched_job(
    ctx: &Ctx,
    job_id: &str,
    children: Vec<Job>,
) -> Result<Value> {
    let latest = ctx.store.get_job(job_id).await?;
    let mut child_ids = Vec::new();
    if children.is_empty() {
        let mut waiting = latest.clone();
        if !waiting.state.is_terminal() {
            waiting.mark_waiting();
            ctx.store.update_job(&waiting).await?;
        }
    } else {
        for child in children {
            let child = ctx.store.create_child_job(&latest, child).await?;
            child_ids.push(child.id);
        }
    }
    Ok(json!({"dispatched": true, "waiting": true, "job_id": job_id, "child_job_ids": child_ids}))
}

pub(crate) async fn fail_dispatched_job(
    ctx: &Ctx,
    job_id: &str,
    error: anyhow::Error,
) -> Result<Value> {
    let error_text = error.to_string();
    let mut latest = ctx.store.get_job(job_id).await?;
    latest.set_state(JobState::Failed);
    latest.metadata.error = error_text.clone();
    ctx.store.update_job(&latest).await?;
    util::log(&format!("job dispatch failed {job_id}: {error_text}"));
    Ok(json!({"dispatched": false, "job": latest.to_value(), "error": error_text}))
}

pub(crate) async fn requeue_dispatched_job(
    ctx: &Ctx,
    job_id: &str,
    delay_for_attempt: fn(i64) -> chrono::Duration,
    error_text: String,
    log_prefix: &'static str,
) -> Result<Value> {
    let mut latest = ctx.store.get_job(job_id).await?;
    latest.attempts = latest.attempts.saturating_add(1);
    latest.set_state(JobState::Queued);
    latest.started_at = None;
    latest.completed_at = None;
    let delay = delay_for_attempt(latest.attempts);
    latest.next_run_at = Some(crate::time::utc_now() + delay);
    latest.metadata.error = error_text.clone();
    ctx.store.update_job(&latest).await?;
    util::log(&format!(
        "{log_prefix} {job_id}: attempt {} next_run_at {} error: {error_text}",
        latest.attempts,
        latest.next_run_at.unwrap_or_default()
    ));
    Ok(json!({
        "dispatched": false,
        "retry_scheduled": true,
        "job": latest.to_value(),
        "error": error_text,
    }))
}
