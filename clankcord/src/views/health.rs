//! The runtime health rollup: component statuses, capability outcomes, active-job summaries, and the health/summary payloads.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Duration, Utc};
use serde_json::{Value, json};
use sqlx::Row;

use crate::Result;
use crate::config;
use crate::domain::Ctx;
use crate::domain::rooms::status;
use crate::domain::voice::capture::wake_circuit;
use crate::model::job::{Job, JobState};
use crate::store::VOICE_ADAPTER_SNAPSHOT_STATUS_KEY;
use crate::time::{instant_ms_dt, isoformat_z, utc_now};
use crate::util::string_field;
use crate::views::diagnostics::{
    BacklogKindSummary, FAILURE_WINDOW_SECONDS, JobDiagnosticRow, age_seconds, count_pair_rows,
    count_rows, database_diagnostics, json_usize, ms_iso, operational_diagnostics,
    operational_failure_summary, postgres_pool_payload,
};

pub async fn operational_health_payload(ctx: &Ctx) -> Result<Value> {
    Ok(dashboard_summary_payload(ctx)
        .await?
        .get("health")
        .cloned()
        .expect("dashboard summary always contains health"))
}

pub async fn dashboard_summary_payload(ctx: &Ctx) -> Result<Value> {
    let now = utc_now();
    let database = database_health_probe(ctx).await;
    if !database.get("ok").and_then(Value::as_bool).unwrap_or(false) {
        return Ok(json!({
            "generatedAt": isoformat_z(now),
            "health": runtime_health_from_facts(
                &database,
                &RuntimeHealthFacts::default(),
                &unavailable_failure_summary(now),
                &VoiceObservationSummary::default(),
                &json!({"status": "unknown", "available": false, "reason": "database_unavailable"}),
                0,
                0,
                now,
            ),
            "jobs": {"summary": active_job_summary(&[])},
            "operations": {"backlog": active_job_backlog(&[], now)},
        }));
    }

    let (active_jobs, mut health_facts, failures, voice, inventory) = tokio::try_join!(
        active_job_aggregates(ctx, now),
        lean_terminal_health_facts(ctx, now),
        operational_failure_summary(ctx, now, false),
        lean_voice_observation_summary(ctx, now),
        dashboard_inventory_counts(ctx),
    )?;
    apply_active_health_facts(&mut health_facts, &active_jobs, now);
    let (configured_room_count, automation_count) = inventory;
    let wake_provider = wake_circuit::wake_provider_health(&ctx.store).await?;
    Ok(json!({
        "generatedAt": isoformat_z(now),
        "health": runtime_health_from_facts(
            &database,
            &health_facts,
            &failures,
            &voice,
            &wake_provider,
            configured_room_count,
            automation_count,
            now,
        ),
        "jobs": {"summary": active_job_summary(&active_jobs)},
        "operations": {"backlog": active_job_backlog(&active_jobs, now)},
    }))
}

async fn dashboard_health_bundle(ctx: &Ctx) -> Result<(Value, Value)> {
    let now = utc_now();
    let database = database_health_probe(ctx).await;
    if !database.get("ok").and_then(Value::as_bool).unwrap_or(false) {
        let failures = unavailable_failure_summary(now);
        let health = runtime_health(
            &database,
            &[],
            &failures,
            &VoiceObservationSummary::default(),
            &json!({"status": "unknown", "available": false, "reason": "database_unavailable"}),
            0,
            0,
            now,
        );
        return Ok((
            health,
            json!({
                "coverage": {"complete": false},
                "backlog": {},
                "windows": [],
                "latencies": {},
                "failures": failures,
            }),
        ));
    }

    let mut status = status::status_payload(ctx, None).await?;
    let voice = apply_voice_observation_freshness(ctx, &mut status, now).await?;
    let operations = operational_diagnostics(ctx, now).await?;
    let configured_room_count = ctx.store.list_room_configs().await?.len();
    let automation_count = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM automations")
        .fetch_one(&ctx.store.pool)
        .await?;
    let wake_provider = wake_circuit::wake_provider_health(&ctx.store).await?;
    let health = runtime_health(
        &database,
        &operations.job_rows,
        &operations.failure_summary,
        &voice,
        &wake_provider,
        configured_room_count,
        automation_count as usize,
        now,
    );
    Ok((health, operations.payload))
}

pub async fn dashboard_health_payload(
    ctx: &Ctx,
    http_requests: Value,
    process_load: Value,
) -> Result<Value> {
    let now = utc_now();
    let (health, operations) = dashboard_health_bundle(ctx).await?;
    let database = database_diagnostics(ctx).await;
    let active_jobs = ctx
        .store
        .list_jobs_by_states_with_visibility(
            None,
            &[
                JobState::Queued,
                JobState::Running,
                JobState::Waiting,
                JobState::CancelRequested,
                JobState::ConfirmationPending,
            ],
            crate::store::JobVisibility::IncludeEphemeral,
        )
        .await?;
    Ok(json!({
        "generatedAt": isoformat_z(now),
        "health": health,
        "database": database,
        "requests": http_requests,
        "process": {"load": process_load},
        "load": load_payload(&active_jobs, now),
        "operations": operations,
    }))
}

#[derive(Debug, Default)]
struct ScopeJobSummary {
    scope_kind: String,
    guild_id: String,
    scope_id: String,
    total: usize,
    active: usize,
    failed: usize,
    latest_at: String,
}

#[derive(Debug, Default)]
pub(crate) struct VoiceObservationSummary {
    snapshot_at_ms: Option<i64>,
    fresh_for_seconds: i64,
    observed_bots: usize,
    ready_bots: usize,
    gateway_bots: usize,
    active_sessions: usize,
    stale_bots: usize,
    stale_sessions: usize,
}

