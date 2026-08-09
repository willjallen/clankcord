use std::collections::{BTreeMap, BTreeSet};
use std::fs;

use chrono::{DateTime, Duration, Utc};
use serde_json::{Map, Value, json};
use sqlx::{Postgres, QueryBuilder, Row};

use super::dashboard::{dashboard_job_category, dashboard_job_duration_ms};
use crate::Result;
use crate::adapters::codex::{codex_usage_payload, parse_codex_jsonl};
use crate::config;
use crate::model::job::{Job, JobKind, JobState};
use crate::runtime::Ctx;
use crate::runtime::agents::AgentRuntime;
use crate::runtime::automations::{AutomationRecord, AutomationTrigger};
use crate::runtime::domain::voice_capture::wake_circuit;
use crate::runtime::timeline::store::{
    OPERATIONAL_JOB_OUTCOME_RETENTION_SECONDS, VOICE_ADAPTER_SNAPSHOT_STATUS_KEY,
};
use crate::runtime::timeline::util::timeline_event_payload;
use crate::runtime::timeline::{
    instant_ms_dt, isoformat_z, ms_to_datetime, parse_instant, round3, utc_now,
};
use crate::runtime::util::{first_non_empty, non_empty, preview, string_field};
use crate::views::dashboard;
use crate::views::status;

const AGENT_ARTIFACT_MAX_BYTES: usize = 2 * 1024 * 1024;
const AGENT_SESSION_JOB_LIMIT: usize = 100;
const DASHBOARD_VALUE_MAX_STRING_CHARS: usize = 4000;
const DASHBOARD_VALUE_MAX_ARRAY_ITEMS: usize = 100;
const HEALTH_WINDOWS: &[(&str, i64)] = &[("5m", 5 * 60), ("15m", 15 * 60), ("1h", 60 * 60)];
const FAILURE_WINDOW_SECONDS: i64 = 60 * 60;
const FAILURE_RECENT_LIMIT: i64 = 25;
const OPERATIONAL_COVERAGE_START_KEY: &str = "operational_job_outcomes_coverage_start_ms";

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
            "generatedAt": isoformat_z(Some(now)),
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
        lean_failure_summary(ctx, now),
        lean_voice_observation_summary(ctx, now),
        dashboard_inventory_counts(ctx),
    )?;
    apply_active_health_facts(&mut health_facts, &active_jobs, now);
    let (configured_room_count, automation_count) = inventory;
    let wake_provider = wake_circuit::wake_provider_health(&ctx.store).await?;
    Ok(json!({
        "generatedAt": isoformat_z(Some(now)),
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
            crate::runtime::timeline::JobVisibility::IncludeEphemeral,
        )
        .await?;
    Ok(json!({
        "generatedAt": isoformat_z(Some(now)),
        "health": health,
        "database": database,
        "requests": http_requests,
        "process": {"load": process_load},
        "load": load_payload(&active_jobs, now),
        "operations": operations,
    }))
}

pub async fn dashboard_rooms_payload(ctx: &Ctx) -> Result<Value> {
    let now = utc_now();
    let mut status = status::status_payload(ctx, None).await?;
    if let Value::Object(object) = &mut status {
        object.insert(
            "liveOccupancy".to_string(),
            ctx.store.voice_occupancy_snapshot().await?,
        );
    }
    apply_voice_observation_freshness(ctx, &mut status, now).await?;
    Ok(json!({
        "generatedAt": isoformat_z(Some(now)),
        "status": status,
    }))
}

pub async fn recent_transcript_events(
    ctx: &Ctx,
    since: Option<DateTime<Utc>>,
    limit: usize,
    channel: &str,
    query: &str,
) -> Result<Vec<Value>> {
    let kinds = BTreeSet::from(["speech_segment".to_string(), "transcript".to_string()]);
    let channel = channel.trim();
    recent_events_by_kind_filtered(
        ctx,
        since,
        None,
        limit,
        Some(&kinds),
        query,
        (!channel.is_empty()).then_some(channel),
    )
    .await
}

async fn recent_events_by_kind_filtered(
    ctx: &Ctx,
    start: Option<DateTime<Utc>>,
    end: Option<DateTime<Utc>>,
    limit: usize,
    kinds: Option<&BTreeSet<String>>,
    query: &str,
    channel: Option<&str>,
) -> Result<Vec<Value>> {
    if kinds.is_some_and(BTreeSet::is_empty) {
        return Ok(Vec::new());
    }

    let mut statement = QueryBuilder::<Postgres>::new(
        r#"
            WITH selected_events AS MATERIALIZED (
            SELECT e.sequence, e.started_at_ms, e.event_id
            FROM timeline_events e
            "#,
    );
    if !query.trim().is_empty() {
        statement.push(
            r#"
            LEFT JOIN voice_rooms r
              ON e.scope_kind = 'voice_channel'
             AND r.guild_id = e.guild_id
             AND r.voice_channel_id = e.scope_id
                "#,
        );
    }
    statement.push(
        r#"
            WHERE e.forgotten = FALSE
            "#,
    );
    if let Some(start) = start {
        statement
            .push(" AND e.ended_at_ms > ")
            .push_bind(instant_ms_dt(start));
    }
    if let Some(end) = end {
        statement
            .push(" AND e.started_at_ms < ")
            .push_bind(instant_ms_dt(end));
    }
    if let Some(kinds) = kinds {
        statement.push(" AND e.event_kind IN (");
        let mut separated = statement.separated(", ");
        for kind in kinds {
            separated.push_bind(kind);
        }
        separated.push_unseparated(")");
    }
    if let Some(channel) = channel {
        statement.push(" AND e.scope_id = ").push_bind(channel);
    }
    push_transcript_event_search(&mut statement, query);
    statement
        .push(" ORDER BY e.started_at_ms DESC, e.sequence DESC, e.event_id DESC LIMIT ")
        .push_bind(limit as i64)
        .push(
            r#"
            )
            SELECT e.*,
                   r.guild_slug AS room_guild_slug,
                   r.voice_channel_name AS room_voice_channel_name,
                   r.voice_channel_slug AS room_voice_channel_slug
            FROM selected_events selected
            JOIN timeline_events e ON e.sequence = selected.sequence
            LEFT JOIN voice_rooms r
              ON e.scope_kind = 'voice_channel'
             AND r.guild_id = e.guild_id
             AND r.voice_channel_id = e.scope_id
            ORDER BY selected.started_at_ms DESC, selected.sequence DESC, selected.event_id DESC
                "#,
        );

    let rows = statement.build().fetch_all(&ctx.store.pool).await?;
    rows.iter()
        .map(timeline_event_payload)
        .map(|event| event.map(compact_dashboard_event))
        .collect()
}

pub async fn dashboard_agent_job(ctx: &Ctx, job_id: &str) -> Result<Value> {
    let job = ctx.store.get_job(job_id).await?;
    if job.kind != JobKind::AgentTask {
        anyhow::bail!("job {job_id} is not an agent task");
    }
    agent_job_payload(ctx, &job).await
}

fn push_transcript_event_search(statement: &mut QueryBuilder<'_, Postgres>, raw_query: &str) {
    let terms = raw_query
        .split_whitespace()
        .map(transcript_search_term)
        .filter(|term| !term.is_empty())
        .collect::<Vec<_>>();
    if terms.is_empty() {
        return;
    }
    let search_expression = transcript_event_search_sql();
    for term in terms {
        statement
            .push(" AND strpos(lower(")
            .push(search_expression)
            .push("), ")
            .push_bind(term)
            .push(") > 0");
    }
}

fn transcript_event_search_sql() -> &'static str {
    r#"concat_ws(' ',
                e.event_kind,
                e.text,
                e.speaker_label,
                e.payload_json->>'kind',
                e.payload_json->>'text',
                e.payload_json->>'feedback_message',
                e.payload_json->>'reason',
                e.payload_json->>'quality',
                e.payload_json->>'job_kind',
                e.payload_json->>'state',
                e.payload_json->>'command_kind',
                e.payload_json->>'command_name',
                r.guild_slug,
                r.voice_channel_name,
                r.voice_channel_slug,
                e.payload_json->>'guild_slug',
                e.payload_json->>'voice_channel_name',
                e.payload_json->>'voice_channel_slug',
                e.payload_json->>'speaker_label',
                e.payload_json->>'speaker_username',
                e.payload_json #>> '{result,kind}',
                e.payload_json #>> '{result,status}',
                e.payload_json #>> '{result,reason}',
                e.payload_json #>> '{result,action}',
                e.payload_json #>> '{result,message}',
                e.payload_json #>> '{result,summary}',
                e.payload_json #>> '{command_result,kind}',
                e.payload_json #>> '{command_result,status}',
                e.payload_json #>> '{command_result,reason}',
                e.payload_json #>> '{command_result,action}',
                e.payload_json #>> '{command_result,message}',
                e.payload_json #>> '{command_result,summary}',
                e.payload_json #>> '{command_response,kind}',
                e.payload_json #>> '{command_response,status}',
                e.payload_json #>> '{command_response,reason}',
                e.payload_json #>> '{command_response,action}',
                e.payload_json #>> '{command_response,message}',
                e.payload_json #>> '{command_response,summary}'
            )"#
}

fn transcript_search_term(term: &str) -> String {
    term.trim_start_matches('/').to_lowercase()
}

pub(super) fn dashboard_job_value(job: &Job) -> Value {
    let mut value = compact_dashboard_value(job.to_value(), 5);
    if let Value::Object(object) = &mut value {
        let command_kind = job.command_kind();
        if !command_kind.trim().is_empty() {
            object.insert("command_kind".to_string(), json!(command_kind));
        }
    }
    value
}

fn compact_dashboard_event(event: Value) -> Value {
    let compact = compact_dashboard_value(event, 4);
    if let Value::Object(object) = &compact {
        let mut result = Map::new();
        for key in [
            "event_id",
            "event_kind",
            "kind",
            "scope_kind",
            "scope_id",
            "guild_id",
            "guild_slug",
            "voice_channel_id",
            "voice_channel_name",
            "voice_channel_slug",
            "speaker_user_id",
            "speaker_label",
            "speaker_username",
            "created_at",
            "startedAt",
            "endedAt",
            "text",
            "feedback_message",
            "reason",
            "state",
            "quality",
            "job_id",
            "job_kind",
            "command_kind",
            "command_name",
            "options",
            "conversation_id",
            "capture_run_id",
            "segment_index",
            "duration_ms",
            "referenced_message_id",
            "discord_message_id",
            "discord_channel_id",
            "agent_session_id",
        ] {
            if let Some(value) = object.get(key).filter(|value| !value.is_null()) {
                result.insert(key.to_string(), value.clone());
            }
        }
        for key in ["result", "command_result", "command_response"] {
            if let Some(value) = object.get(key).filter(|value| !value.is_null()) {
                result.insert(key.to_string(), compact_dashboard_value(value.clone(), 2));
            }
        }
        return Value::Object(result);
    }
    compact
}

