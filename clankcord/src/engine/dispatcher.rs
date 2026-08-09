use serde_json::{Value, json};

use crate::Result;
use crate::domain::Ctx;
use crate::domain::voice_capture::segments;
use crate::engine::JobDecision;
use crate::model::job::{Job, JobKind, JobOutput, JobState};
use crate::ports::discord::DiscordApi;

use crate::domain::interactions::tasks;
use crate::domain::interactions::thread_titles;
use crate::engine::routes;
use crate::store;
use crate::util;

pub async fn dispatch_claimed_runtime_job<A>(
    ctx: &Ctx,
    external_api: &A,
    running: Job,
) -> Result<Value>
where
    A: DiscordApi,
{
    let job_id = running.id.clone();
    match routes::execute(ctx, &running, external_api).await {
        Ok(decision) => apply_job_decision(ctx, &job_id, running, decision).await,
        Err(error) => fail_dispatched_job(ctx, &job_id, running, error).await,
    }
}

pub async fn dispatch_claimed_blocking_job(ctx: &Ctx, running: Job) -> Result<Value> {
    let job_id = running.id.clone();
    match running.kind {
        JobKind::WakeProbe => match routes::execute_wake_probe(ctx, &running).await {
            Ok(result) => complete_dispatched_job(ctx, &job_id, running, result).await,
            Err(error) => fail_dispatched_job(ctx, &job_id, running, error).await,
        },
        JobKind::AudioSegment => match routes::execute_audio_segment(ctx, &running).await {
            Ok(result) => complete_dispatched_job(ctx, &job_id, running, result).await,
            Err(error) if segments::is_retryable_audio_segment_error(&error) => {
                let retry = segments::retry_plan(error);
                requeue_dispatched_job(
                    ctx,
                    &job_id,
                    running,
                    retry.delay_for_attempt,
                    retry.error,
                    retry.log_prefix,
                )
                .await
            }
            Err(error) => fail_dispatched_job(ctx, &job_id, running, error).await,
        },
        JobKind::TranscriptionMux => match routes::execute_transcription_mux(ctx, &running).await {
            Ok(result) => complete_dispatched_job(ctx, &job_id, running, result).await,
            Err(error) if segments::is_retryable_audio_segment_error(&error) => {
                let retry = segments::retry_plan(error);
                requeue_dispatched_job(
                    ctx,
                    &job_id,
                    running,
                    retry.delay_for_attempt,
                    retry.error,
                    retry.log_prefix,
                )
                .await
            }
            Err(error) => fail_dispatched_job(ctx, &job_id, running, error).await,
        },
        JobKind::AgentTask => tasks::dispatch_claimed_agent_task_job(ctx, running).await,
        JobKind::AgentThreadTitleRefresh => {
            let decision = match &running.payload {
                crate::model::job::JobPayload::AgentThreadTitleRefresh(payload) => {
                    thread_titles::prepare_agent_thread_title_refresh_job(ctx, &running, payload)
                        .await
                }
                payload => anyhow::bail!(
                    "job kind {} has unexpected payload {}",
                    running.kind,
                    payload.kind()
                ),
            };
            match decision {
                Ok(decision) => apply_job_decision(ctx, &job_id, running, decision).await,
                Err(error) => fail_dispatched_job(ctx, &job_id, running, error).await,
            }
        }
        kind => anyhow::bail!("job kind {kind} is not handled by blocking dispatcher"),
    }
}

pub(crate) async fn apply_job_decision(
    ctx: &Ctx,
    job_id: &str,
    fallback_job: Job,
    decision: JobDecision,
) -> Result<Value> {
    match decision {
        JobDecision::Complete(output) => {
            complete_dispatched_job(ctx, job_id, fallback_job, output).await
        }
        JobDecision::Fail(failure) => {
            fail_dispatched_job(ctx, job_id, fallback_job, anyhow::anyhow!(failure.message)).await
        }
        JobDecision::Wait => wait_dispatched_job(ctx, job_id, fallback_job, Vec::new()).await,
        JobDecision::WaitFor(children) => {
            wait_dispatched_job(ctx, job_id, fallback_job, children).await
        }
    }
}

pub(crate) async fn complete_dispatched_job(
    ctx: &Ctx,
    job_id: &str,
    fallback_job: Job,
    output: JobOutput,
) -> Result<Value> {
    let mut latest = match ctx.store.get_job(job_id).await {
        Ok(job) => job,
        Err(_) => fallback_job.clone(),
    };
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
    fallback_job: Job,
    children: Vec<Job>,
) -> Result<Value> {
    let latest = match ctx.store.get_job(job_id).await {
        Ok(job) => job,
        Err(_) => fallback_job.clone(),
    };
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
    fallback_job: Job,
    error: anyhow::Error,
) -> Result<Value> {
    let error_text = error.to_string();
    let mut latest = match ctx.store.get_job(job_id).await {
        Ok(job) => job,
        Err(_) => fallback_job.clone(),
    };
    latest.set_state(JobState::Failed);
    latest.metadata.error = error_text.clone();
    ctx.store.update_job(&latest).await?;
    util::log(&format!("job dispatch failed {job_id}: {error_text}"));
    Ok(json!({"dispatched": false, "job": latest.to_value(), "error": error_text}))
}

pub(crate) async fn requeue_dispatched_job(
    ctx: &Ctx,
    job_id: &str,
    fallback_job: Job,
    delay_for_attempt: fn(i64) -> chrono::Duration,
    error_text: String,
    log_prefix: &'static str,
) -> Result<Value> {
    let mut latest = match ctx.store.get_job(job_id).await {
        Ok(job) => job,
        Err(_) => fallback_job.clone(),
    };
    latest.attempts = latest.attempts.saturating_add(1);
    latest.set_state(JobState::Queued);
    latest.started_at = None;
    latest.completed_at = None;
    let delay = delay_for_attempt(latest.attempts);
    latest.next_run_at = Some(crate::store::isoformat_z(Some(
        crate::store::utc_now() + delay,
    )));
    latest.metadata.error = error_text.clone();
    ctx.store.update_job(&latest).await?;
    util::log(&format!(
        "{log_prefix} {job_id}: attempt {} next_run_at {} error: {error_text}",
        latest.attempts,
        latest.next_run_at.clone().unwrap_or_default()
    ));
    Ok(json!({
        "dispatched": false,
        "retry_scheduled": true,
        "job": latest.to_value(),
        "error": error_text,
    }))
}