#[derive(Debug, Clone, Default)]
struct SchedulerHealthFacts {
    latest_job_id: String,
    latest_state: String,
    observed_at_ms: Option<i64>,
    latest_failed: bool,
    oldest_due_seconds: i64,
    oldest_running_seconds: i64,
}

#[derive(Debug, Clone, Default)]
struct CapabilityHealthFacts {
    active: usize,
    terminal: usize,
    completed: usize,
    failed: usize,
    latest_at_ms: Option<i64>,
}

#[derive(Debug, Clone, Default)]
struct RuntimeHealthFacts {
    scheduler: SchedulerHealthFacts,
    transcription: CapabilityHealthFacts,
    agent_runtime: CapabilityHealthFacts,
    delivery: CapabilityHealthFacts,
}

/// The job kinds that roll up into capability health, in the buckets
/// [`RuntimeHealthFacts::capability_mut`] assigns. SQL that feeds the
/// rollup constrains itself to this list.
const CAPABILITY_JOB_KINDS: &[&str] = &[
    "audio_segment",
    "transcription_mux",
    "agent_task",
    "text_delivery",
    "discord_text_send",
];

impl RuntimeHealthFacts {
    fn capability_mut(&mut self, kind: &str) -> Option<&mut CapabilityHealthFacts> {
        match kind {
            "audio_segment" | "transcription_mux" => Some(&mut self.transcription),
            "agent_task" => Some(&mut self.agent_runtime),
            "text_delivery" | "discord_text_send" => Some(&mut self.delivery),
            _ => None,
        }
    }
}

#[derive(Debug)]
struct ActiveJobAggregate {
    scope_kind: String,
    guild_id: String,
    scope_id: String,
    kind: String,
    state: String,
    lane: String,
    count: usize,
    cancellable: usize,
    due_queued: usize,
    oldest_created_at_ms: i64,
    oldest_due_at_ms: Option<i64>,
    oldest_running_at_ms: Option<i64>,
    latest_updated_at_ms: i64,
}