fn compact_dashboard_value(value: Value, depth: usize) -> Value {
    match value {
        Value::Object(object) => {
            if depth == 0 {
                return json!({"truncated": true, "fields": object.len()});
            }
            let mut compact = Map::new();
            for (key, value) in object {
                if omit_dashboard_key(&key) {
                    continue;
                }
                compact.insert(key, compact_dashboard_value(value, depth - 1));
            }
            Value::Object(compact)
        }
        Value::Array(values) => {
            if depth == 0 {
                return json!({"truncated": true, "items": values.len()});
            }
            let original_len = values.len();
            let mut compact = values
                .into_iter()
                .take(DASHBOARD_VALUE_MAX_ARRAY_ITEMS)
                .map(|value| compact_dashboard_value(value, depth - 1))
                .collect::<Vec<_>>();
            if original_len > compact.len() {
                compact.push(json!({"truncated": true, "remaining": original_len - compact.len()}));
            }
            Value::Array(compact)
        }
        Value::String(value) => Value::String(truncate_dashboard_string(value)),
        value => value,
    }
}

fn omit_dashboard_key(key: &str) -> bool {
    matches!(
        key,
        "stt"
            | "wake_metadata"
            | "wakeMetadata"
            | "token_logprobs"
            | "tokenLogprobs"
            | "logprobs"
            | "audio_bytes"
            | "audioBytes"
            | "audio_checksum"
            | "audioChecksum"
            | "source_audio_path"
            | "sourceAudioPath"
            | "local"
            | "artifacts"
    )
}

