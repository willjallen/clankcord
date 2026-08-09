use serde_json::{Value, json};

use crate::Result;
use crate::config;
use crate::domain::Ctx;
use crate::domain::automations::engine;
use crate::domain::children;
use crate::domain::interactions::thread_titles;
use crate::domain::maintenance::STALE_RUNNING_JOB_TIMEOUT_MINUTES;
use crate::domain::maintenance::voice_status;
use crate::domain::transcription::mux;
use crate::engine::JobDecision;
use crate::model::job::{
    Job, JobKind, JobOutput, JobState, OpaqueValue, RuntimeMaintenancePayload,
};
use crate::store::{JobVisibility, isoformat_z, parse_instant, utc_now};

pub(crate) async fn execute_runtime_maintenance_job(
    ctx: &Ctx,
    job: &Job,
    _payload: &RuntimeMaintenancePayload,
) -> Result<JobDecision> {
    let mut submitted = Vec::new();
    for definition_job in thread_titles::agent_thread_title_refresh_jobs(ctx, job).await? {
        let created = ctx.store.create_job(definition_job).await?;
        submitted.push(json!({
            "definition": "agent_thread_title_refresh",
            "job_id": created.id,
            "job_kind": created.kind.as_str(),
        }));
    }
    let requeued_audio_segments = ctx
        .store
        .requeue_failed_audio_segment_jobs(config::failed_audio_segment_retry_batch_limit())
        .await?;
    let recovered_transcription_slots = ctx.store.recover_abandoned_transcription_slots().await?;
    let requeued_transcription_slots = ctx
        .store
        .requeue_retryable_failed_transcription_slots(
            config::failed_audio_segment_retry_batch_limit(),
        )
        .await?;
    let transcription_mux_plan_jobs = mux::ensure_transcription_mux_plan_jobs_for_queued_slots(
        ctx,
        config::transcription_mux_batch_delay_ms(),
    )
    .await?;
    Ok(JobDecision::Complete(JobOutput::from_boundary_json(
        &json!({
            "kind": "runtime_maintenance",
            "submitted_jobs": submitted,
            "requeued_audio_segments": requeued_audio_segments,
            "recovered_transcription_slots": recovered_transcription_slots,
            "requeued_transcription_slots": requeued_transcription_slots,
            "transcription_mux_plan_jobs": transcription_mux_plan_jobs
                .iter()
                .map(|job| job.to_value())
                .collect::<Vec<_>>(),
        }),
    )?))
}

pub(crate) async fn execute_voice_status_sync_job(ctx: &Ctx, job: &Job) -> Result<JobDecision> {
    let children =
        match children::await_children(ctx, &job.id, "voice status snapshot dependency").await? {
            crate::domain::children::ChildResolution::Pending => {
                return Ok(JobDecision::Wait);
            }
            crate::domain::children::ChildResolution::Failed { message, .. } => {
                return Ok(JobDecision::fail(message));
            }
            crate::domain::children::ChildResolution::Settled(children) => children,
        };
    if let Some(snapshot_job) = children
        .iter()
        .find(|child| child.kind == JobKind::DiscordVoiceStatusSnapshot)
    {
        let Some(JobOutput::DiscordVoiceStatusSnapshot(output)) =
            snapshot_job.metadata.output.clone()
        else {
            return Ok(JobDecision::fail(format!(
                "voice status snapshot child {} completed without snapshot output",
                snapshot_job.id
            )));
        };
        let bot_count = output.bots.len();
        let session_count = output.sessions.len();
        let voice_states = output
            .voice_states
            .iter()
            .map(OpaqueValue::to_json)
            .collect::<Vec<_>>();
        let voice_state_count = voice_states.len();
        let voice_state_guild_count = output.voice_state_guild_ids.len();
        voice_status::sync_voice_adapter_status(
            ctx,
            output.bots,
            output.sessions,
            output.voice_state_guild_ids,
            voice_states,
        )
        .await?;
        return Ok(JobDecision::Complete(JobOutput::from_boundary_json(
            &json!({
                "kind": "voice_status_sync",
                "snapshot_job_id": snapshot_job.id,
                "bot_count": bot_count,
                "session_count": session_count,
                "voice_state_guild_count": voice_state_guild_count,
                "voice_state_count": voice_state_count,
            }),
        )?));
    }
    Ok(JobDecision::WaitFor(vec![
        Job::discord_voice_status_snapshot(job.id.clone()),
    ]))
}

pub(crate) async fn execute_automation_evaluation_job(
    ctx: &Ctx,
    _job: &Job,
) -> Result<JobDecision> {
    let run = engine::run_automations(ctx).await?;
    Ok(JobDecision::Complete(JobOutput::from_boundary_json(
        &json!({
            "kind": "automation_evaluation",
            "result": run.to_json(),
        }),
    )?))
}

pub(crate) async fn execute_stale_wake_probe_sweep_job(
    ctx: &Ctx,
    max_age_seconds: i64,
) -> Result<JobDecision> {
    let cancelled = ctx
        .store
        .cancel_stale_wake_probe_jobs(max_age_seconds)
        .await?;
    Ok(JobDecision::Complete(JobOutput::from_boundary_json(
        &json!({
            "kind": "stale_wake_probe_sweep",
            "max_age_seconds": max_age_seconds,
            "jobs": cancelled,
        }),
    )?))
}

pub(crate) async fn execute_ephemeral_job_gc_job(
    ctx: &Ctx,
    batch_limit: usize,
) -> Result<JobDecision> {
    let result = ctx
        .store
        .garbage_collect_ephemeral_jobs(batch_limit)
        .await?;
    Ok(JobDecision::Complete(JobOutput::from_boundary_json(
        &json!({
            "kind": "ephemeral_job_gc",
            "result": result,
        }),
    )?))
}

pub async fn recover_stale_running_jobs_for_maintenance_pass(ctx: &Ctx) -> Result<Vec<Value>> {
    fail_stale_running_jobs(ctx, STALE_RUNNING_JOB_TIMEOUT_MINUTES).await
}

async fn fail_stale_running_jobs(ctx: &Ctx, timeout_minutes: i64) -> Result<Vec<Value>> {
    let timeout = chrono::Duration::minutes(timeout_minutes);
    let now = utc_now();
    let mut timed_out = Vec::new();
    for mut job in ctx
        .store
        .list_jobs_with_visibility(
            None,
            Some(JobState::Running),
            JobVisibility::IncludeEphemeral,
        )
        .await?
    {
        if job.kind == JobKind::AgentTask {
            continue;
        }
        let updated_at = parse_instant(&job.updated_at);
        if updated_at
            .map(|value| now - value < timeout)
            .unwrap_or(false)
        {
            continue;
        }
        job.set_state(JobState::FailedTimeout);
        job.metadata.error = "job exceeded stale running-job timeout".to_string();
        job.metadata.timed_out_at = isoformat_z(None);
        ctx.store.update_job(&job).await?;
        timed_out.push(job.to_value());
    }
    Ok(timed_out)
}