pub(crate) async fn apply_voice_observation_freshness(
    runtime: &Ctx,
    status: &mut Value,
    now: DateTime<Utc>,
) -> Result<VoiceObservationSummary> {
    let now_ms = instant_ms_dt(now);
    let fresh_for_seconds = scheduler_fresh_for_seconds();
    let fresh_for_ms = fresh_for_seconds * 1000;
    let snapshot_at_ms = sqlx::query_scalar::<_, i64>(
        "SELECT updated_at_ms FROM runtime_status WHERE status_key = $1",
    )
    .bind(VOICE_ADAPTER_SNAPSHOT_STATUS_KEY)
    .fetch_optional(&runtime.store.pool)
    .await?;
    let snapshot_fresh =
        snapshot_at_ms.is_some_and(|observed_at_ms| now_ms - observed_at_ms <= fresh_for_ms);

    let bot_rows = sqlx::query("SELECT bot_id, updated_at_ms FROM bot_states ORDER BY bot_id")
        .fetch_all(&runtime.store.pool)
        .await?;
    let bot_observed_at = bot_rows
        .into_iter()
        .map(|row| {
            Ok((
                row.try_get::<String, _>("bot_id")?,
                row.try_get::<i64, _>("updated_at_ms")?,
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let session_rows =
        sqlx::query("SELECT session_id, updated_at_ms FROM capture_sessions ORDER BY session_id")
            .fetch_all(&runtime.store.pool)
            .await?;
    let session_observed_at = session_rows
        .into_iter()
        .map(|row| {
            Ok((
                row.try_get::<String, _>("session_id")?,
                row.try_get::<i64, _>("updated_at_ms")?,
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;

    let object = status
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("runtime status payload must be an object"))?;
    let bots = object
        .remove("bots")
        .and_then(|value| value.as_array().cloned())
        .ok_or_else(|| anyhow::anyhow!("runtime status payload must contain a bots array"))?;
    let sessions = object
        .remove("sessions")
        .and_then(|value| value.as_array().cloned())
        .ok_or_else(|| anyhow::anyhow!("runtime status payload must contain a sessions array"))?;

    let mut fresh_bots = Vec::new();
    let mut stale_bots = Vec::new();
    let mut ready_bots = 0_usize;
    let mut gateway_bots = 0_usize;
    for mut bot in bots {
        let bot_id = bot
            .get("botId")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("voice bot status must contain botId"))?;
        let observed_at_ms = *bot_observed_at
            .get(bot_id)
            .ok_or_else(|| anyhow::anyhow!("voice bot {bot_id} has no observation timestamp"))?;
        let fresh = snapshot_fresh && now_ms - observed_at_ms <= fresh_for_ms;
        add_observation_metadata(&mut bot, observed_at_ms, fresh_for_seconds, fresh, now_ms)?;
        if fresh {
            if bot.get("ready").and_then(Value::as_bool).unwrap_or(false) {
                ready_bots += 1;
            }
            if bot
                .get("gatewayRunning")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                gateway_bots += 1;
            }
            fresh_bots.push(bot);
        } else {
            stale_bots.push(bot);
        }
    }

    let mut fresh_sessions = Vec::new();
    let mut stale_sessions = Vec::new();
    let mut fresh_session_ids = BTreeSet::new();
    for mut session in sessions {
        let session_id = session
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("capture session status must contain sessionId"))?
            .to_string();
        let observed_at_ms = *session_observed_at.get(&session_id).ok_or_else(|| {
            anyhow::anyhow!("capture session {session_id} has no observation timestamp")
        })?;
        let fresh = snapshot_fresh && now_ms - observed_at_ms <= fresh_for_ms;
        add_observation_metadata(
            &mut session,
            observed_at_ms,
            fresh_for_seconds,
            fresh,
            now_ms,
        )?;
        if fresh {
            fresh_session_ids.insert(session_id);
            fresh_sessions.push(session);
        } else {
            stale_sessions.push(session);
        }
    }

    if let Some(rooms) = object.get_mut("rooms").and_then(Value::as_array_mut) {
        for room in rooms {
            let current_session_id = room
                .get("activeSessionId")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !current_session_id.is_empty() && !fresh_session_ids.contains(current_session_id) {
                room.as_object_mut()
                    .expect("runtime room status is an object")
                    .insert("activeSessionId".to_string(), json!(""));
            }
        }
    }
    if let Some(pool) = object.get_mut("pool").and_then(Value::as_object_mut) {
        let active_assignments = pool
            .get("activeAssignments")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        pool.insert("observedBots".to_string(), json!(fresh_bots.len()));
        pool.insert(
            "availableBots".to_string(),
            json!(fresh_bots.len().saturating_sub(active_assignments)),
        );
    }

    let stale_bot_count = stale_bots.len();
    let stale_session_count = stale_sessions.len();
    let observed_bot_count = fresh_bots.len();
    let active_session_count = fresh_sessions.len();
    object.insert("bots".to_string(), Value::Array(fresh_bots));
    object.insert("sessions".to_string(), Value::Array(fresh_sessions));
    object.insert(
        "staleObservations".to_string(),
        json!({
            "bots": stale_bots,
            "sessions": stale_sessions,
        }),
    );
    object.insert(
        "observation".to_string(),
        json!({
            "observedAt": snapshot_at_ms.map(ms_iso),
            "ageSeconds": snapshot_at_ms.map(|at| age_seconds(now_ms, at)),
            "freshForSeconds": fresh_for_seconds,
            "fresh": snapshot_fresh,
        }),
    );

    Ok(VoiceObservationSummary {
        snapshot_at_ms,
        fresh_for_seconds,
        observed_bots: observed_bot_count,
        ready_bots,
        gateway_bots,
        active_sessions: active_session_count,
        stale_bots: stale_bot_count,
        stale_sessions: stale_session_count,
    })
}

fn add_observation_metadata(
    value: &mut Value,
    observed_at_ms: i64,
    fresh_for_seconds: i64,
    fresh: bool,
    now_ms: i64,
) -> Result<()> {
    let object = value
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("runtime observation payload must be an object"))?;
    object.insert(
        "observation".to_string(),
        json!({
            "observedAt": ms_iso(observed_at_ms),
            "ageSeconds": age_seconds(now_ms, observed_at_ms),
            "freshForSeconds": fresh_for_seconds,
            "fresh": fresh,
        }),
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)] // parameter-struct cleanup tracked in WORKING_PLAN
fn runtime_health(
    database: &Value,
    jobs: &[JobDiagnosticRow],
    failure_summary: &Value,
    voice: &VoiceObservationSummary,
    wake_provider: &Value,
    configured_room_count: usize,
    automation_count: usize,
    now: DateTime<Utc>,
) -> Value {
    let facts = runtime_health_facts_from_rows(jobs, instant_ms_dt(now));
    runtime_health_from_facts(
        database,
        &facts,
        failure_summary,
        voice,
        wake_provider,
        configured_room_count,
        automation_count,
        now,
    )
}

#[allow(clippy::too_many_arguments)] // parameter-struct cleanup tracked in WORKING_PLAN
fn runtime_health_from_facts(
    database: &Value,
    facts: &RuntimeHealthFacts,
    failure_summary: &Value,
    voice: &VoiceObservationSummary,
    wake_provider: &Value,
    configured_room_count: usize,
    automation_count: usize,
    now: DateTime<Utc>,
) -> Value {
    let now_ms = instant_ms_dt(now);
    let observed_at = isoformat_z(now);
    let mut components = Vec::new();

    let database_ok = database.get("ok").and_then(Value::as_bool).unwrap_or(false);
    let database_diagnostic_errors = database
        .get("errors")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let database_status = if !database_ok {
        "down"
    } else if database_diagnostic_errors > 0 {
        "degraded"
    } else {
        "ok"
    };
    let pool = database
        .get("pool")
        .and_then(Value::as_object)
        .expect("database health payload contains pool state");
    let open_connections = pool
        .get("openConnections")
        .and_then(Value::as_u64)
        .expect("database pool state contains openConnections");
    let max_connections = pool
        .get("configuredMaxConnections")
        .and_then(Value::as_u64)
        .expect("database pool state contains configuredMaxConnections");
    let database_reason = match database_status {
        "down" => "Connection failed".to_string(),
        "degraded" => format!(
            "{database_diagnostic_errors} query errors · {open_connections}/{max_connections} connections"
        ),
        _ => format!("{open_connections}/{max_connections} connections"),
    };
    components.push(health_component(
        "postgres",
        database_status,
        true,
        Some(now_ms),
        30,
        &database_reason,
        json!({
            "queryErrorCount": database_diagnostic_errors,
            "errors": database.get("errors").cloned().unwrap_or_else(|| json!([])),
            "pool": database.get("pool").cloned().unwrap_or_else(|| json!({})),
        }),
        now_ms,
    ));

    let scheduler_fresh_for_seconds = scheduler_fresh_for_seconds();
    let scheduler = &facts.scheduler;
    let scheduler_observed_at_ms = scheduler.observed_at_ms;
    let scheduler_age = scheduler_observed_at_ms.map(|at| age_seconds(now_ms, at));
    let (scheduler_status, scheduler_reason) = match (scheduler_observed_at_ms, scheduler_age) {
        (None, _) => ("unknown", "No heartbeat".to_string()),
        (Some(_), Some(age)) if age > scheduler_fresh_for_seconds => (
            "stale",
            format!("Heartbeat {age}s old · threshold {scheduler_fresh_for_seconds}s"),
        ),
        (Some(_), Some(age)) if scheduler.latest_failed => (
            "degraded",
            format!("Heartbeat {age}s ago · latest {}", scheduler.latest_state),
        ),
        (Some(_), Some(age)) if scheduler.oldest_due_seconds > scheduler_fresh_for_seconds => (
            "degraded",
            format!(
                "Heartbeat {age}s ago · due backlog {}s",
                scheduler.oldest_due_seconds
            ),
        ),
        (Some(_), Some(age)) if scheduler.oldest_running_seconds > 30 * 60 => (
            "degraded",
            format!(
                "Heartbeat {age}s ago · running {}s",
                scheduler.oldest_running_seconds
            ),
        ),
        (Some(_), Some(age)) if scheduler.oldest_due_seconds > 0 => (
            "ok",
            format!(
                "Heartbeat {age}s ago · due backlog {}s",
                scheduler.oldest_due_seconds
            ),
        ),
        (Some(_), Some(age)) => ("ok", format!("Heartbeat {age}s ago · no due backlog")),
        (Some(_), None) => unreachable!("scheduler observation age is present"),
    };
    components.push(health_component(
        "scheduler",
        scheduler_status,
        true,
        scheduler_observed_at_ms,
        scheduler_fresh_for_seconds,
        &scheduler_reason,
        json!({
            "latestJobId": scheduler.latest_job_id,
            "latestState": scheduler.latest_state,
            "oldestDueQueuedAgeSeconds": scheduler.oldest_due_seconds,
            "oldestRunningAgeSeconds": scheduler.oldest_running_seconds,
        }),
        now_ms,
    ));

    let voice_required = configured_room_count > 0;
    let voice_snapshot_age = voice.snapshot_at_ms.map(|at| age_seconds(now_ms, at));
    let (voice_status, voice_reason) = match (voice.snapshot_at_ms, voice_snapshot_age) {
        (None, _) => ("unknown", "No snapshot".to_string()),
        (Some(_), Some(age)) if age > voice.fresh_for_seconds => (
            "stale",
            format!(
                "Snapshot {age}s old · threshold {}s",
                voice.fresh_for_seconds
            ),
        ),
        (Some(_), Some(age)) if voice.observed_bots == 0 && voice.stale_bots > 0 => (
            "down",
            format!(
                "0/{} fresh · {} stale · snapshot {age}s old",
                voice.stale_bots, voice.stale_bots
            ),
        ),
        (Some(_), Some(age)) if voice.observed_bots == 0 && voice_required => (
            "down",
            format!("0 bots observed · {configured_room_count} rooms · snapshot {age}s old"),
        ),
        (Some(_), Some(age)) if voice.gateway_bots == 0 && voice.observed_bots > 0 => (
            "down",
            format!("0/{} gateways · snapshot {age}s old", voice.observed_bots),
        ),
        (Some(_), Some(age))
            if voice.ready_bots < voice.observed_bots
                || voice.gateway_bots < voice.observed_bots =>
        {
            (
                "degraded",
                format!(
                    "{}/{} ready · {}/{} gateways · snapshot {age}s old",
                    voice.ready_bots, voice.observed_bots, voice.gateway_bots, voice.observed_bots
                ),
            )
        }
        (Some(_), Some(age)) => (
            "ok",
            format!(
                "{}/{} ready · snapshot {age}s old",
                voice.ready_bots, voice.observed_bots
            ),
        ),
        (Some(_), None) => unreachable!("voice snapshot age is present"),
    };
    components.push(health_component(
        "voice_gateway",
        voice_status,
        voice_required,
        voice.snapshot_at_ms,
        voice.fresh_for_seconds,
        &voice_reason,
        json!({
            "configuredRooms": configured_room_count,
            "observedBots": voice.observed_bots,
            "readyBots": voice.ready_bots,
            "gatewayBots": voice.gateway_bots,
            "staleBotObservations": voice.stale_bots,
        }),
        now_ms,
    ));

    let (capture_status, capture_reason) = match (voice.snapshot_at_ms, voice_snapshot_age) {
        (None, _) => ("unknown", "No snapshot".to_string()),
        (Some(_), Some(age)) if age > voice.fresh_for_seconds => (
            "stale",
            format!(
                "Snapshot {age}s old · threshold {}s",
                voice.fresh_for_seconds
            ),
        ),
        (Some(_), Some(age)) if voice.stale_sessions > 0 => (
            "stale",
            format!(
                "{} active · {} stale · snapshot {age}s old",
                voice.active_sessions, voice.stale_sessions
            ),
        ),
        (Some(_), Some(age)) => (
            "ok",
            format!("{} active · snapshot {age}s old", voice.active_sessions),
        ),
        (Some(_), None) => unreachable!("capture snapshot age is present"),
    };
    components.push(health_component(
        "capture",
        capture_status,
        voice_required,
        voice.snapshot_at_ms,
        voice.fresh_for_seconds,
        &capture_reason,
        json!({
            "activeSessions": voice.active_sessions,
            "staleSessionObservations": voice.stale_sessions,
        }),
        now_ms,
    ));

    let wake_provider_available = wake_provider
        .get("available")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let wake_status = string_field(wake_provider, "status");
    let wake_failures = wake_provider
        .get("consecutiveFailures")
        .and_then(Value::as_u64)
        .expect("wake provider health contains consecutiveFailures");
    let wake_reason = format!("Circuit {wake_status} · {wake_failures} consecutive failures");
    components.push(health_component(
        "wake_provider",
        if wake_provider_available {
            "ok"
        } else {
            "down"
        },
        true,
        Some(now_ms),
        30,
        &wake_reason,
        wake_provider.clone(),
        now_ms,
    ));

    components.push(capability_health_component(
        "transcription",
        &facts.transcription,
        now_ms,
    ));
    components.push(capability_health_component(
        "agent_runtime",
        &facts.agent_runtime,
        now_ms,
    ));
    components.push(capability_health_component(
        "delivery",
        &facts.delivery,
        now_ms,
    ));

    let overall_status = overall_health_status(&components);
    json!({
        "ok": overall_status == "ok",
        "status": overall_status,
        "observedAt": observed_at,
        "components": components,
        "failures": failure_summary,
        "inventory": {
            "configuredRooms": configured_room_count,
            "automationsLoaded": automation_count,
        },
    })
}

#[allow(clippy::too_many_arguments)] // parameter-struct cleanup tracked in WORKING_PLAN
fn health_component(
    component: &str,
    status: &str,
    required: bool,
    observed_at_ms: Option<i64>,
    fresh_for_seconds: i64,
    reason: &str,
    details: Value,
    now_ms: i64,
) -> Value {
    json!({
        "component": component,
        "status": status,
        "required": required,
        "observedAt": observed_at_ms.map(ms_iso),
        "ageSeconds": observed_at_ms.map(|at| age_seconds(now_ms, at)),
        "freshForSeconds": fresh_for_seconds,
        "reason": reason,
        "details": details,
    })
}

fn capability_health_component(
    component: &str,
    facts: &CapabilityHealthFacts,
    now_ms: i64,
) -> Value {
    let (status, reason) = if facts.terminal == 0 {
        ("unknown", "0 outcomes · 1h".to_string())
    } else if facts.failed > 0 {
        (
            "degraded",
            format!(
                "{}/{} complete · {} failed · 1h",
                facts.completed, facts.terminal, facts.failed
            ),
        )
    } else {
        (
            "ok",
            format!(
                "{}/{} complete · 0 failed · 1h",
                facts.completed, facts.terminal
            ),
        )
    };
    health_component(
        component,
        status,
        false,
        facts.latest_at_ms,
        FAILURE_WINDOW_SECONDS,
        &reason,
        json!({
            "window": "1h",
            "active": facts.active,
            "terminal": facts.terminal,
            "completed": facts.completed,
            "failed": facts.failed,
        }),
        now_ms,
    )
}

fn runtime_health_facts_from_rows(jobs: &[JobDiagnosticRow], now_ms: i64) -> RuntimeHealthFacts {
    let scheduler_latest = jobs
        .iter()
        .filter(|row| row.kind == "runtime_maintenance" && row.terminal)
        .max_by_key(|row| row.activity_ms());
    let mut facts = RuntimeHealthFacts {
        scheduler: SchedulerHealthFacts {
            latest_job_id: scheduler_latest
                .map(|row| row.job_id.clone())
                .unwrap_or_default(),
            latest_state: scheduler_latest
                .map(|row| row.state.clone())
                .unwrap_or_default(),
            observed_at_ms: scheduler_latest.map(JobDiagnosticRow::activity_ms),
            latest_failed: scheduler_latest.is_some_and(JobDiagnosticRow::is_failed),
            oldest_due_seconds: jobs
                .iter()
                .filter(|row| row.state == "queued" && row.ready_at_ms <= now_ms)
                .map(|row| age_seconds(now_ms, row.ready_at_ms))
                .max()
                .unwrap_or(0),
            oldest_running_seconds: jobs
                .iter()
                .filter(|row| row.state == "running")
                .map(|row| age_seconds(now_ms, row.started_at_ms.unwrap_or(row.created_at_ms)))
                .max()
                .unwrap_or(0),
        },
        ..RuntimeHealthFacts::default()
    };
    for row in jobs {
        let Some(capability) = facts.capability_mut(&row.kind) else {
            continue;
        };
        if row.is_active() {
            capability.active += 1;
        } else if row
            .terminal_at_ms()
            .is_some_and(|at| at >= now_ms - FAILURE_WINDOW_SECONDS * 1000)
        {
            capability.terminal += 1;
            capability.completed += usize::from(row.state == "complete");
            capability.failed += usize::from(row.is_failed());
            capability.latest_at_ms = Some(
                capability
                    .latest_at_ms
                    .map_or(row.updated_at_ms, |latest| latest.max(row.updated_at_ms)),
            );
        }
    }
    facts
}

fn overall_health_status(components: &[Value]) -> &'static str {
    let has = |status: &str, required: Option<bool>| {
        components.iter().any(|component| {
            component.get("status").and_then(Value::as_str) == Some(status)
                && required.is_none_or(|required| {
                    component.get("required").and_then(Value::as_bool) == Some(required)
                })
        })
    };
    if has("down", Some(true)) {
        "down"
    } else if has("stale", Some(true)) {
        "stale"
    } else if has("unknown", Some(true)) {
        "unknown"
    } else if has("degraded", None) || has("down", Some(false)) || has("stale", Some(false)) {
        "degraded"
    } else {
        "ok"
    }
}

fn scheduler_fresh_for_seconds() -> i64 {
    ((config::runtime_maintenance_interval_ms().saturating_mul(4) + 999) / 1000).max(60)
}

fn unavailable_failure_summary(now: DateTime<Utc>) -> Value {
    json!({
        "window": "1h",
        "since": isoformat_z(now - Duration::seconds(FAILURE_WINDOW_SECONDS)),
        "count": 0,
        "complete": false,
        "coverageStartsAt": Value::Null,
        "recent": [],
    })
}

async fn database_health_probe(runtime: &Ctx) -> Value {
    match sqlx::query("SELECT 1").execute(&runtime.store.pool).await {
        Ok(_) => json!({
            "ok": true,
            "errors": [],
            "pool": postgres_pool_payload(runtime),
        }),
        Err(error) => json!({
            "ok": false,
            "error": error.to_string(),
            "errors": [error.to_string()],
            "pool": postgres_pool_payload(runtime),
        }),
    }
}

async fn active_job_aggregates(
    runtime: &Ctx,
    now: DateTime<Utc>,
) -> Result<Vec<ActiveJobAggregate>> {
    let rows = sqlx::query(
        r#"
        SELECT scope_kind, guild_id, scope_id, kind, state, lane,
               COUNT(*)::BIGINT AS job_count,
               COUNT(*) FILTER (WHERE cancellable)::BIGINT AS cancellable_count,
               COUNT(*) FILTER (
                 WHERE state = 'queued' AND ready_at_ms <= $1
               )::BIGINT AS due_queued_count,
               MIN(created_at_ms) AS oldest_created_at_ms,
               MIN(ready_at_ms) FILTER (
                 WHERE state = 'queued' AND ready_at_ms <= $1
               ) AS oldest_due_at_ms,
               MIN(COALESCE(started_at_ms, created_at_ms)) FILTER (
                 WHERE state = 'running'
               ) AS oldest_running_at_ms,
               MAX(updated_at_ms) AS latest_updated_at_ms
        FROM jobs
        WHERE terminal = FALSE
        GROUP BY scope_kind, guild_id, scope_id, kind, state, lane
        ORDER BY state, kind, scope_kind, guild_id, scope_id, lane
        "#,
    )
    .bind(instant_ms_dt(now))
    .fetch_all(&runtime.store.pool)
    .await?;
    rows.into_iter()
        .map(|row| {
            Ok(ActiveJobAggregate {
                scope_kind: row.try_get("scope_kind")?,
                guild_id: row.try_get("guild_id")?,
                scope_id: row.try_get("scope_id")?,
                kind: row.try_get("kind")?,
                state: row.try_get("state")?,
                lane: row.try_get("lane")?,
                count: row.try_get::<i64, _>("job_count")? as usize,
                cancellable: row.try_get::<i64, _>("cancellable_count")? as usize,
                due_queued: row.try_get::<i64, _>("due_queued_count")? as usize,
                oldest_created_at_ms: row.try_get("oldest_created_at_ms")?,
                oldest_due_at_ms: row.try_get("oldest_due_at_ms")?,
                oldest_running_at_ms: row.try_get("oldest_running_at_ms")?,
                latest_updated_at_ms: row.try_get("latest_updated_at_ms")?,
            })
        })
        .collect()
}

fn active_job_summary(rows: &[ActiveJobAggregate]) -> Value {
    let mut by_state = BTreeMap::<String, usize>::new();
    let mut by_kind = BTreeMap::<String, usize>::new();
    let mut by_scope = BTreeMap::<String, ScopeJobSummary>::new();
    let mut total = 0_usize;
    let mut queued = 0_usize;
    let mut running = 0_usize;
    let mut waiting = 0_usize;
    let mut cancellable = 0_usize;
    for row in rows {
        total += row.count;
        cancellable += row.cancellable;
        *by_state.entry(row.state.clone()).or_insert(0) += row.count;
        *by_kind.entry(row.kind.clone()).or_insert(0) += row.count;
        match row.state.as_str() {
            "queued" => queued += row.count,
            "running" => running += row.count,
            "waiting" => waiting += row.count,
            _ => {}
        }
        let key = format!("{}\n{}\n{}", row.scope_kind, row.guild_id, row.scope_id);
        let scope = by_scope.entry(key).or_insert_with(|| ScopeJobSummary {
            scope_kind: row.scope_kind.clone(),
            guild_id: row.guild_id.clone(),
            scope_id: row.scope_id.clone(),
            ..ScopeJobSummary::default()
        });
        scope.total += row.count;
        scope.active += row.count;
        let latest_at = ms_iso(row.latest_updated_at_ms);
        if latest_at > scope.latest_at {
            scope.latest_at = latest_at;
        }
    }
    json!({
        "total": total,
        "active": total,
        "terminal": 0,
        "queued": queued,
        "running": running,
        "waiting": waiting,
        "failed": 0,
        "cancellable": cancellable,
        "byState": count_rows(by_state, "state"),
        "byKind": count_rows(by_kind, "kind"),
        "byScope": scope_job_rows(by_scope),
    })
}

fn active_job_backlog(rows: &[ActiveJobAggregate], now: DateTime<Utc>) -> Value {
    let now_ms = instant_ms_dt(now);
    let mut total = 0_usize;
    let mut queued = 0_usize;
    let mut due_queued = 0_usize;
    let mut running = 0_usize;
    let mut waiting = 0_usize;
    let mut cancel_requested = 0_usize;
    let mut confirmation_pending = 0_usize;
    let mut cancellable = 0_usize;
    let mut oldest_active_age_seconds = 0_i64;
    let mut oldest_queued_age_seconds = 0_i64;
    let mut oldest_running_age_seconds = 0_i64;
    let mut by_state = BTreeMap::<String, usize>::new();
    let mut by_kind_state = BTreeMap::<(String, String), usize>::new();
    let mut by_lane_state = BTreeMap::<(String, String), usize>::new();
    let mut by_kind = BTreeMap::<String, BacklogKindSummary>::new();
    for row in rows {
        total += row.count;
        cancellable += row.cancellable;
        *by_state.entry(row.state.clone()).or_insert(0) += row.count;
        *by_kind_state
            .entry((row.kind.clone(), row.state.clone()))
            .or_insert(0) += row.count;
        *by_lane_state
            .entry((row.lane.clone(), row.state.clone()))
            .or_insert(0) += row.count;
        let active_age = age_seconds(now_ms, row.oldest_created_at_ms);
        oldest_active_age_seconds = oldest_active_age_seconds.max(active_age);
        let kind = by_kind
            .entry(row.kind.clone())
            .or_insert_with(|| BacklogKindSummary {
                kind: row.kind.clone(),
                ..BacklogKindSummary::default()
            });
        kind.active += row.count;
        kind.cancellable += row.cancellable;
        kind.oldest_active_age_seconds = kind.oldest_active_age_seconds.max(active_age);
        match row.state.as_str() {
            "queued" => {
                queued += row.count;
                due_queued += row.due_queued;
                oldest_queued_age_seconds = oldest_queued_age_seconds.max(active_age);
                kind.queued += row.count;
                kind.due_queued += row.due_queued;
                kind.oldest_queued_age_seconds = kind.oldest_queued_age_seconds.max(active_age);
            }
            "running" => {
                running += row.count;
                let running_age = row
                    .oldest_running_at_ms
                    .map(|at| age_seconds(now_ms, at))
                    .unwrap_or(active_age);
                oldest_running_age_seconds = oldest_running_age_seconds.max(running_age);
                kind.running += row.count;
                kind.oldest_running_age_seconds = kind.oldest_running_age_seconds.max(running_age);
            }
            "waiting" => {
                waiting += row.count;
                kind.waiting += row.count;
            }
            "cancel_requested" => {
                cancel_requested += row.count;
                kind.cancel_requested += row.count;
            }
            "confirmation_pending" => {
                confirmation_pending += row.count;
                kind.confirmation_pending += row.count;
            }
            _ => {}
        }
    }
    let mut kind_rows = by_kind
        .into_values()
        .map(|summary| summary.to_json())
        .collect::<Vec<_>>();
    kind_rows.sort_by(|left, right| {
        json_usize(right, "active")
            .cmp(&json_usize(left, "active"))
            .then_with(|| json_usize(right, "dueQueued").cmp(&json_usize(left, "dueQueued")))
            .then_with(|| string_field(left, "kind").cmp(&string_field(right, "kind")))
    });
    json!({
        "total": total,
        "queued": queued,
        "dueQueued": due_queued,
        "running": running,
        "waiting": waiting,
        "cancelRequested": cancel_requested,
        "confirmationPending": confirmation_pending,
        "cancellable": cancellable,
        "oldestActiveAgeSeconds": oldest_active_age_seconds,
        "oldestQueuedAgeSeconds": oldest_queued_age_seconds,
        "oldestRunningAgeSeconds": oldest_running_age_seconds,
        "byState": count_rows(by_state, "state"),
        "byKindState": count_pair_rows(by_kind_state, "kind", "state"),
        "byLaneState": count_pair_rows(by_lane_state, "lane", "state"),
        "byKind": kind_rows,
    })
}

fn apply_active_health_facts(
    facts: &mut RuntimeHealthFacts,
    rows: &[ActiveJobAggregate],
    now: DateTime<Utc>,
) {
    let now_ms = instant_ms_dt(now);
    for row in rows {
        facts.scheduler.oldest_due_seconds = facts.scheduler.oldest_due_seconds.max(
            row.oldest_due_at_ms
                .map(|at| age_seconds(now_ms, at))
                .unwrap_or(0),
        );
        facts.scheduler.oldest_running_seconds = facts.scheduler.oldest_running_seconds.max(
            row.oldest_running_at_ms
                .map(|at| age_seconds(now_ms, at))
                .unwrap_or(0),
        );
        if let Some(capability) = facts.capability_mut(&row.kind) {
            capability.active += row.count;
        }
    }
}

async fn lean_terminal_health_facts(
    runtime: &Ctx,
    now: DateTime<Utc>,
) -> Result<RuntimeHealthFacts> {
    let since_ms = instant_ms_dt(now) - FAILURE_WINDOW_SECONDS * 1000;
    let capability_outcomes_sql = format!(
        r#"
            SELECT kind, state, failed, COUNT(*)::BIGINT AS outcome_count,
                   MAX(observed_at_ms) AS latest_at_ms
            FROM operational_job_outcomes
            WHERE observed_at_ms >= $1
              AND kind IN ({})
            GROUP BY kind, state, failed
            "#,
        CAPABILITY_JOB_KINDS
            .iter()
            .map(|kind| format!("'{kind}'"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let (latest_maintenance, rows) = tokio::try_join!(
        sqlx::query(
            r#"
            SELECT job_id, state, failed, observed_at_ms
            FROM operational_job_outcomes
            WHERE kind = 'runtime_maintenance'
            ORDER BY observed_at_ms DESC, observation_id DESC
            LIMIT 1
            "#,
        )
        .fetch_optional(&runtime.store.pool),
        sqlx::query(&capability_outcomes_sql)
            .bind(since_ms)
            .fetch_all(&runtime.store.pool),
    )?;
    let mut facts = RuntimeHealthFacts::default();
    if let Some(row) = latest_maintenance {
        facts.scheduler.latest_job_id = row.try_get("job_id")?;
        facts.scheduler.latest_state = row.try_get("state")?;
        facts.scheduler.latest_failed = row.try_get("failed")?;
        facts.scheduler.observed_at_ms = Some(row.try_get("observed_at_ms")?);
    }
    for row in rows {
        let kind = row.try_get::<String, _>("kind")?;
        let capability = facts
            .capability_mut(&kind)
            .expect("capability health query constrains itself to CAPABILITY_JOB_KINDS");
        let count = row.try_get::<i64, _>("outcome_count")? as usize;
        let state = row.try_get::<String, _>("state")?;
        capability.terminal += count;
        capability.completed += usize::from(state == "complete") * count;
        capability.failed += usize::from(row.try_get::<bool, _>("failed")?) * count;
        let latest_at_ms = row.try_get::<i64, _>("latest_at_ms")?;
        capability.latest_at_ms = Some(
            capability
                .latest_at_ms
                .map_or(latest_at_ms, |latest| latest.max(latest_at_ms)),
        );
    }
    Ok(facts)
}

async fn lean_voice_observation_summary(
    runtime: &Ctx,
    now: DateTime<Utc>,
) -> Result<VoiceObservationSummary> {
    let now_ms = instant_ms_dt(now);
    let fresh_for_seconds = scheduler_fresh_for_seconds();
    let cutoff_ms = now_ms - fresh_for_seconds * 1000;
    let row = sqlx::query(
        r#"
        SELECT
          (SELECT updated_at_ms FROM runtime_status WHERE status_key = $2)
            AS snapshot_at_ms,
          (SELECT COUNT(*)::BIGINT FROM bot_states) AS bot_count,
          (SELECT COUNT(*)::BIGINT FROM bot_states WHERE updated_at_ms >= $1) AS fresh_bot_count,
          (SELECT COUNT(*)::BIGINT FROM bot_states
             WHERE updated_at_ms >= $1
               AND COALESCE((payload_json->>'ready')::BOOLEAN, FALSE)) AS ready_bot_count,
          (SELECT COUNT(*)::BIGINT FROM bot_states
             WHERE updated_at_ms >= $1
               AND COALESCE((payload_json->>'gatewayRunning')::BOOLEAN, FALSE)) AS gateway_bot_count,
          (SELECT COUNT(*)::BIGINT FROM capture_sessions WHERE active = TRUE) AS session_count,
          (SELECT COUNT(*)::BIGINT FROM capture_sessions
             WHERE active = TRUE AND updated_at_ms >= $1) AS fresh_session_count
        "#,
    )
    .bind(cutoff_ms)
    .bind(VOICE_ADAPTER_SNAPSHOT_STATUS_KEY)
    .fetch_one(&runtime.store.pool)
    .await?;
    let snapshot_at_ms = row.try_get::<Option<i64>, _>("snapshot_at_ms")?;
    let snapshot_fresh = snapshot_at_ms.is_some_and(|at| at >= cutoff_ms);
    let bot_count = row.try_get::<i64, _>("bot_count")? as usize;
    let session_count = row.try_get::<i64, _>("session_count")? as usize;
    let fresh_bot_count = row.try_get::<i64, _>("fresh_bot_count")? as usize;
    let fresh_session_count = row.try_get::<i64, _>("fresh_session_count")? as usize;
    Ok(VoiceObservationSummary {
        snapshot_at_ms,
        fresh_for_seconds,
        observed_bots: if snapshot_fresh { fresh_bot_count } else { 0 },
        ready_bots: if snapshot_fresh {
            row.try_get::<i64, _>("ready_bot_count")? as usize
        } else {
            0
        },
        gateway_bots: if snapshot_fresh {
            row.try_get::<i64, _>("gateway_bot_count")? as usize
        } else {
            0
        },
        active_sessions: if snapshot_fresh {
            fresh_session_count
        } else {
            0
        },
        stale_bots: if snapshot_fresh {
            bot_count.saturating_sub(fresh_bot_count)
        } else {
            bot_count
        },
        stale_sessions: if snapshot_fresh {
            session_count.saturating_sub(fresh_session_count)
        } else {
            session_count
        },
    })
}

async fn dashboard_inventory_counts(runtime: &Ctx) -> Result<(usize, usize)> {
    let row = sqlx::query(
        r#"
        SELECT (SELECT COUNT(*)::BIGINT FROM voice_rooms) AS room_count,
               (SELECT COUNT(*)::BIGINT FROM automations) AS automation_count
        "#,
    )
    .fetch_one(&runtime.store.pool)
    .await?;
    Ok((
        row.try_get::<i64, _>("room_count")? as usize,
        row.try_get::<i64, _>("automation_count")? as usize,
    ))
}

fn load_payload(jobs: &[Job], now: DateTime<Utc>) -> Value {
    let mut by_kind = BTreeMap::new();
    let mut by_state = BTreeMap::new();
    let mut oldest_queued_age_seconds = 0_i64;
    let mut due_queued = 0_usize;
    for job in jobs {
        *by_kind
            .entry((
                job.kind.as_str().to_string(),
                job.state.as_str().to_string(),
            ))
            .or_insert(0_usize) += 1;
        *by_state
            .entry(job.state.as_str().to_string())
            .or_insert(0_usize) += 1;
        if job.state == JobState::Queued {
            if job.next_run_at.is_none_or(|due| due <= now) {
                due_queued += 1;
            }
            oldest_queued_age_seconds =
                oldest_queued_age_seconds.max((now - job.created_at).num_seconds());
        }
    }
    let by_kind_rows = by_kind
        .into_iter()
        .map(|((kind, state), count)| json!({"kind": kind, "state": state, "count": count}))
        .collect::<Vec<_>>();
    json!({
        "dueQueuedJobs": due_queued,
        "oldestQueuedAgeSeconds": oldest_queued_age_seconds,
        "byState": count_rows(by_state, "state"),
        "byKindState": by_kind_rows,
    })
}

fn scope_job_rows(scopes: BTreeMap<String, ScopeJobSummary>) -> Vec<Value> {
    let mut rows = scopes.into_values().collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        right
            .active
            .cmp(&left.active)
            .then_with(|| right.failed.cmp(&left.failed))
            .then_with(|| right.total.cmp(&left.total))
            .then_with(|| right.latest_at.cmp(&left.latest_at))
    });
    rows.into_iter()
        .map(|scope| {
            json!({
                "scope_kind": scope.scope_kind,
                "guild_id": scope.guild_id,
                "scope_id": scope.scope_id,
                "total": scope.total,
                "active": scope.active,
                "failed": scope.failed,
                "latest_at": scope.latest_at,
            })
        })
        .collect()
}