fn truncate_dashboard_string(value: String) -> String {
    if value.chars().count() <= DASHBOARD_VALUE_MAX_STRING_CHARS {
        return value;
    }
    value
        .chars()
        .take(DASHBOARD_VALUE_MAX_STRING_CHARS)
        .collect::<String>()
        + "...[truncated]"
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

#[derive(Debug, Clone)]
struct JobDiagnosticRow {
    job_id: String,
    kind: String,
    state: String,
    lane: String,
    created_at_ms: i64,
    updated_at_ms: i64,
    ready_at_ms: i64,
    started_at_ms: Option<i64>,
    completed_at_ms: Option<i64>,
    terminal: bool,
    failed: bool,
    cancellable: bool,
}

#[derive(Debug, Clone)]
struct FailureDiagnosticRow {
    job_id: String,
    scope_kind: String,
    guild_id: String,
    scope_id: String,
    scope_label: String,
    kind: String,
    state: String,
    reason: String,
    failed_at_ms: i64,
}

impl FailureDiagnosticRow {
    fn to_json(&self) -> Value {
        json!({
            "jobId": self.job_id,
            "category": dashboard_job_category(&self.kind),
            "kind": self.kind,
            "state": self.state,
            "scopeKind": self.scope_kind,
            "guildId": self.guild_id,
            "scopeId": self.scope_id,
            "scopeLabel": self.scope_label,
            "reason": self.reason,
            "failedAt": ms_iso(self.failed_at_ms),
        })
    }
}

#[derive(Debug)]
struct OperationalDiagnostics {
    payload: Value,
    job_rows: Vec<JobDiagnosticRow>,
    failure_summary: Value,
}

#[derive(Debug, Default)]
struct VoiceObservationSummary {
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

#[derive(Debug, Default)]
struct LatencyExclusions {
    ready_delay_ms: usize,
    queue_ms: usize,
    run_ms: usize,
    total_ms: usize,
    phase_contaminated: usize,
    missing_started_at: usize,
    invalid_timestamp_order: usize,
}

impl LatencyExclusions {
    fn to_json(&self) -> Value {
        json!({
            "readyDelayMs": self.ready_delay_ms,
            "queueMs": self.queue_ms,
            "runMs": self.run_ms,
            "totalMs": self.total_ms,
            "phaseContaminated": self.phase_contaminated,
            "missingStartedAt": self.missing_started_at,
            "invalidTimestampOrder": self.invalid_timestamp_order,
        })
    }
}

impl JobDiagnosticRow {
    fn activity_ms(&self) -> i64 {
        let mut activity = self.created_at_ms.max(self.updated_at_ms);
        if let Some(started_at_ms) = self.started_at_ms {
            activity = activity.max(started_at_ms);
        }
        if let Some(completed_at_ms) = self.completed_at_ms {
            activity = activity.max(completed_at_ms);
        }
        activity
    }

    fn is_active(&self) -> bool {
        !self.terminal
    }

    fn is_failed(&self) -> bool {
        self.failed || is_failed_state(&self.state)
    }

    fn terminal_at_ms(&self) -> Option<i64> {
        self.terminal.then_some(self.updated_at_ms)
    }
}

#[derive(Debug, Clone)]
struct EventDiagnosticRow {
    event_kind: String,
    at_ms: i64,
    ended_at_ms: Option<i64>,
    speaker_user_id: String,
}

#[derive(Debug, Default)]
struct BacklogKindSummary {
    kind: String,
    active: usize,
    queued: usize,
    due_queued: usize,
    running: usize,
    waiting: usize,
    cancel_requested: usize,
    confirmation_pending: usize,
    cancellable: usize,
    oldest_queued_age_seconds: i64,
    oldest_running_age_seconds: i64,
    oldest_active_age_seconds: i64,
}

impl BacklogKindSummary {
    fn add(&mut self, row: &JobDiagnosticRow, now_ms: i64) {
        self.active += 1;
        if row.cancellable {
            self.cancellable += 1;
        }
        self.oldest_active_age_seconds = self
            .oldest_active_age_seconds
            .max(age_seconds(now_ms, row.created_at_ms));
        match row.state.as_str() {
            "queued" => {
                self.queued += 1;
                if row.ready_at_ms <= now_ms {
                    self.due_queued += 1;
                }
                self.oldest_queued_age_seconds = self
                    .oldest_queued_age_seconds
                    .max(age_seconds(now_ms, row.created_at_ms));
            }
            "running" => {
                self.running += 1;
                self.oldest_running_age_seconds = self.oldest_running_age_seconds.max(age_seconds(
                    now_ms,
                    row.started_at_ms.unwrap_or(row.created_at_ms),
                ));
            }
            "waiting" => self.waiting += 1,
            "cancel_requested" => self.cancel_requested += 1,
            "confirmation_pending" => self.confirmation_pending += 1,
            _ => {}
        }
    }

    fn to_json(&self) -> Value {
        json!({
            "kind": self.kind,
            "active": self.active,
            "queued": self.queued,
            "dueQueued": self.due_queued,
            "running": self.running,
            "waiting": self.waiting,
            "cancelRequested": self.cancel_requested,
            "confirmationPending": self.confirmation_pending,
            "cancellable": self.cancellable,
            "oldestQueuedAgeSeconds": self.oldest_queued_age_seconds,
            "oldestRunningAgeSeconds": self.oldest_running_age_seconds,
            "oldestActiveAgeSeconds": self.oldest_active_age_seconds,
        })
    }
}

async fn apply_voice_observation_freshness(
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
    let observed_at = isoformat_z(Some(now));
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
    let wake_status = string_field(&wake_provider, "status");
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
        let capability = match row.kind.as_str() {
            "audio_segment" | "transcription_mux" => Some(&mut facts.transcription),
            "agent_task" => Some(&mut facts.agent_runtime),
            "text_delivery" | "discord_text_send" => Some(&mut facts.delivery),
            _ => None,
        };
        let Some(capability) = capability else {
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
        "since": isoformat_z(Some(now - Duration::seconds(FAILURE_WINDOW_SECONDS))),
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
        match row.kind.as_str() {
            "audio_segment" | "transcription_mux" => {
                facts.transcription.active += row.count;
            }
            "agent_task" => facts.agent_runtime.active += row.count,
            "text_delivery" | "discord_text_send" => facts.delivery.active += row.count,
            _ => {}
        }
    }
}

async fn lean_terminal_health_facts(
    runtime: &Ctx,
    now: DateTime<Utc>,
) -> Result<RuntimeHealthFacts> {
    let since_ms = instant_ms_dt(now) - FAILURE_WINDOW_SECONDS * 1000;
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
        sqlx::query(
            r#"
            SELECT kind, state, failed, COUNT(*)::BIGINT AS outcome_count,
                   MAX(observed_at_ms) AS latest_at_ms
            FROM operational_job_outcomes
            WHERE observed_at_ms >= $1
              AND kind IN (
                'audio_segment', 'transcription_mux', 'agent_task',
                'text_delivery', 'discord_text_send'
              )
            GROUP BY kind, state, failed
            "#,
        )
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
        let capability = match kind.as_str() {
            "audio_segment" | "transcription_mux" => &mut facts.transcription,
            "agent_task" => &mut facts.agent_runtime,
            "text_delivery" | "discord_text_send" => &mut facts.delivery,
            _ => unreachable!("capability health query constrains job kinds"),
        };
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

async fn lean_failure_summary(runtime: &Ctx, now: DateTime<Utc>) -> Result<Value> {
    let now_ms = instant_ms_dt(now);
    let since_ms = now_ms - FAILURE_WINDOW_SECONDS * 1000;
    let coverage_start_ms = operational_coverage_start_ms(runtime).await?;
    let count = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM operational_job_outcomes WHERE failed = TRUE AND observed_at_ms >= $1",
    )
    .bind(since_ms)
    .fetch_one(&runtime.store.pool)
    .await?;
    Ok(json!({
        "window": "1h",
        "since": ms_iso(since_ms),
        "count": count,
        "complete": coverage_start_ms <= since_ms,
        "coverageStartsAt": ms_iso(coverage_start_ms),
        "recent": [],
    }))
}

async fn detailed_failure_summary(runtime: &Ctx, now: DateTime<Utc>) -> Result<Value> {
    let now_ms = instant_ms_dt(now);
    let since_ms = now_ms - FAILURE_WINDOW_SECONDS * 1000;
    let coverage_start_ms = operational_coverage_start_ms(runtime).await?;
    let count = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM operational_job_outcomes WHERE failed = TRUE AND observed_at_ms >= $1",
    )
    .bind(since_ms)
    .fetch_one(&runtime.store.pool)
    .await?;
    let recent = recent_failure_rows(runtime, since_ms, FAILURE_RECENT_LIMIT).await?;
    Ok(json!({
        "window": "1h",
        "since": ms_iso(since_ms),
        "count": count,
        "complete": coverage_start_ms <= since_ms,
        "coverageStartsAt": ms_iso(coverage_start_ms),
        "recent": recent.into_iter().map(|row| row.to_json()).collect::<Vec<_>>(),
    }))
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
        observed_bots: snapshot_fresh.then_some(fresh_bot_count).unwrap_or(0),
        ready_bots: snapshot_fresh
            .then_some(row.try_get::<i64, _>("ready_bot_count")? as usize)
            .unwrap_or(0),
        gateway_bots: snapshot_fresh
            .then_some(row.try_get::<i64, _>("gateway_bot_count")? as usize)
            .unwrap_or(0),
        active_sessions: snapshot_fresh.then_some(fresh_session_count).unwrap_or(0),
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

pub(super) async fn database_diagnostics(runtime: &Ctx) -> Value {
    if let Err(error) = sqlx::query("SELECT 1").execute(&runtime.store.pool).await {
        return json!({
            "ok": false,
            "url": runtime.store.database_url,
            "root": runtime.store.root.display().to_string(),
            "error": error.to_string(),
            "pool": postgres_pool_payload(runtime),
            "tables": [],
        });
    }
    let row = sqlx::query(
        "SELECT current_database() AS database_name, current_user AS user_name, version() AS version",
    )
    .fetch_one(&runtime.store.pool)
    .await
    .ok();
    let mut errors = Vec::new();
    let statistics = match postgres_database_statistics(runtime).await {
        Ok(value) => value,
        Err(error) => {
            errors.push(json!({"source": "pg_stat_database", "error": error.to_string()}));
            json!({})
        }
    };
    let settings = match postgres_settings(runtime).await {
        Ok(value) => value,
        Err(error) => {
            errors.push(json!({"source": "pg_settings", "error": error.to_string()}));
            Vec::new()
        }
    };
    let activity = match postgres_activity_rows(runtime).await {
        Ok(value) => value,
        Err(error) => {
            errors.push(json!({"source": "pg_stat_activity", "error": error.to_string()}));
            Vec::new()
        }
    };
    let locks = match postgres_lock_rows(runtime).await {
        Ok(value) => value,
        Err(error) => {
            errors.push(json!({"source": "pg_locks", "error": error.to_string()}));
            Vec::new()
        }
    };
    let table_activity = match postgres_table_activity_rows(runtime).await {
        Ok(value) => value,
        Err(error) => {
            errors.push(json!({"source": "pg_stat_user_tables", "error": error.to_string()}));
            Vec::new()
        }
    };
    let table_rows = table_counts(runtime).await;
    json!({
        "ok": true,
        "url": runtime.store.database_url,
        "root": runtime.store.root.display().to_string(),
        "database": row.as_ref().and_then(|row| row.try_get::<String, _>("database_name").ok()).unwrap_or_default(),
        "user": row.as_ref().and_then(|row| row.try_get::<String, _>("user_name").ok()).unwrap_or_default(),
        "version": row.as_ref().and_then(|row| row.try_get::<String, _>("version").ok()).unwrap_or_default(),
        "pool": postgres_pool_payload(runtime),
        "statistics": statistics,
        "settings": settings,
        "activity": activity,
        "locks": locks,
        "tables": table_rows,
        "tableActivity": table_activity,
        "errors": errors,
    })
}

fn postgres_pool_payload(runtime: &Ctx) -> Value {
    let open_connections = u64::from(runtime.store.pool.size());
    let idle_connections = runtime.store.pool.num_idle() as u64;
    json!({
        "configuredMaxConnections": runtime.store.pool.options().get_max_connections(),
        "openConnections": open_connections,
        "idleConnections": idle_connections,
        "inUseConnections": open_connections - idle_connections,
        "closed": runtime.store.pool.is_closed(),
    })
}

async fn postgres_database_statistics(runtime: &Ctx) -> Result<Value> {
    let row = sqlx::query(
        r#"
        SELECT
            numbackends::BIGINT AS numbackends,
            xact_commit,
            xact_rollback,
            blks_read,
            blks_hit,
            tup_returned,
            tup_fetched,
            tup_inserted,
            tup_updated,
            tup_deleted,
            conflicts,
            temp_files,
            temp_bytes,
            deadlocks,
            blk_read_time,
            blk_write_time,
            stats_reset,
            pg_database_size(current_database())::BIGINT AS database_size_bytes
        FROM pg_stat_database
        WHERE datname = current_database()
        "#,
    )
    .fetch_one(&runtime.store.pool)
    .await?;

    let commits = row.try_get::<i64, _>("xact_commit")?;
    let rollbacks = row.try_get::<i64, _>("xact_rollback")?;
    let block_hits = row.try_get::<i64, _>("blks_hit")?;
    let blocks_read = row.try_get::<i64, _>("blks_read")?;
    let transactions = commits + rollbacks;
    let block_accesses = block_hits + blocks_read;
    let stats_reset = row.try_get::<Option<DateTime<Utc>>, _>("stats_reset")?;

    Ok(json!({
        "databaseSizeBytes": row.try_get::<i64, _>("database_size_bytes")?,
        "backends": row.try_get::<i64, _>("numbackends")?,
        "transactions": transactions,
        "commits": commits,
        "rollbacks": rollbacks,
        "rollbackPercent": ratio_percent(rollbacks, transactions),
        "blocksRead": blocks_read,
        "blockHits": block_hits,
        "cacheHitPercent": ratio_percent(block_hits, block_accesses),
        "tuplesReturned": row.try_get::<i64, _>("tup_returned")?,
        "tuplesFetched": row.try_get::<i64, _>("tup_fetched")?,
        "tuplesInserted": row.try_get::<i64, _>("tup_inserted")?,
        "tuplesUpdated": row.try_get::<i64, _>("tup_updated")?,
        "tuplesDeleted": row.try_get::<i64, _>("tup_deleted")?,
        "conflicts": row.try_get::<i64, _>("conflicts")?,
        "tempFiles": row.try_get::<i64, _>("temp_files")?,
        "tempBytes": row.try_get::<i64, _>("temp_bytes")?,
        "deadlocks": row.try_get::<i64, _>("deadlocks")?,
        "blockReadMillis": row.try_get::<f64, _>("blk_read_time")?,
        "blockWriteMillis": row.try_get::<f64, _>("blk_write_time")?,
        "statsResetAt": stats_reset.map(|time| isoformat_z(Some(time))).unwrap_or_default(),
    }))
}

async fn postgres_settings(runtime: &Ctx) -> Result<Vec<Value>> {
    let rows = sqlx::query(
        r#"
        SELECT name, setting, COALESCE(unit, '') AS unit
        FROM pg_settings
        WHERE name IN (
            'max_connections',
            'shared_buffers',
            'effective_cache_size',
            'work_mem',
            'maintenance_work_mem',
            'track_io_timing'
        )
        ORDER BY name
        "#,
    )
    .fetch_all(&runtime.store.pool)
    .await?;

    rows.into_iter()
        .map(|row| {
            Ok(json!({
                "name": row.try_get::<String, _>("name")?,
                "setting": row.try_get::<String, _>("setting")?,
                "unit": row.try_get::<String, _>("unit")?,
            }))
        })
        .collect()
}

async fn postgres_activity_rows(runtime: &Ctx) -> Result<Vec<Value>> {
    let rows = sqlx::query(
        r#"
        SELECT
            COALESCE(state, 'unknown') AS state,
            COUNT(*)::BIGINT AS connections,
            COUNT(*) FILTER (WHERE wait_event_type IS NOT NULL)::BIGINT AS waiting,
            COALESCE(MAX(EXTRACT(EPOCH FROM now() - query_start))::BIGINT, 0) AS oldest_query_seconds,
            COALESCE(MAX(EXTRACT(EPOCH FROM now() - xact_start))::BIGINT, 0) AS oldest_transaction_seconds
        FROM pg_stat_activity
        WHERE datname = current_database()
        GROUP BY COALESCE(state, 'unknown')
        ORDER BY connections DESC, state
        "#,
    )
    .fetch_all(&runtime.store.pool)
    .await?;

    rows.into_iter()
        .map(|row| {
            Ok(json!({
                "state": row.try_get::<String, _>("state")?,
                "connections": row.try_get::<i64, _>("connections")?,
                "waiting": row.try_get::<i64, _>("waiting")?,
                "oldestQuerySeconds": row.try_get::<i64, _>("oldest_query_seconds")?,
                "oldestTransactionSeconds": row.try_get::<i64, _>("oldest_transaction_seconds")?,
            }))
        })
        .collect()
}

async fn postgres_lock_rows(runtime: &Ctx) -> Result<Vec<Value>> {
    let rows = sqlx::query(
        r#"
        SELECT mode, granted, COUNT(*)::BIGINT AS locks
        FROM pg_locks
        WHERE database = (
            SELECT oid FROM pg_database WHERE datname = current_database()
        )
        GROUP BY mode, granted
        ORDER BY locks DESC, mode, granted DESC
        "#,
    )
    .fetch_all(&runtime.store.pool)
    .await?;

    rows.into_iter()
        .map(|row| {
            Ok(json!({
                "mode": row.try_get::<String, _>("mode")?,
                "granted": row.try_get::<bool, _>("granted")?,
                "locks": row.try_get::<i64, _>("locks")?,
            }))
        })
        .collect()
}

async fn postgres_table_activity_rows(runtime: &Ctx) -> Result<Vec<Value>> {
    let rows = sqlx::query(
        r#"
        SELECT
            relname AS table_name,
            n_live_tup,
            n_dead_tup,
            seq_scan,
            idx_scan,
            n_tup_ins,
            n_tup_upd,
            n_tup_del,
            vacuum_count,
            autovacuum_count,
            analyze_count,
            autoanalyze_count,
            last_vacuum,
            last_autovacuum,
            last_analyze,
            last_autoanalyze,
            pg_total_relation_size(relid)::BIGINT AS total_bytes,
            pg_relation_size(relid)::BIGINT AS heap_bytes,
            pg_indexes_size(relid)::BIGINT AS index_bytes
        FROM pg_stat_user_tables
        WHERE schemaname = current_schema()
        ORDER BY pg_total_relation_size(relid) DESC, n_dead_tup DESC, relname
        "#,
    )
    .fetch_all(&runtime.store.pool)
    .await?;

    rows.into_iter()
        .map(|row| {
            let last_vacuum = row.try_get::<Option<DateTime<Utc>>, _>("last_vacuum")?;
            let last_autovacuum = row.try_get::<Option<DateTime<Utc>>, _>("last_autovacuum")?;
            let last_analyze = row.try_get::<Option<DateTime<Utc>>, _>("last_analyze")?;
            let last_autoanalyze = row.try_get::<Option<DateTime<Utc>>, _>("last_autoanalyze")?;
            Ok(json!({
                "table": row.try_get::<String, _>("table_name")?,
                "liveRows": row.try_get::<i64, _>("n_live_tup")?,
                "deadRows": row.try_get::<i64, _>("n_dead_tup")?,
                "seqScans": row.try_get::<i64, _>("seq_scan")?,
                "indexScans": row.try_get::<i64, _>("idx_scan")?,
                "inserts": row.try_get::<i64, _>("n_tup_ins")?,
                "updates": row.try_get::<i64, _>("n_tup_upd")?,
                "deletes": row.try_get::<i64, _>("n_tup_del")?,
                "vacuumCount": row.try_get::<i64, _>("vacuum_count")?,
                "autovacuumCount": row.try_get::<i64, _>("autovacuum_count")?,
                "analyzeCount": row.try_get::<i64, _>("analyze_count")?,
                "autoanalyzeCount": row.try_get::<i64, _>("autoanalyze_count")?,
                "lastVacuumAt": last_vacuum.map(|time| isoformat_z(Some(time))).unwrap_or_default(),
                "lastAutovacuumAt": last_autovacuum.map(|time| isoformat_z(Some(time))).unwrap_or_default(),
                "lastAnalyzeAt": last_analyze.map(|time| isoformat_z(Some(time))).unwrap_or_default(),
                "lastAutoanalyzeAt": last_autoanalyze.map(|time| isoformat_z(Some(time))).unwrap_or_default(),
                "totalBytes": row.try_get::<i64, _>("total_bytes")?,
                "heapBytes": row.try_get::<i64, _>("heap_bytes")?,
                "indexBytes": row.try_get::<i64, _>("index_bytes")?,
            }))
        })
        .collect()
}

fn ratio_percent(part: i64, total: i64) -> Option<f64> {
    if total <= 0 {
        return None;
    }
    Some(round3((part as f64 / total as f64) * 100.0))
}

fn observed_tables() -> &'static [&'static str] {
    &[
        "voice_rooms",
        "bot_states",
        "assignments",
        "occupancy",
        "voice_states",
        "discord_member_cache_refreshes",
        "discord_members",
        "capture_runs",
        "capture_sessions",
        "timeline_events",
        "conversations",
        "windows",
        "publications",
        "jobs",
        "job_payloads",
        "operational_job_outcomes",
        "job_dependencies",
        "automations",
    ]
}

async fn table_counts(runtime: &Ctx) -> Vec<Value> {
    let mut rows = Vec::new();
    for table in observed_tables() {
        let result = sqlx::query(&format!(
            "SELECT COUNT(*) AS row_count, pg_total_relation_size('{table}'::regclass)::BIGINT AS total_bytes FROM {table}"
        ))
            .fetch_one(&runtime.store.pool)
            .await;
        match result {
            Ok(row) => match (
                row.try_get::<i64, _>("row_count"),
                row.try_get::<i64, _>("total_bytes"),
            ) {
                (Ok(row_count), Ok(total_bytes)) => rows.push(json!({
                    "table": table,
                    "rows": row_count,
                    "totalBytes": total_bytes,
                })),
                (Err(error), _) | (_, Err(error)) => rows.push(json!({
                    "table": table,
                    "error": error.to_string(),
                })),
            },
            Err(error) => rows.push(json!({
                "table": table,
                "error": error.to_string(),
            })),
        }
    }
    rows
}

async fn operational_diagnostics(
    runtime: &Ctx,
    now: DateTime<Utc>,
) -> Result<OperationalDiagnostics> {
    let since_ms = instant_ms_dt(now - chrono::Duration::seconds(max_health_window_seconds()));
    let job_rows = diagnostic_job_rows(runtime, since_ms).await?;
    let event_rows = diagnostic_event_rows(runtime, since_ms).await?;
    let coverage_start_ms = operational_coverage_start_ms(runtime).await?;
    let failure_summary = failure_summary(runtime, &job_rows, coverage_start_ms, now).await?;
    let payload = json!({
        "coverage": {
            "startsAt": ms_iso(coverage_start_ms),
            "ageSeconds": age_seconds(instant_ms_dt(now), coverage_start_ms),
            "retentionSeconds": OPERATIONAL_JOB_OUTCOME_RETENTION_SECONDS,
        },
        "backlog": backlog_payload(&job_rows, now),
        "windows": operational_windows(&job_rows, &event_rows, coverage_start_ms, now),
        "latencies": latency_payload(&job_rows, coverage_start_ms, now),
        "failures": failure_summary,
    });
    Ok(OperationalDiagnostics {
        payload,
        job_rows,
        failure_summary,
    })
}

pub(super) async fn dashboard_latency_by_kind_payload(
    runtime: &Ctx,
    now: DateTime<Utc>,
) -> Result<Value> {
    let now_ms = instant_ms_dt(now);
    let since_ms = now_ms - FAILURE_WINDOW_SECONDS * 1000;
    let coverage_start_ms = operational_coverage_start_ms(runtime).await?;
    let rows = sqlx::query(
        r#"
        SELECT job_id, kind, state, lane, created_at_ms,
               observed_at_ms AS updated_at_ms, ready_at_ms, started_at_ms,
               completed_at_ms, failed
        FROM operational_job_outcomes
        WHERE observed_at_ms >= $1
        ORDER BY observed_at_ms DESC, observation_id DESC
        "#,
    )
    .bind(since_ms)
    .fetch_all(&runtime.store.pool)
    .await?
    .into_iter()
    .map(|row| {
        Ok(JobDiagnosticRow {
            job_id: row.try_get("job_id")?,
            kind: row.try_get("kind")?,
            state: row.try_get("state")?,
            lane: row.try_get("lane")?,
            created_at_ms: row.try_get("created_at_ms")?,
            updated_at_ms: row.try_get("updated_at_ms")?,
            ready_at_ms: row.try_get("ready_at_ms")?,
            started_at_ms: row.try_get("started_at_ms")?,
            completed_at_ms: row.try_get("completed_at_ms")?,
            terminal: true,
            failed: row.try_get("failed")?,
            cancellable: false,
        })
    })
    .collect::<Result<Vec<_>>>()?;
    let coverage = window_coverage_payload(coverage_start_ms, since_ms, now_ms);
    let failures = detailed_failure_summary(runtime, now).await?;
    Ok(json!({
        "coverage": {
            "startsAt": ms_iso(coverage_start_ms),
            "ageSeconds": age_seconds(now_ms, coverage_start_ms),
            "retentionSeconds": OPERATIONAL_JOB_OUTCOME_RETENTION_SECONDS,
        },
        "latencies": {
            "windows": [{
                "label": "1h",
                "since": ms_iso(since_ms),
                "coverage": coverage,
            }],
            "byKind": latency_by_kind(&rows, since_ms),
        },
        "failures": failures,
    }))
}

async fn diagnostic_job_rows(runtime: &Ctx, since_ms: i64) -> Result<Vec<JobDiagnosticRow>> {
    let rows = sqlx::query(
        r#"
        SELECT job_id, kind, state, lane,
               created_at_ms, updated_at_ms, ready_at_ms, started_at_ms,
               completed_at_ms, terminal, failed, cancellable
        FROM (
          SELECT j.job_id, j.kind, j.state, j.lane, j.created_at_ms,
                 j.updated_at_ms, j.ready_at_ms,
                 j.started_at_ms, j.completed_at_ms, j.terminal, j.failed,
                 j.cancellable
          FROM jobs j
          WHERE j.terminal = FALSE
          UNION ALL
          SELECT outcome.job_id, outcome.kind, outcome.state, outcome.lane,
                 outcome.created_at_ms, outcome.observed_at_ms AS updated_at_ms,
                 outcome.ready_at_ms, outcome.started_at_ms,
                 outcome.completed_at_ms, TRUE AS terminal, outcome.failed,
                 FALSE AS cancellable
          FROM operational_job_outcomes outcome
          WHERE outcome.observed_at_ms >= $1
        ) observed_jobs
        ORDER BY updated_at_ms DESC
        "#,
    )
    .bind(since_ms)
    .fetch_all(&runtime.store.pool)
    .await?;

    rows.into_iter()
        .map(|row| {
            Ok(JobDiagnosticRow {
                job_id: row.try_get("job_id")?,
                kind: row.try_get("kind")?,
                state: row.try_get("state")?,
                lane: row.try_get("lane")?,
                created_at_ms: row.try_get("created_at_ms")?,
                updated_at_ms: row.try_get("updated_at_ms")?,
                ready_at_ms: row.try_get("ready_at_ms")?,
                started_at_ms: row.try_get("started_at_ms")?,
                completed_at_ms: row.try_get("completed_at_ms")?,
                terminal: row.try_get("terminal")?,
                failed: row.try_get("failed")?,
                cancellable: row.try_get("cancellable")?,
            })
        })
        .collect()
}

async fn operational_coverage_start_ms(runtime: &Ctx) -> Result<i64> {
    let value =
        sqlx::query_scalar::<_, String>("SELECT value FROM runtime_metadata WHERE key = $1")
            .bind(OPERATIONAL_COVERAGE_START_KEY)
            .fetch_one(&runtime.store.pool)
            .await?;
    value.parse::<i64>().map_err(|error| {
        anyhow::anyhow!(
            "runtime metadata {OPERATIONAL_COVERAGE_START_KEY} contains invalid milliseconds `{value}`: {error}"
        )
    })
}

async fn failure_summary(
    runtime: &Ctx,
    job_rows: &[JobDiagnosticRow],
    coverage_start_ms: i64,
    now: DateTime<Utc>,
) -> Result<Value> {
    let now_ms = instant_ms_dt(now);
    let since_ms = now_ms - FAILURE_WINDOW_SECONDS * 1000;
    let count = job_rows
        .iter()
        .filter(|row| {
            row.is_failed()
                && row
                    .terminal_at_ms()
                    .is_some_and(|failed_at_ms| failed_at_ms >= since_ms)
        })
        .count();
    let recent = recent_failure_rows(runtime, since_ms, FAILURE_RECENT_LIMIT).await?;
    Ok(json!({
        "window": "1h",
        "since": ms_iso(since_ms),
        "count": count,
        "complete": coverage_start_ms <= since_ms,
        "coverageStartsAt": ms_iso(coverage_start_ms),
        "recent": recent.into_iter().map(|row| row.to_json()).collect::<Vec<_>>(),
    }))
}

async fn recent_failure_rows(
    runtime: &Ctx,
    since_ms: i64,
    limit: i64,
) -> Result<Vec<FailureDiagnosticRow>> {
    let mut failures = Vec::new();
    let outcome_rows = sqlx::query(
        r#"
        SELECT job_id, scope_kind, guild_id, scope_id, kind, state,
               error_text, observed_at_ms
        FROM operational_job_outcomes
        WHERE failed = TRUE
          AND observed_at_ms >= $1
        ORDER BY observed_at_ms DESC, observation_id DESC
        LIMIT $2
        "#,
    )
    .bind(since_ms)
    .bind(limit)
    .fetch_all(&runtime.store.pool)
    .await?;
    for row in outcome_rows {
        failures.push(FailureDiagnosticRow {
            job_id: row.try_get("job_id")?,
            scope_kind: row.try_get("scope_kind")?,
            guild_id: row.try_get("guild_id")?,
            scope_id: row.try_get("scope_id")?,
            scope_label: String::new(),
            kind: row.try_get("kind")?,
            state: row.try_get("state")?,
            reason: row.try_get("error_text")?,
            failed_at_ms: row.try_get("observed_at_ms")?,
        });
    }
    let scope_keys = failures
        .iter()
        .map(|failure| {
            (
                failure.scope_kind.clone(),
                failure.guild_id.clone(),
                failure.scope_id.clone(),
            )
        })
        .collect::<Vec<_>>();
    let scope_labels = dashboard::dashboard_scope_label_batch(runtime, &scope_keys).await?;
    for failure in &mut failures {
        failure.scope_label = scope_labels
            .get(&(
                failure.scope_kind.clone(),
                failure.guild_id.clone(),
                failure.scope_id.clone(),
            ))
            .expect("failure scope label was resolved")
            .clone();
    }
    failures.sort_by(|left, right| {
        right
            .failed_at_ms
            .cmp(&left.failed_at_ms)
            .then_with(|| right.job_id.cmp(&left.job_id))
    });
    failures.truncate(limit as usize);
    Ok(failures)
}

async fn diagnostic_event_rows(runtime: &Ctx, since_ms: i64) -> Result<Vec<EventDiagnosticRow>> {
    let rows = sqlx::query(
        r#"
        SELECT event_kind, started_at_ms AS at_ms,
               ended_at_ms, speaker_user_id
        FROM timeline_events
        WHERE forgotten = FALSE
          AND started_at_ms >= $1
          AND (
            event_kind IN (
              'speech_segment',
              'transcript',
              'wake_detected',
              'wake_activation_dispatched',
              'wake_activation_amended',
              'wake_activation_replaced',
              'wake_activation_ignored',
              'wake_activation_window_closed'
            )
            OR event_kind LIKE 'wake_%'
          )
        ORDER BY started_at_ms DESC
        "#,
    )
    .bind(since_ms)
    .fetch_all(&runtime.store.pool)
    .await?;

    rows.into_iter()
        .map(|row| {
            Ok(EventDiagnosticRow {
                event_kind: row.try_get("event_kind")?,
                at_ms: row.try_get("at_ms")?,
                ended_at_ms: row.try_get("ended_at_ms")?,
                speaker_user_id: row.try_get("speaker_user_id")?,
            })
        })
        .collect()
}

fn backlog_payload(rows: &[JobDiagnosticRow], now: DateTime<Utc>) -> Value {
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

    for row in rows.iter().filter(|row| row.is_active()) {
        total += 1;
        *by_state.entry(row.state.clone()).or_insert(0) += 1;
        *by_kind_state
            .entry((row.kind.clone(), row.state.clone()))
            .or_insert(0) += 1;
        *by_lane_state
            .entry((row.lane.clone(), row.state.clone()))
            .or_insert(0) += 1;
        if row.cancellable {
            cancellable += 1;
        }
        oldest_active_age_seconds =
            oldest_active_age_seconds.max(age_seconds(now_ms, row.created_at_ms));
        match row.state.as_str() {
            "queued" => {
                queued += 1;
                if row.ready_at_ms <= now_ms {
                    due_queued += 1;
                }
                oldest_queued_age_seconds =
                    oldest_queued_age_seconds.max(age_seconds(now_ms, row.created_at_ms));
            }
            "running" => {
                running += 1;
                oldest_running_age_seconds = oldest_running_age_seconds.max(age_seconds(
                    now_ms,
                    row.started_at_ms.unwrap_or(row.created_at_ms),
                ));
            }
            "waiting" => waiting += 1,
            "cancel_requested" => cancel_requested += 1,
            "confirmation_pending" => confirmation_pending += 1,
            _ => {}
        }
        let entry = by_kind
            .entry(row.kind.clone())
            .or_insert_with(|| BacklogKindSummary {
                kind: row.kind.clone(),
                ..BacklogKindSummary::default()
            });
        entry.add(row, now_ms);
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

fn operational_windows(
    jobs: &[JobDiagnosticRow],
    events: &[EventDiagnosticRow],
    coverage_start_ms: i64,
    now: DateTime<Utc>,
) -> Vec<Value> {
    let now_ms = instant_ms_dt(now);
    HEALTH_WINDOWS
        .iter()
        .map(|(label, seconds)| {
            let since_ms = now_ms - (seconds * 1000);
            let window_events = events
                .iter()
                .filter(|event| event.at_ms >= since_ms)
                .collect::<Vec<_>>();
            let speakers = window_events
                .iter()
                .filter_map(|event| {
                    (!event.speaker_user_id.trim().is_empty())
                        .then(|| event.speaker_user_id.clone())
                })
                .collect::<BTreeSet<_>>();
            let speech_audio_ms = window_events
                .iter()
                .filter(|event| event.event_kind == "speech_segment")
                .filter_map(|event| event.ended_at_ms.map(|ended| ended - event.at_ms))
                .filter(|duration| *duration > 0)
                .sum::<i64>();

            json!({
                "label": *label,
                "since": ms_iso(since_ms),
                "coverage": window_coverage_payload(coverage_start_ms, since_ms, now_ms),
                "allJobs": job_window_counts(jobs, since_ms, None),
                "audioSegmentJobs": job_window_counts(jobs, since_ms, Some("audio_segment")),
                "wakeProbeJobs": job_window_counts(jobs, since_ms, Some("wake_probe")),
                "wakeActivationJobs": job_window_counts(jobs, since_ms, Some("wake_activation")),
                "events": {
                    "speechSegments": window_events.iter().filter(|event| event.event_kind == "speech_segment").count(),
                    "transcripts": window_events.iter().filter(|event| event.event_kind == "transcript").count(),
                    "wakeDetected": window_events.iter().filter(|event| event.event_kind == "wake_detected").count(),
                    "wakeActivationDispatched": window_events.iter().filter(|event| event.event_kind == "wake_activation_dispatched").count(),
                    "wakeEvents": window_events.iter().filter(|event| event.event_kind.starts_with("wake_")).count(),
                    "speakers": speakers.len(),
                    "speechAudioMs": speech_audio_ms,
                },
            })
        })
        .collect()
}

fn job_window_counts(rows: &[JobDiagnosticRow], since_ms: i64, kind: Option<&str>) -> Value {
    let mut total = 0_usize;
    let mut active = 0_usize;
    let mut queued = 0_usize;
    let mut running = 0_usize;
    let mut waiting = 0_usize;
    let mut completed = 0_usize;
    let mut failed = 0_usize;
    let mut latest_ms = None::<i64>;

    for row in rows
        .iter()
        .filter(|row| row.activity_ms() >= since_ms && kind.is_none_or(|kind| row.kind == kind))
    {
        total += 1;
        if row.is_active() {
            active += 1;
        }
        match row.state.as_str() {
            "queued" => queued += 1,
            "running" => running += 1,
            "waiting" => waiting += 1,
            "complete" => completed += 1,
            _ => {}
        }
        if row.is_failed() {
            failed += 1;
        }
        let activity_ms = row.activity_ms();
        latest_ms = Some(latest_ms.map_or(activity_ms, |latest| latest.max(activity_ms)));
    }

    json!({
        "total": total,
        "active": active,
        "queued": queued,
        "running": running,
        "waiting": waiting,
        "completed": completed,
        "failed": failed,
        "latestAt": latest_ms.map(ms_iso).unwrap_or_default(),
    })
}

fn latency_payload(rows: &[JobDiagnosticRow], coverage_start_ms: i64, now: DateTime<Utc>) -> Value {
    let now_ms = instant_ms_dt(now);
    let windows = HEALTH_WINDOWS
        .iter()
        .map(|(label, seconds)| {
            let since_ms = now_ms - (seconds * 1000);
            json!({
                "label": *label,
                "since": ms_iso(since_ms),
                "coverage": window_coverage_payload(coverage_start_ms, since_ms, now_ms),
                "all": latency_stats_for(rows, since_ms, None),
                "stt": latency_stats_for(rows, since_ms, Some("audio_segment")),
                "wakeword": latency_stats_for(rows, since_ms, Some("wake_probe")),
            })
        })
        .collect::<Vec<_>>();
    let since_ms = now_ms - (max_health_window_seconds() * 1000);
    json!({
        "windows": windows,
        "byKind": latency_by_kind(rows, since_ms),
    })
}

fn latency_by_kind(rows: &[JobDiagnosticRow], since_ms: i64) -> Vec<Value> {
    let kinds = rows
        .iter()
        .filter(|row| {
            row.terminal_at_ms()
                .is_some_and(|terminal_at| terminal_at >= since_ms)
        })
        .map(|row| row.kind.clone())
        .collect::<BTreeSet<_>>();
    let mut values = kinds
        .into_iter()
        .map(|kind| {
            let mut value = latency_stats_for(rows, since_ms, Some(&kind));
            if let Value::Object(object) = &mut value {
                object.insert("kind".to_string(), json!(kind));
            }
            value
        })
        .collect::<Vec<_>>();
    values.sort_by(|left, right| {
        json_usize(right, "count")
            .cmp(&json_usize(left, "count"))
            .then_with(|| string_field(left, "kind").cmp(&string_field(right, "kind")))
    });
    values.truncate(16);
    values
}

fn latency_stats_for(rows: &[JobDiagnosticRow], since_ms: i64, kind: Option<&str>) -> Value {
    let mut ready_delay_ms = Vec::new();
    let mut queue_ms = Vec::new();
    let mut run_ms = Vec::new();
    let mut total_ms = Vec::new();
    let mut excluded = LatencyExclusions::default();
    let mut count = 0_usize;
    let mut failed = 0_usize;
    let mut latest_ms = None::<i64>;

    for row in rows.iter().filter(|row| {
        kind.is_none_or(|kind| row.kind == kind)
            && row
                .terminal_at_ms()
                .is_some_and(|terminal_at_ms| terminal_at_ms >= since_ms)
    }) {
        let terminal_at_ms = row
            .terminal_at_ms()
            .expect("latency row has a terminal observation time");
        count += 1;
        if row.is_failed() {
            failed += 1;
        }
        let mut invalid_timestamp_order = false;
        if terminal_at_ms >= row.created_at_ms {
            total_ms.push(terminal_at_ms - row.created_at_ms);
        } else {
            excluded.total_ms += 1;
            invalid_timestamp_order = true;
        }
        let phase_contaminated = row
            .started_at_ms
            .is_some_and(|started_at_ms| started_at_ms < row.ready_at_ms);
        if phase_contaminated {
            excluded.phase_contaminated += 1;
        }
        if row.ready_at_ms >= row.created_at_ms && !phase_contaminated {
            ready_delay_ms.push(row.ready_at_ms - row.created_at_ms);
        } else {
            excluded.ready_delay_ms += 1;
            invalid_timestamp_order |= row.ready_at_ms < row.created_at_ms;
        }
        match row.started_at_ms {
            Some(started_at_ms) => {
                if started_at_ms >= row.ready_at_ms {
                    queue_ms.push(started_at_ms - row.ready_at_ms);
                } else {
                    excluded.queue_ms += 1;
                    invalid_timestamp_order |= !phase_contaminated;
                }
                if terminal_at_ms >= started_at_ms {
                    run_ms.push(terminal_at_ms - started_at_ms);
                } else {
                    excluded.run_ms += 1;
                    invalid_timestamp_order = true;
                }
            }
            None => {
                excluded.missing_started_at += 1;
                excluded.queue_ms += 1;
                excluded.run_ms += 1;
            }
        }
        if invalid_timestamp_order {
            excluded.invalid_timestamp_order += 1;
        }
        latest_ms = Some(latest_ms.map_or(terminal_at_ms, |latest| latest.max(terminal_at_ms)));
    }

    json!({
        "count": count,
        "failed": failed,
        "readyDelayMs": latency_metric(ready_delay_ms),
        "queueMs": latency_metric(queue_ms),
        "runMs": latency_metric(run_ms),
        "totalMs": latency_metric(total_ms),
        "excluded": excluded.to_json(),
        "latestAt": latest_ms.map(ms_iso).unwrap_or_default(),
    })
}

fn window_coverage_payload(coverage_start_ms: i64, since_ms: i64, now_ms: i64) -> Value {
    let covered_from_ms = coverage_start_ms.max(since_ms);
    json!({
        "complete": coverage_start_ms <= since_ms,
        "startsAt": ms_iso(coverage_start_ms),
        "coveredSeconds": ((now_ms - covered_from_ms) / 1000).max(0),
    })
}

fn latency_metric(mut values: Vec<i64>) -> Value {
    values.sort_unstable();
    if values.is_empty() {
        return json!({
            "count": 0,
            "p50": Value::Null,
            "p95": Value::Null,
            "max": Value::Null,
        });
    }
    json!({
        "count": values.len(),
        "p50": percentile(&values, 50),
        "p95": percentile(&values, 95),
        "max": values[values.len() - 1],
    })
}

fn percentile(values: &[i64], percentile: usize) -> i64 {
    let rank = ((percentile as f64 / 100.0) * values.len() as f64).ceil() as usize;
    values[rank.saturating_sub(1).min(values.len() - 1)]
}

fn max_health_window_seconds() -> i64 {
    HEALTH_WINDOWS
        .iter()
        .map(|(_, seconds)| *seconds)
        .max()
        .expect("health windows are configured")
}

fn age_seconds(now_ms: i64, then_ms: i64) -> i64 {
    ((now_ms - then_ms) / 1000).max(0)
}

fn ms_iso(value: i64) -> String {
    ms_to_datetime(value)
        .map(|instant| isoformat_z(Some(instant)))
        .expect("health timestamp is representable")
}

fn count_pair_rows(
    counts: BTreeMap<(String, String), usize>,
    left_key: &str,
    right_key: &str,
) -> Vec<Value> {
    let mut rows = counts
        .into_iter()
        .map(|((left, right), count)| {
            let mut object = Map::new();
            object.insert(left_key.to_string(), json!(left));
            object.insert(right_key.to_string(), json!(right));
            object.insert("count".to_string(), json!(count));
            Value::Object(object)
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        json_usize(right, "count")
            .cmp(&json_usize(left, "count"))
            .then_with(|| string_field(left, left_key).cmp(&string_field(right, left_key)))
            .then_with(|| string_field(left, right_key).cmp(&string_field(right, right_key)))
    });
    rows
}

fn json_usize(value: &Value, key: &str) -> usize {
    value.get(key).and_then(Value::as_u64).unwrap_or(0) as usize
}

pub(super) fn load_payload(jobs: &[Job], now: DateTime<Utc>) -> Value {
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
            if job
                .next_run_at
                .as_deref()
                .and_then(parse_instant)
                .is_none_or(|due| due <= now)
            {
                due_queued += 1;
            }
            if let Some(created_at) = parse_instant(&job.created_at) {
                oldest_queued_age_seconds =
                    oldest_queued_age_seconds.max((now - created_at).num_seconds());
            }
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

pub(super) fn agent_usage_payload(jobs: &[Job], now: DateTime<Utc>) -> Value {
    codex_usage_rollup(jobs, now)
}

pub(super) fn automation_dashboard_payload(records: &[AutomationRecord]) -> Value {
    let mut by_state = BTreeMap::<String, usize>::new();
    let mut by_trigger = BTreeMap::<String, usize>::new();
    let mut active = 0_usize;
    let mut fired = 0_usize;
    for record in records {
        let state = format!("{:?}", record.state).to_lowercase();
        *by_state.entry(state.clone()).or_insert(0) += 1;
        if state == "active" {
            active += 1;
        }
        if record.fire_count > 0 {
            fired += 1;
        }
        *by_trigger
            .entry(automation_trigger_kind(&record.spec.trigger).to_string())
            .or_insert(0) += 1;
    }
    json!({
        "records": records.iter().map(AutomationRecord::to_json).collect::<Vec<_>>(),
        "summary": {
            "total": records.len(),
            "active": active,
            "fired": fired,
            "byState": count_rows(by_state, "state"),
            "byTrigger": count_rows(by_trigger, "trigger"),
        },
    })
}

fn automation_trigger_kind(trigger: &AutomationTrigger) -> &'static str {
    match trigger {
        AutomationTrigger::Tick { .. } => "tick",
        AutomationTrigger::Event { .. } => "event",
        AutomationTrigger::Job { .. } => "job",
        AutomationTrigger::RoomStateChanged => "room_state_changed",
    }
}

#[derive(Debug, Clone)]
struct CodexUsageWindow {
    label: &'static str,
    since: DateTime<Utc>,
    jobs: usize,
    jobs_with_usage: usize,
    input_tokens: i64,
    cached_input_tokens: i64,
    output_tokens: i64,
    reasoning_output_tokens: i64,
    latest_at: Option<DateTime<Utc>>,
}

impl CodexUsageWindow {
    fn new(label: &'static str, since: DateTime<Utc>) -> Self {
        Self {
            label,
            since,
            jobs: 0,
            jobs_with_usage: 0,
            input_tokens: 0,
            cached_input_tokens: 0,
            output_tokens: 0,
            reasoning_output_tokens: 0,
            latest_at: None,
        }
    }

    fn add_job(&mut self, at: DateTime<Utc>, usage: &Value) {
        self.jobs += 1;
        self.latest_at = Some(self.latest_at.map_or(at, |latest| latest.max(at)));
        if !usage.as_object().is_none_or(Map::is_empty) {
            self.jobs_with_usage += 1;
            self.input_tokens += usage_token_field(usage, "input_tokens");
            self.cached_input_tokens += usage_token_field(usage, "cached_input_tokens");
            self.output_tokens += usage_token_field(usage, "output_tokens");
            self.reasoning_output_tokens += usage_token_field(usage, "reasoning_output_tokens");
        }
    }

    fn to_json(&self) -> Value {
        json!({
            "label": self.label,
            "since": isoformat_z(Some(self.since)),
            "jobs": self.jobs,
            "jobsWithUsage": self.jobs_with_usage,
            "inputTokens": self.input_tokens,
            "cachedInputTokens": self.cached_input_tokens,
            "outputTokens": self.output_tokens,
            "reasoningOutputTokens": self.reasoning_output_tokens,
            "latestAt": isoformat_z(self.latest_at),
        })
    }
}

fn codex_usage_rollup(jobs: &[Job], now: DateTime<Utc>) -> Value {
    let mut five_hour = CodexUsageWindow::new("5h", now - chrono::Duration::hours(5));
    let mut one_week = CodexUsageWindow::new("1w", now - chrono::Duration::days(7));
    let mut latest_rate_limits = Value::Null;

    for job in jobs.iter().filter(|job| job.kind == JobKind::AgentTask) {
        let usage = codex_usage_for_job(job);
        if latest_rate_limits.is_null() {
            let rate_limits = codex_rate_limits_for_job(job);
            if rate_limits_is_present(&rate_limits) {
                latest_rate_limits = rate_limits;
            }
        }
        let Some(at) = job_activity_instant(job) else {
            continue;
        };
        if at >= five_hour.since {
            five_hour.add_job(at, &usage);
        }
        if at >= one_week.since {
            one_week.add_job(at, &usage);
        }
    }

    json!({
        "source": "clankcord_agent_jobs",
        "globalLimitSource": if rate_limits_is_present(&latest_rate_limits) { "codex_rate_limits" } else { "not_reported_by_codex_cli" },
        "globalLimitsKnown": rate_limits_is_present(&latest_rate_limits),
        "rateLimits": latest_rate_limits,
        "windows": [
            five_hour.to_json(),
            one_week.to_json(),
        ],
    })
}

fn codex_usage_for_job(job: &Job) -> Value {
    let Some(metadata) = job.metadata.agent_task() else {
        return json!({});
    };
    if !metadata.agent.usage.is_empty() {
        return usage_payload_info(&metadata.agent.usage.to_json());
    }
    json!({})
}

fn codex_rate_limits_for_job(job: &Job) -> Value {
    let _ = job;
    Value::Null
}

fn rate_limits_is_present(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Object(object) => !object.is_empty(),
        Value::Array(values) => !values.is_empty(),
        _ => true,
    }
}

fn job_activity_instant(job: &Job) -> Option<DateTime<Utc>> {
    let timestamp = first_non_empty([
        job.completed_at.clone().unwrap_or_default(),
        job.updated_at.clone(),
        job.started_at.clone().unwrap_or_default(),
        job.created_at.clone(),
    ]);
    parse_instant(&timestamp)
}

fn usage_token_field(usage: &Value, key: &str) -> i64 {
    usage
        .get("total_token_usage")
        .and_then(|value| value.get(key))
        .and_then(Value::as_i64)
        .or_else(|| {
            usage
                .get("last_token_usage")
                .and_then(|value| value.get(key))
                .and_then(Value::as_i64)
        })
        .or_else(|| {
            usage
                .get("raw_usage")
                .and_then(|value| value.get(key))
                .and_then(Value::as_i64)
        })
        .or_else(|| usage.get(key).and_then(Value::as_i64))
        .unwrap_or(0)
}

fn agent_sessions_from_jobs(jobs: &[Job]) -> Vec<AgentSessionView> {
    let mut ordered = jobs
        .iter()
        .filter(|job| job.kind == JobKind::AgentTask)
        .collect::<Vec<_>>();
    ordered.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then_with(|| left.id.cmp(&right.id))
    });
    let mut sessions = BTreeMap::<String, AgentSessionView>::new();
    for job in ordered {
        let key = AgentRuntime::task_session_key(&job.guild_id, &job.scope_id);
        let entry = sessions
            .entry(key.clone())
            .or_insert_with(|| AgentSessionView {
                key,
                role: "task".to_string(),
                guild_id: job.guild_id.clone(),
                scope_id: job.scope_id.clone(),
                created_at: job.created_at.clone(),
                ..AgentSessionView::default()
            });
        entry.invocation_count += 1;
        entry.latest_job_id = job.id.clone();
        entry.last_used_at = job.updated_at.clone();
        if let Some(task) = job.metadata.agent_task() {
            if !task.agent.session_id.trim().is_empty() {
                entry.session_id = task.agent.session_id.clone();
            }
            if !task.dispatch_error.trim().is_empty() {
                entry.last_error = task.dispatch_error.clone();
            }
        }
        if !job.state.is_terminal() {
            entry.status = AgentSessionStatus::Running;
            entry.active_job_id = job.id.clone();
        } else if is_failed_state(job.state.as_str()) {
            entry.status = AgentSessionStatus::Failed;
            entry.active_job_id.clear();
        } else if entry.status != AgentSessionStatus::Running {
            entry.status = AgentSessionStatus::Idle;
            entry.active_job_id.clear();
        }
    }
    sessions.into_values().collect()
}

