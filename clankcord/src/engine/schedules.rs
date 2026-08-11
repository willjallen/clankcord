//! The engine side of recurring work.
//!
//! Every recurring unit of work in the system is one `job_schedules` row —
//! there is no other clock. The dispatch loop calls [`run_due_schedules`]
//! each pass; due rows are claimed atomically and their jobs submitted
//! through the bus like any other work. Adding a new automated task
//! (conversation summaries, a memory backend sync, any cron-style job) is:
//! declare a `JobKind` with its spec and handler, teach
//! [`schedule_job`] to build its payload, and upsert a schedule row.

use serde_json::{Value, json};

use crate::Result;
use crate::config;
use crate::engine::JobBus;
use crate::model::job::{Job, JobKind};
use crate::store::TimelineStore;
use crate::time::{instant_ms_dt, utc_now};
use crate::util::log;

/// Builds the job a schedule row mints. Only kinds that make sense on a
/// clock are constructible here; asking for anything else is a declaration
/// error and fails loudly.
pub fn schedule_job(schedule: &crate::store::JobScheduleRow) -> Result<Job> {
    let kind: JobKind = schedule.kind.parse()?;
    let payload = &schedule.payload_json;
    let job = match kind {
        JobKind::RuntimeMaintenance => Job::runtime_maintenance(schedule.interval_ms),
        JobKind::VoiceStatusSync => Job::voice_status_sync(schedule_source(schedule)),
        JobKind::AutomationEvaluation => Job::automation_evaluation(schedule_source(schedule)),
        JobKind::AgentSessionRetirement => Job::agent_session_retirement(schedule_source(schedule)),
        JobKind::StaleWakeProbeSweep => Job::stale_wake_probe_sweep(
            schedule_source(schedule),
            payload
                .get("max_age_seconds")
                .and_then(Value::as_i64)
                .unwrap_or_else(config::wake_probe_max_queue_age_seconds),
        ),
        JobKind::EphemeralJobGc => Job::ephemeral_job_gc(
            schedule_source(schedule),
            payload
                .get("batch_limit")
                .and_then(Value::as_u64)
                .unwrap_or_else(|| config::ephemeral_job_gc_batch_limit() as u64)
                as usize,
        ),
        JobKind::MemberSync => {
            let Some(guild_id) = payload.get("guild_id").and_then(Value::as_str) else {
                anyhow::bail!(
                    "schedule {} requires payload.guild_id for member_sync",
                    schedule.schedule_id
                );
            };
            Job::member_sync(guild_id)
        }
        other => anyhow::bail!(
            "job kind {other} is not schedulable; declare it in engine::schedules::schedule_job"
        ),
    };
    Ok(job)
}

/// Claims and submits every due schedule. Called from the dispatch loop;
/// one failed schedule is reported and does not block the others.
pub async fn run_due_schedules(store: &TimelineStore, bus: &JobBus) -> Result<Vec<Value>> {
    let now_ms = instant_ms_dt(utc_now());
    let due = store.claim_due_job_schedules(now_ms).await?;
    let mut submitted = Vec::new();
    for schedule in due {
        let job = match schedule_job(&schedule) {
            Ok(job) => job,
            Err(error) => {
                log(&format!(
                    "schedule {} failed to build its job: {error}",
                    schedule.schedule_id
                ));
                submitted.push(json!({
                    "schedule_id": schedule.schedule_id,
                    "error": error.to_string(),
                }));
                continue;
            }
        };
        let job_id = job.id.clone();
        match bus.submit(job).await {
            Ok(_) => {
                store
                    .record_job_schedule_submission(&schedule.schedule_id, &job_id)
                    .await?;
                submitted.push(json!({
                    "schedule_id": schedule.schedule_id,
                    "job_id": job_id,
                    "kind": schedule.kind,
                }));
            }
            Err(error) => {
                log(&format!(
                    "schedule {} failed to submit {job_id}: {error}",
                    schedule.schedule_id
                ));
                submitted.push(json!({
                    "schedule_id": schedule.schedule_id,
                    "error": error.to_string(),
                }));
            }
        }
    }
    Ok(submitted)
}

/// Declares the built-in schedules. Runs at boot; intervals follow config.
/// Rows keep their cadence position across restarts.
pub async fn ensure_default_schedules(store: &TimelineStore) -> Result<()> {
    let maintenance_ms = config::runtime_maintenance_interval_ms();
    for (id, kind, payload) in [
        (
            "runtime_maintenance",
            JobKind::RuntimeMaintenance,
            json!({}),
        ),
        ("voice_status_sync", JobKind::VoiceStatusSync, json!({})),
        (
            "automation_evaluation",
            JobKind::AutomationEvaluation,
            json!({}),
        ),
        (
            "agent_session_retirement",
            JobKind::AgentSessionRetirement,
            json!({}),
        ),
        (
            "stale_wake_probe_sweep",
            JobKind::StaleWakeProbeSweep,
            json!({}),
        ),
        ("ephemeral_job_gc", JobKind::EphemeralJobGc, json!({})),
    ] {
        store
            .upsert_job_schedule(id, kind.as_str(), &payload, maintenance_ms, true)
            .await?;
    }
    Ok(())
}

fn schedule_source(schedule: &crate::store::JobScheduleRow) -> String {
    format!("schedule:{}", schedule.schedule_id)
}