async fn agent_job_payload(runtime: &Ctx, job: &Job) -> Result<Value> {
    let metadata = job.metadata.agent_task().cloned().unwrap_or_default();
    let raw = read_text_artifact(&metadata.raw_result_path, AGENT_ARTIFACT_MAX_BYTES);
    let codex = parse_codex_trace(raw.get("content").and_then(Value::as_str).unwrap_or(""));
    let session_id = agent_job_session_id(job, &codex);
    let session = agent_session_payload(runtime, job, &codex).await?;
    Ok(json!({
        "job": job.to_value(),
        "paths": {
            "workdir": metadata.workdir_path,
            "prompt": metadata.prompt_path,
            "result": metadata.result_path,
            "raw": metadata.raw_result_path,
        },
        "workdir": workspace_artifact(&metadata.workdir_path),
        "prompt": read_text_artifact(&metadata.prompt_path, AGENT_ARTIFACT_MAX_BYTES),
        "result": read_text_artifact(&metadata.result_path, AGENT_ARTIFACT_MAX_BYTES),
        "raw": raw,
        "codex": codex,
        "trace": {
            "selectedJobId": job.id.clone(),
            "selectedSessionId": session_id,
        },
        "session": session,
    }))
}

async fn agent_session_payload(
    runtime: &Ctx,
    selected: &Job,
    selected_codex: &Value,
) -> Result<Value> {
    let key = AgentRuntime::task_session_key(&selected.guild_id, &selected.scope_id);
    let mut jobs = runtime
        .store
        .list_jobs_by_scope_kind(&selected.guild_id, &selected.scope_id, JobKind::AgentTask)
        .await?;
    let current = agent_sessions_from_jobs(&jobs)
        .into_iter()
        .find(|session| session.key == key)
        .map(|session| session.to_json());
    let selected_session_id = agent_job_session_id(selected, selected_codex);
    jobs.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then_with(|| left.id.cmp(&right.id))
    });
    let scope_job_count = jobs.len();
    let mut rows = jobs
        .iter()
        .filter(|job| agent_job_matches_selected_session(*job, selected, &selected_session_id))
        .map(agent_session_job_payload)
        .collect::<Vec<_>>();
    let total_job_count = rows.len();
    let truncated = rows.len() > AGENT_SESSION_JOB_LIMIT;
    if truncated {
        rows = rows[rows.len().saturating_sub(AGENT_SESSION_JOB_LIMIT)..].to_vec();
    }
    let scope = if selected_session_id.trim().is_empty() {
        "selected_job"
    } else {
        "codex_session"
    };
    Ok(json!({
        "key": key,
        "scope": scope,
        "selectedJobId": selected.id.clone(),
        "sessionId": selected_session_id,
        "current": current,
        "jobCount": rows.len(),
        "totalJobCount": total_job_count,
        "scopeJobCount": scope_job_count,
        "truncated": truncated,
        "jobs": rows,
    }))
}

fn agent_job_session_id(job: &Job, codex: &Value) -> String {
    non_empty(
        agent_job_metadata_session_id(job),
        string_field(codex, "sessionId"),
    )
}

fn agent_job_metadata_session_id(job: &Job) -> String {
    job.metadata
        .agent_task()
        .map(|task| task.agent.session_id.clone())
        .unwrap_or_default()
}

fn agent_job_matches_selected_session(
    job: &Job,
    selected: &Job,
    selected_session_id: &str,
) -> bool {
    if job.id == selected.id {
        return true;
    }
    !selected_session_id.trim().is_empty()
        && agent_job_metadata_session_id(job) == selected_session_id
}

fn agent_session_job_payload(job: &Job) -> Value {
    let metadata = job.metadata.agent_task();
    let request = job
        .command()
        .map(|command| command.arguments.request_text())
        .unwrap_or_default();
    let result_excerpt = metadata
        .map(|metadata| preview(&metadata.response_text, 1200))
        .unwrap_or_default();
    let error = preview(
        &first_non_empty([
            job.metadata.error.clone(),
            metadata
                .map(|metadata| metadata.dispatch_error_after_cancel.clone())
                .unwrap_or_default(),
            metadata
                .map(|metadata| metadata.dispatch_error.clone())
                .unwrap_or_default(),
        ]),
        1200,
    );
    json!({
        "job_id": job.id.clone(),
        "state": job.state.as_str(),
        "created_at": job.created_at.clone(),
        "updated_at": job.updated_at.clone(),
        "durationMs": dashboard_job_duration_ms(job),
        "attempts": job.attempts,
        "request": preview(&request, 1000),
        "resultExcerpt": result_excerpt,
        "error": error,
        "session_id": metadata.map(|metadata| metadata.agent.session_id.clone()).unwrap_or_default(),
        "model": metadata.map(|metadata| metadata.agent.model.clone()).unwrap_or_default(),
        "reasoning_effort": metadata.map(|metadata| metadata.agent.reasoning_effort.clone()).unwrap_or_default(),
        "fast_mode": metadata.is_some_and(|metadata| metadata.agent.fast_mode),
        "tokenUsage": metadata.map(|metadata| metadata.agent.usage.to_json()).unwrap_or_else(|| json!({})),
        "detailUrl": format!("/v1/dashboard/agents/{}", job.id),
    })
}

fn workspace_artifact(path: &str) -> Value {
    let path = path.trim();
    if path.is_empty() {
        return json!({"path": "", "exists": false, "files": []});
    }
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(_) => return json!({"path": path, "exists": false, "files": []}),
    };
    let mut files = entries
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let metadata = entry.metadata().ok()?;
            let name = entry.file_name().to_string_lossy().to_string();
            let mut object = serde_json::Map::new();
            object.insert("name".to_string(), json!(name));
            object.insert(
                "path".to_string(),
                json!(entry.path().display().to_string()),
            );
            object.insert("is_dir".to_string(), json!(metadata.is_dir()));
            object.insert("bytes".to_string(), json!(metadata.len()));
            if metadata.is_file() && metadata.len() <= 4096 {
                if let Ok(text) = fs::read_to_string(entry.path()) {
                    object.insert("preview".to_string(), json!(preview(&text, 1200)));
                }
            }
            Some(Value::Object(object))
        })
        .collect::<Vec<_>>();
    files.sort_by(|left, right| string_field(left, "name").cmp(&string_field(right, "name")));
    json!({"path": path, "exists": true, "files": files})
}

fn read_text_artifact(path: &str, max_bytes: usize) -> Value {
    let path = path.trim();
    if path.is_empty() {
        return json!({"path": "", "exists": false, "bytes": 0, "truncated": false, "content": ""});
    }
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(_) => {
            return json!({"path": path, "exists": false, "bytes": 0, "truncated": false, "content": ""});
        }
    };
    let truncated = bytes.len() > max_bytes;
    let visible = if truncated {
        &bytes[..max_bytes]
    } else {
        &bytes[..]
    };
    json!({
        "path": path,
        "exists": true,
        "bytes": bytes.len(),
        "truncated": truncated,
        "content": String::from_utf8_lossy(visible).to_string(),
    })
}

pub fn parse_codex_trace(raw: &str) -> Value {
    let events = parse_codex_jsonl(raw);
    let mut session_id = String::new();
    let mut model = String::new();
    let mut cli_version = String::new();
    let mut messages = Vec::new();
    let mut tool_calls = Vec::new();
    let mut timeline = Vec::new();
    let mut token_usage = Value::Object(Map::new());
    let mut rate_limits = Value::Null;
    let mut context_window = 0_i64;
    for event in &events {
        if let Some(usage) = codex_usage_payload(event.clone()) {
            token_usage = usage_payload_info(&usage);
            context_window = context_window_from_usage(&token_usage).unwrap_or(context_window);
        }
        match event.get("type").and_then(Value::as_str).unwrap_or("") {
            "session_meta" => {
                let payload = event.get("payload").unwrap_or(&Value::Null);
                session_id = non_empty(session_id, string_field(payload, "id"));
                model = non_empty(model, string_field(payload, "model"));
                cli_version = non_empty(cli_version, string_field(payload, "cli_version"));
            }
            "thread.started" => {
                session_id = non_empty(session_id, string_field(event, "thread_id"));
            }
            "item.started" | "item.completed" => {
                collect_current_item(event, &mut messages, &mut tool_calls, &mut timeline);
            }
            "response_item" => {
                collect_response_item(event, &mut messages, &mut tool_calls, &mut timeline)
            }
            "event_msg" => {
                let payload = event.get("payload").unwrap_or(&Value::Null);
                match payload.get("type").and_then(Value::as_str).unwrap_or("") {
                    "agent_message" => push_message(
                        &mut messages,
                        &mut timeline,
                        json!({
                            "role": "assistant",
                            "phase": string_field(payload, "phase"),
                            "text": string_field(payload, "message"),
                            "timestamp": string_field(event, "timestamp"),
                        }),
                    ),
                    "token_count" => {
                        token_usage = payload.get("info").cloned().unwrap_or_else(|| json!({}));
                        rate_limits = payload.get("rate_limits").cloned().unwrap_or(Value::Null);
                        context_window =
                            context_window_from_usage(&token_usage).unwrap_or(context_window);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    let total_input = token_usage_input_tokens(&token_usage);
    let context_used_percent = if context_window > 0 {
        (total_input as f64 / context_window as f64) * 100.0
    } else {
        0.0
    };
    json!({
        "sessionId": session_id,
        "model": model,
        "cliVersion": cli_version,
        "eventCount": events.len(),
        "messages": messages,
        "toolCalls": tool_calls,
        "timeline": timeline,
        "tokenUsage": token_usage,
        "rateLimits": rate_limits,
        "contextUsedTokens": total_input,
        "modelContextWindow": context_window,
        "contextUsedPercent": context_used_percent,
    })
}

fn usage_payload_info(usage: &Value) -> Value {
    usage.get("info").cloned().unwrap_or_else(|| usage.clone())
}

fn context_window_from_usage(usage: &Value) -> Option<i64> {
    usage
        .get("model_context_window")
        .and_then(Value::as_i64)
        .or_else(|| usage.get("modelContextWindow").and_then(Value::as_i64))
}

fn token_usage_input_tokens(usage: &Value) -> i64 {
    usage
        .get("total_token_usage")
        .and_then(|value| value.get("input_tokens"))
        .and_then(Value::as_i64)
        .or_else(|| {
            usage
                .get("last_token_usage")
                .and_then(|value| value.get("input_tokens"))
                .and_then(Value::as_i64)
        })
        .unwrap_or(0)
}

fn collect_current_item(
    event: &Value,
    messages: &mut Vec<Value>,
    tool_calls: &mut Vec<Value>,
    timeline: &mut Vec<Value>,
) {
    let item = event.get("item").unwrap_or(&Value::Null);
    let item_type = item.get("type").and_then(Value::as_str).unwrap_or("");
    let event_type = event.get("type").and_then(Value::as_str).unwrap_or("");
    let status = if event_type.ends_with(".started") {
        "started"
    } else {
        "completed"
    };
    match item_type {
        "agent_message" => {
            let text = string_field(item, "text");
            if !text.trim().is_empty() {
                push_message(
                    messages,
                    timeline,
                    json!({
                        "role": "assistant",
                        "phase": status,
                        "text": text,
                        "timestamp": string_field(event, "timestamp"),
                    }),
                );
            }
        }
        "message" => {
            let text = text_from_current_message_item(item);
            if !text.trim().is_empty() {
                push_message(
                    messages,
                    timeline,
                    json!({
                        "role": string_field(item, "role"),
                        "phase": status,
                        "text": text,
                        "timestamp": string_field(event, "timestamp"),
                    }),
                );
            }
        }
        "command_execution" => push_tool_call(
            tool_calls,
            timeline,
            json!({
                "name": "command_execution",
                "arguments": string_field(item, "command"),
                "output": item.get("aggregated_output").cloned().unwrap_or_else(|| json!("")),
                "status": item.get("status").and_then(Value::as_str).unwrap_or(status),
                "exitCode": item.get("exit_code").cloned().unwrap_or(Value::Null),
                "callId": string_field(item, "id"),
                "timestamp": string_field(event, "timestamp"),
            }),
        ),
        _ if item_type.contains("tool") || item_type.contains("function") => {
            push_tool_call(
                tool_calls,
                timeline,
                json!({
                    "name": non_empty(string_field(item, "name"), item_type.to_string()),
                    "arguments": item.get("arguments")
                        .or_else(|| item.get("input"))
                        .cloned()
                        .unwrap_or_else(|| json!("")),
                    "output": item.get("output")
                        .or_else(|| item.get("result"))
                        .cloned()
                        .unwrap_or_else(|| json!("")),
                    "status": item.get("status").and_then(Value::as_str).unwrap_or(status),
                    "callId": string_field(item, "id"),
                    "timestamp": string_field(event, "timestamp"),
                }),
            );
        }
        _ => {}
    }
}

fn text_from_current_message_item(item: &Value) -> String {
    if let Some(text) = item.get("text").and_then(Value::as_str) {
        return text.to_string();
    }
    item.get("content")
        .and_then(Value::as_array)
        .map(|content| {
            content
                .iter()
                .filter_map(|part| {
                    part.get("text")
                        .or_else(|| part.get("content"))
                        .and_then(Value::as_str)
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

fn collect_response_item(
    event: &Value,
    messages: &mut Vec<Value>,
    tool_calls: &mut Vec<Value>,
    timeline: &mut Vec<Value>,
) {
    let payload = event.get("payload").unwrap_or(&Value::Null);
    match payload.get("type").and_then(Value::as_str).unwrap_or("") {
        "message" => {
            let text = payload
                .get("content")
                .and_then(Value::as_array)
                .map(|content| {
                    content
                        .iter()
                        .filter_map(|part| part.get("text").and_then(Value::as_str))
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();
            if !text.trim().is_empty() {
                push_message(
                    messages,
                    timeline,
                    json!({
                        "role": string_field(payload, "role"),
                        "phase": string_field(payload, "phase"),
                        "text": text,
                        "timestamp": string_field(event, "timestamp"),
                    }),
                );
            }
        }
        "function_call" => push_tool_call(
            tool_calls,
            timeline,
            json!({
                "name": string_field(payload, "name"),
                "arguments": payload.get("arguments").cloned().unwrap_or_else(|| json!("")),
                "callId": string_field(payload, "call_id"),
                "timestamp": string_field(event, "timestamp"),
            }),
        ),
        "function_call_output" => push_tool_call(
            tool_calls,
            timeline,
            json!({
                "output": payload.get("output").cloned().unwrap_or_else(|| json!("")),
                "callId": string_field(payload, "call_id"),
                "timestamp": string_field(event, "timestamp"),
            }),
        ),
        _ => {}
    }
}

fn push_message(messages: &mut Vec<Value>, timeline: &mut Vec<Value>, mut message: Value) {
    if let Value::Object(object) = &mut message {
        object.insert("kind".to_string(), json!("message"));
    }
    messages.push(message.clone());
    timeline.push(message);
}

fn push_tool_call(tool_calls: &mut Vec<Value>, timeline: &mut Vec<Value>, mut tool_call: Value) {
    if let Value::Object(object) = &mut tool_call {
        object.insert("kind".to_string(), json!("tool_call"));
    }
    tool_calls.push(tool_call.clone());
    timeline.push(tool_call);
}

fn is_failed_state(state: &str) -> bool {
    state.contains("failed") || state == "approval_failed"
}

fn count_rows(counts: BTreeMap<String, usize>, label_key: &str) -> Vec<Value> {
    let mut rows = counts
        .into_iter()
        .map(|(label, count)| {
            let mut object = Map::new();
            object.insert(label_key.to_string(), json!(label));
            object.insert("count".to_string(), json!(count));
            Value::Object(object)
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        let left_count = left.get("count").and_then(Value::as_u64).unwrap_or(0);
        let right_count = right.get("count").and_then(Value::as_u64).unwrap_or(0);
        right_count
            .cmp(&left_count)
            .then_with(|| string_field(left, label_key).cmp(&string_field(right, label_key)))
    });
    rows
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

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(super) struct AgentSessionView {
    pub key: String,
    pub role: String,
    pub guild_id: String,
    pub scope_id: String,
    pub session_id: String,
    pub active_job_id: String,
    pub latest_job_id: String,
    pub status: AgentSessionStatus,
    pub invocation_count: u64,
    pub created_at: String,
    pub last_used_at: String,
    pub last_error: String,
}

impl AgentSessionView {
    pub fn to_json(&self) -> Value {
        json!({
            "key": self.key,
            "role": self.role,
            "guild_id": self.guild_id,
            "scope_id": self.scope_id,
            "session_id": self.session_id,
            "active_job_id": self.active_job_id,
            "latest_job_id": self.latest_job_id,
            "status": self.status.as_str(),
            "invocation_count": self.invocation_count,
            "created_at": self.created_at,
            "last_used_at": self.last_used_at,
            "last_error": self.last_error,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum AgentSessionStatus {
    #[default]
    Idle,
    Running,
    Failed,
}

impl AgentSessionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Running => "running",
            Self::Failed => "failed",
        }
    }
}
