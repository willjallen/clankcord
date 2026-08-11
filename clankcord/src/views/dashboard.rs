use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value, json};
use sqlx::{Postgres, QueryBuilder, Row};

use super::operations::{
    agent_usage_payload, automation_dashboard_payload, dashboard_job_value,
    dashboard_latency_by_kind_payload,
};
use crate::Result;
use crate::domain::Ctx;
use crate::model::agents::task_session_key;
use crate::model::job::{Job, JobState};
use crate::store::JobVisibility;
use crate::store::timeline_event_payload;
use crate::time::{
    instant_ms_dt, isoformat_z, ms_to_datetime, parse_instant, resolve_time_reference, utc_now,
};
use crate::util::{first_non_empty, preview};
use crate::views::jobs;
use crate::views::operations;
use crate::views::search::{
    SearchField, push_event_member_joins, push_event_search, push_job_member_joins,
    push_job_search, search_terms,
};

const DEFAULT_FROM: &str = "-1h";
const DEFAULT_LIMIT: usize = 120;
const MAX_LIMIT: usize = 1000;
const EVENT_RANK: i8 = 1;
const JOB_RANK: i8 = 0;

const DASHBOARD_CATEGORIES: &[(&str, &str, bool)] = &[
    ("conversation", "Conversation", true),
    ("agent", "Agent", true),
    ("messaging_control", "Messaging & Control", true),
    ("automation", "Automation", true),
    ("operations", "Operations", true),
    ("other", "Other", true),
    ("voice_detail", "Voice Detail", false),
    ("background", "Background", false),
];

const CONVERSATION_EVENT_KINDS: &[&str] = &[
    "transcript",
    "conversation_started",
    "publication_created",
    "wake_detected",
];
const VOICE_DETAIL_EVENT_KINDS: &[&str] = &["speech_segment"];
const AGENT_EVENT_KINDS: &[&str] = &[
    "agent_session_created",
    "agent_session_resumed",
    "agent_session_thread_created",
    "agent_session_thread_unavailable",
    "agent_thread_titled",
];
const MESSAGING_CONTROL_EVENT_KINDS: &[&str] = &[
    "discord_text_message",
    "discord_slash_command",
    "feedback",
    "text_delivered",
    "confirmation_posted",
    "command_created",
    "listening_paused",
    "listening_resumed",
    "forget_applied",
    "voice_bot_assigned",
    "voice_bot_released",
];
const AUTOMATION_EVENT_KINDS: &[&str] = &[
    "automation_created",
    "automation_cancelled",
    "automation_fired",
    "automation_action_failed",
];
const OPERATIONS_EVENT_KINDS: &[&str] = &[
    "agent_mcp_token_warning",
    "agent_task_result_suppressed",
    "wake_provider_circuit_open",
    "discord_publish_error",
];
const BACKGROUND_EVENT_KINDS: &[&str] = &[
    "job_created",
    "agent_session_retired",
    "agent_thread_title_skipped",
    "agent_thread_title_refresh_attempted",
    "discord_typing_indicator",
    "retention_retired",
    "voice_adapter_snapshot",
    "voice_status",
    "voice_status_sync",
    "discord_voice_status_snapshot",
    "runtime_maintenance",
    "automation_evaluation",
    "stale_wake_probe_sweep",
    "stale_running_job_sweep",
    "ephemeral_job_gc",
    "transcription_mux",
    "transcription_mux_plan",
    "wake_probe",
];

/// A dashboard multi-select preserves all three UI states at the HTTP boundary.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum DashboardFilter {
    #[default]
    All,
    None,
    Values(BTreeSet<String>),
}

impl DashboardFilter {
    fn includes(&self, value: &str) -> bool {
        match self {
            Self::All => true,
            Self::None => false,
            Self::Values(values) => values.contains(value),
        }
    }

    fn is_none(&self) -> bool {
        matches!(self, Self::None)
    }
}

/// Parses a comma-separated dashboard selector. Omission/`all`, `none`, and a
/// non-empty value list are deliberately distinct contracts.
pub fn parse_dashboard_filter(raw: Option<&str>, label: &str) -> Result<DashboardFilter> {
    let Some(raw) = raw else {
        return Ok(DashboardFilter::All);
    };
    let raw = raw.trim();
    if raw.eq_ignore_ascii_case("all") {
        return Ok(DashboardFilter::All);
    }
    if raw.eq_ignore_ascii_case("none") || raw.is_empty() {
        return Ok(DashboardFilter::None);
    }
    let values = raw
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .collect::<BTreeSet<_>>();
    if values
        .iter()
        .any(|value| value.eq_ignore_ascii_case("all") || value.eq_ignore_ascii_case("none"))
    {
        anyhow::bail!("dashboard {label} cannot mix all or none with explicit values");
    }
    if values.is_empty() {
        anyhow::bail!("dashboard {label} must be all, none, or a comma-separated value list");
    }
    Ok(DashboardFilter::Values(values))
}

pub fn default_dashboard_categories() -> DashboardFilter {
    DashboardFilter::Values(
        DASHBOARD_CATEGORIES
            .iter()
            .filter(|(_, _, selected)| *selected)
            .map(|(id, _, _)| (*id).to_string())
            .collect(),
    )
}

#[derive(Debug, Clone)]
pub struct DashboardTimelineRequest {
    pub record_types: DashboardFilter,
    pub categories: DashboardFilter,
    /// Unified Kind selector. Events match either `event_kind` or their related
    /// `job_kind`; jobs match `kind`.
    pub kinds: DashboardFilter,
    pub event_kinds: DashboardFilter,
    pub job_kinds: DashboardFilter,
    pub states: DashboardFilter,
    pub scope_kinds: DashboardFilter,
    pub scope_ids: DashboardFilter,
    pub guild_ids: DashboardFilter,
    pub from: String,
    pub to: String,
    pub search: String,
    pub search_field: String,
    pub metadata: String,
    pub limit: usize,
    pub cursor: String,
}

impl Default for DashboardTimelineRequest {
    fn default() -> Self {
        Self {
            record_types: DashboardFilter::All,
            categories: default_dashboard_categories(),
            kinds: DashboardFilter::All,
            event_kinds: DashboardFilter::All,
            job_kinds: DashboardFilter::All,
            states: DashboardFilter::All,
            scope_kinds: DashboardFilter::All,
            scope_ids: DashboardFilter::All,
            guild_ids: DashboardFilter::All,
            from: DEFAULT_FROM.to_string(),
            to: String::new(),
            search: String::new(),
            search_field: "all".to_string(),
            metadata: "full".to_string(),
            limit: DEFAULT_LIMIT,
            cursor: String::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct DashboardJobsRequest {
    pub categories: DashboardFilter,
    pub kinds: DashboardFilter,
    pub job_kinds: DashboardFilter,
    pub states: DashboardFilter,
    pub scope_kinds: DashboardFilter,
    pub scope_ids: DashboardFilter,
    pub guild_ids: DashboardFilter,
    pub from: String,
    pub to: String,
    pub search: String,
    pub search_field: String,
    pub metadata: String,
    pub limit: usize,
    pub cursor: String,
}

#[derive(Debug, Clone)]
pub struct DashboardOverviewRequest {
    pub jobs_limit: usize,
}

impl Default for DashboardOverviewRequest {
    fn default() -> Self {
        Self { jobs_limit: 120 }
    }
}

#[derive(Debug, Clone)]
pub struct DashboardAgentsRequest {
    pub limit: usize,
}

impl Default for DashboardAgentsRequest {
    fn default() -> Self {
        Self { limit: 120 }
    }
}

#[derive(Debug, Clone)]
pub struct DashboardTranscriptRequest {
    pub since: String,
    pub limit: usize,
    pub channel: String,
    pub search: String,
}

impl Default for DashboardTranscriptRequest {
    fn default() -> Self {
        Self {
            since: "-24h".to_string(),
            limit: 250,
            channel: String::new(),
            search: String::new(),
        }
    }
}

impl Default for DashboardJobsRequest {
    fn default() -> Self {
        Self {
            categories: default_dashboard_categories(),
            kinds: DashboardFilter::All,
            job_kinds: DashboardFilter::All,
            states: DashboardFilter::All,
            scope_kinds: DashboardFilter::All,
            scope_ids: DashboardFilter::All,
            guild_ids: DashboardFilter::All,
            from: DEFAULT_FROM.to_string(),
            to: String::new(),
            search: String::new(),
            search_field: "all".to_string(),
            metadata: "full".to_string(),
            limit: DEFAULT_LIMIT,
            cursor: String::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MetadataMode {
    Full,
    Count,
    None,
}

#[derive(Debug, Clone)]
struct QueryWindow {
    from_ms: Option<i64>,
    to_ms: i64,
    snapshot_ms: i64,
}

#[derive(Debug, Clone)]
struct PageCursor {
    snapshot_ms: i64,
    sort_ms: i64,
    rank: i8,
    tie: String,
}

#[derive(Debug)]
struct TimelineRecord {
    sort_ms: i64,
    rank: i8,
    tie: String,
    id: String,
    category: String,
    payload: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ScopeKey {
    kind: String,
    guild_id: String,
    id: String,
}

#[derive(Debug, Default)]
struct FacetAccumulator {
    record_types: BTreeMap<String, i64>,
    categories: BTreeMap<String, i64>,
    category_kinds: BTreeMap<String, BTreeSet<String>>,
    kinds: BTreeMap<String, i64>,
    event_kinds: BTreeMap<String, i64>,
    job_kinds: BTreeMap<String, i64>,
    states: BTreeMap<String, (i64, i64)>,
    scopes: BTreeMap<ScopeKey, (i64, String)>,
}

pub async fn dashboard_overview(ctx: &Ctx, request: DashboardOverviewRequest) -> Result<Value> {
    let now = utc_now();
    let limit = request.jobs_limit.clamp(1, 500);
    let active_records = dashboard_active_jobs(ctx, limit).await?;
    let recent_records = dashboard_recent_jobs(ctx, now, limit).await?;
    let mut active = active_records
        .iter()
        .map(dashboard_job_value)
        .collect::<Vec<_>>();
    let mut recent = recent_records
        .iter()
        .map(dashboard_job_value)
        .collect::<Vec<_>>();
    enrich_job_values(ctx, &active_records, &mut active).await?;
    enrich_job_values(ctx, &recent_records, &mut recent).await?;
    let mut summary = dashboard_overview_job_summary(ctx, now).await?;
    enrich_job_summary(ctx, &mut summary).await?;
    let event_page = dashboard_timeline(
        ctx,
        DashboardTimelineRequest {
            record_types: DashboardFilter::Values(BTreeSet::from(["event".to_string()])),
            job_kinds: DashboardFilter::None,
            from: "-1h".to_string(),
            metadata: "none".to_string(),
            limit,
            ..DashboardTimelineRequest::default()
        },
    )
    .await?;
    let recent_events = event_page
        .get("records")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|record| record.get("event").cloned())
        .collect::<Vec<_>>();
    let operations = dashboard_latency_by_kind_payload(ctx, now).await?;
    let charts = dashboard_overview_charts(ctx, now).await?;
    Ok(json!({
        "generatedAt": isoformat_z(Some(now)),
        "jobs": {
            "summary": summary,
            "active": active,
            "recent": recent,
        },
        "timeline": {
            "window": "-1h",
            "recentEvents": recent_events,
        },
        "operations": operations,
        "charts": charts,
    }))
}

pub async fn dashboard_agents(ctx: &Ctx, request: DashboardAgentsRequest) -> Result<Value> {
    let now = utc_now();
    let limit = request.limit.clamp(1, 500);
    let jobs = ctx
        .store
        .list_jobs_by_kind_with_visibility(
            crate::model::job::JobKind::AgentTask,
            limit,
            JobVisibility::IncludeEphemeral,
        )
        .await?;
    let usage_jobs = dashboard_agent_usage_jobs(ctx, now).await?;
    let mut agents = json!({
        "jobs": jobs.iter().map(dashboard_agent_list_entry).collect::<Vec<_>>(),
        "summary": dashboard_agent_summary(ctx).await?,
        "sessions": dashboard_agent_sessions(ctx).await?,
        "codex": {"usage": agent_usage_payload(&usage_jobs, now)},
    });
    if let Some(entries) = agents.get_mut("jobs").and_then(Value::as_array_mut) {
        let mut values = entries
            .iter()
            .map(|entry| entry.get("job").cloned().unwrap_or_else(|| json!({})))
            .collect::<Vec<_>>();
        enrich_job_values(ctx, &jobs, &mut values).await?;
        for (entry, job) in entries.iter_mut().zip(values) {
            entry
                .as_object_mut()
                .expect("dashboard agent entry")
                .insert("job".to_string(), job);
        }
    }
    enrich_agent_sessions(ctx, &mut agents).await?;
    Ok(json!({
        "generatedAt": isoformat_z(Some(now)),
        "agents": agents,
    }))
}

pub async fn dashboard_automations(ctx: &Ctx) -> Result<Value> {
    let records = ctx.store.list_automations(None, None, None).await?;
    let mut automations = automation_dashboard_payload(&records);
    enrich_automation_payload(ctx, &mut automations).await?;
    Ok(json!({
        "generatedAt": isoformat_z(Some(utc_now())),
        "automations": automations,
    }))
}

pub async fn dashboard_transcript(ctx: &Ctx, request: DashboardTranscriptRequest) -> Result<Value> {
    let now = utc_now();
    let since = if request.since.trim().eq_ignore_ascii_case("all") {
        None
    } else {
        let raw = if request.since.trim().is_empty() {
            "-24h"
        } else {
            request.since.trim()
        };
        Some(
            resolve_time_reference(raw, Some(now))
                .ok_or_else(|| anyhow::anyhow!("invalid dashboard transcript since: {raw}"))?,
        )
    };
    let mut events = operations::recent_transcript_events(
        ctx,
        since,
        request.limit.clamp(1, 5000),
        &request.channel,
        &request.search,
    )
    .await?;
    enrich_event_values(ctx, &mut events).await?;
    events.reverse();
    Ok(json!({
        "generatedAt": isoformat_z(Some(now)),
        "transcript": {
            "since": since.map(|value| isoformat_z(Some(value))).unwrap_or_else(|| "all".to_string()),
            "events": events,
        },
    }))
}

pub async fn dashboard_agent_detail(ctx: &Ctx, job_id: &str) -> Result<Value> {
    let mut detail = operations::dashboard_agent_job(ctx, job_id).await?;
    if let Some(job_value) = detail.get_mut("job") {
        let job = ctx.store.get_job(job_id).await?;
        enrich_job_values(
            ctx,
            std::slice::from_ref(&job),
            std::slice::from_mut(job_value),
        )
        .await?;
        job_value
            .as_object_mut()
            .expect("dashboard agent detail job")
            .extend([
                ("attempts".to_string(), json!(job.attempts)),
                (
                    "durationMs".to_string(),
                    json!(dashboard_job_duration_ms(&job)),
                ),
                (
                    "request".to_string(),
                    json!(
                        job.command()
                            .map(|command| command.arguments.request_text())
                            .unwrap_or_default()
                    ),
                ),
            ]);
    }
    Ok(detail)
}

async fn dashboard_recent_jobs(
    ctx: &Ctx,
    now: chrono::DateTime<chrono::Utc>,
    limit: usize,
) -> Result<Vec<Job>> {
    let rows = sqlx::query(
        r#"
            SELECT p.payload_blob
            FROM jobs j
            JOIN job_payloads p ON p.job_id = j.job_id
            WHERE j.updated_at_ms >= $1 AND j.updated_at_ms <= $2
            ORDER BY j.updated_at_ms DESC, j.created_at_ms DESC, j.job_id DESC
            LIMIT $3
            "#,
    )
    .bind(instant_ms_dt(now - chrono::Duration::hours(1)))
    .bind(instant_ms_dt(now))
    .bind(limit as i64)
    .fetch_all(&ctx.store.pool)
    .await?;
    rows.into_iter()
        .map(|row| Job::decode(&row.try_get::<Vec<u8>, _>("payload_blob")?))
        .collect()
}

async fn dashboard_active_jobs(ctx: &Ctx, limit: usize) -> Result<Vec<Job>> {
    let rows = sqlx::query(
        r#"
            SELECT p.payload_blob
            FROM jobs j
            JOIN job_payloads p ON p.job_id = j.job_id
            WHERE j.terminal = FALSE
            ORDER BY j.updated_at_ms DESC, j.created_at_ms DESC, j.job_id DESC
            LIMIT $1
            "#,
    )
    .bind(limit as i64)
    .fetch_all(&ctx.store.pool)
    .await?;
    rows.into_iter()
        .map(|row| Job::decode(&row.try_get::<Vec<u8>, _>("payload_blob")?))
        .collect()
}

async fn dashboard_agent_usage_jobs(
    ctx: &Ctx,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Vec<Job>> {
    let rows = sqlx::query(
        r#"
            SELECT p.payload_blob
            FROM jobs j
            JOIN job_payloads p ON p.job_id = j.job_id
            WHERE j.kind = 'agent_task' AND j.updated_at_ms >= $1
            ORDER BY j.updated_at_ms DESC, j.job_id DESC
            "#,
    )
    .bind(instant_ms_dt(now - chrono::Duration::days(7)))
    .fetch_all(&ctx.store.pool)
    .await?;
    rows.into_iter()
        .map(|row| Job::decode(&row.try_get::<Vec<u8>, _>("payload_blob")?))
        .collect()
}

async fn dashboard_agent_summary(ctx: &Ctx) -> Result<Value> {
    let now = utc_now();
    let since = now - chrono::Duration::hours(24);
    let row = sqlx::query(
            r#"
            SELECT COUNT(*) FILTER (WHERE updated_at_ms >= $1)::BIGINT AS total,
                   COUNT(*) FILTER (WHERE terminal = FALSE)::BIGINT AS active,
                   COUNT(*) FILTER (WHERE updated_at_ms >= $1 AND failed = TRUE)::BIGINT AS failed,
                   COUNT(*) FILTER (WHERE updated_at_ms >= $1 AND state = 'complete')::BIGINT AS completed
            FROM jobs
            WHERE kind = 'agent_task'
              AND (updated_at_ms >= $1 OR terminal = FALSE)
            "#,
        )
        .bind(instant_ms_dt(since))
        .fetch_one(&ctx.store.pool)
        .await?;
    Ok(json!({
        "total": row.try_get::<i64, _>("total")?,
        "active": row.try_get::<i64, _>("active")?,
        "failed": row.try_get::<i64, _>("failed")?,
        "completed": row.try_get::<i64, _>("completed")?,
        "window": "24h",
        "since": isoformat_z(Some(since)),
    }))
}

async fn dashboard_agent_sessions(ctx: &Ctx) -> Result<Vec<Value>> {
    let rows = sqlx::query(
            r#"
            WITH grouped AS MATERIALIZED (
              SELECT scope_kind, guild_id, scope_id,
                     COUNT(*)::BIGINT AS invocation_count,
                     MIN(created_at_ms) AS created_at_ms,
                     MAX(updated_at_ms) AS last_used_at_ms,
                     (ARRAY_AGG(job_id ORDER BY updated_at_ms DESC, job_id DESC))[1] AS latest_job_id,
                     (ARRAY_AGG(state ORDER BY updated_at_ms DESC, job_id DESC))[1] AS latest_state,
                     (ARRAY_AGG(job_id ORDER BY updated_at_ms DESC, job_id DESC)
                       FILTER (WHERE terminal = FALSE))[1] AS active_job_id
              FROM jobs
              WHERE kind = 'agent_task'
              GROUP BY scope_kind, guild_id, scope_id
            ), latest AS MATERIALIZED (
              SELECT DISTINCT ON (j.scope_kind, j.guild_id, j.scope_id)
                     j.scope_kind, j.guild_id, j.scope_id, p.payload_blob
              FROM jobs j
              JOIN job_payloads p ON p.job_id = j.job_id
              WHERE j.kind = 'agent_task'
              ORDER BY j.scope_kind, j.guild_id, j.scope_id,
                       j.updated_at_ms DESC, j.job_id DESC
            )
            SELECT grouped.*, latest.payload_blob
            FROM grouped
            JOIN latest USING (scope_kind, guild_id, scope_id)
            ORDER BY grouped.last_used_at_ms DESC, grouped.guild_id, grouped.scope_id
            "#,
        )
        .fetch_all(&ctx.store.pool)
        .await?;
    rows.into_iter()
        .map(|row| {
            let guild_id: String = row.try_get("guild_id")?;
            let scope_id: String = row.try_get("scope_id")?;
            let latest_state: String = row.try_get("latest_state")?;
            let active_job_id: Option<String> = row.try_get("active_job_id")?;
            let latest = Job::decode(&row.try_get::<Vec<u8>, _>("payload_blob")?)?;
            let task = latest.metadata.agent_task();
            let session_id = task
                .map(|task| task.agent.session_id.clone())
                .unwrap_or_default();
            let last_error = task
                .map(|task| task.dispatch_error.clone())
                .unwrap_or_default();
            let status = if active_job_id.is_some() {
                "running"
            } else if latest_state.parse::<JobState>()?.is_failed() {
                "failed"
            } else {
                "idle"
            };
            Ok(json!({
                "key": task_session_key(&guild_id, &scope_id),
                "role": "task",
                "scope_kind": row.try_get::<String, _>("scope_kind")?,
                "guild_id": guild_id,
                "scope_id": scope_id,
                "session_id": session_id,
                "active_job_id": active_job_id.unwrap_or_default(),
                "latest_job_id": row.try_get::<String, _>("latest_job_id")?,
                "status": status,
                "invocation_count": row.try_get::<i64, _>("invocation_count")?,
                "created_at": timestamp(row.try_get::<i64, _>("created_at_ms")?),
                "last_used_at": timestamp(row.try_get::<i64, _>("last_used_at_ms")?),
                "last_error": last_error,
            }))
        })
        .collect()
}

async fn dashboard_overview_job_summary(
    ctx: &Ctx,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Value> {
    let rows = sqlx::query(
        r#"
            WITH selected AS MATERIALIZED (
              SELECT state, kind, scope_kind, guild_id, scope_id, updated_at_ms,
                     terminal, failed
              FROM jobs
              WHERE updated_at_ms >= $1
                 OR terminal = FALSE
            )
            SELECT 'state' AS dimension, state AS key, '' AS scope_kind,
                   '' AS guild_id, '' AS scope_id, COUNT(*) AS count,
                   0::BIGINT AS active, 0::BIGINT AS failed, MAX(updated_at_ms) AS latest_at_ms
            FROM selected
            GROUP BY state
            UNION ALL
            SELECT 'kind', kind, '', '', '', COUNT(*), 0, 0, MAX(updated_at_ms)
            FROM selected
            GROUP BY kind
            UNION ALL
            SELECT 'scope', '', scope_kind, guild_id, scope_id, COUNT(*),
                   COUNT(*) FILTER (WHERE terminal = FALSE),
                   COUNT(*) FILTER (WHERE failed = TRUE),
                   MAX(updated_at_ms)
            FROM selected
            GROUP BY scope_kind, guild_id, scope_id
            "#,
    )
    .bind(instant_ms_dt(now - chrono::Duration::hours(1)))
    .fetch_all(&ctx.store.pool)
    .await?;

    let mut by_state = Vec::new();
    let mut by_kind = Vec::new();
    let mut by_scope = Vec::new();
    let mut total = 0_i64;
    let mut active = 0_i64;
    let mut queued = 0_i64;
    let mut running = 0_i64;
    let mut waiting = 0_i64;
    let mut failed = 0_i64;
    let mut cancellable = 0_i64;
    for row in rows {
        let dimension: String = row.try_get("dimension")?;
        let count: i64 = row.try_get("count")?;
        match dimension.as_str() {
            "state" => {
                let state: String = row.try_get("key")?;
                let job_state: JobState = state.parse()?;
                total += count;
                if !job_state.is_terminal() {
                    active += count;
                }
                if job_state.is_cancellable() {
                    cancellable += count;
                }
                if job_state.is_failed() {
                    failed += count;
                }
                match job_state {
                    JobState::Queued => queued += count,
                    JobState::Running => running += count,
                    JobState::Waiting => waiting += count,
                    _ => {}
                }
                by_state.push(json!({"state": state, "count": count}));
            }
            "kind" => {
                by_kind.push(json!({
                    "kind": row.try_get::<String, _>("key")?,
                    "count": count,
                }));
            }
            "scope" => {
                let latest_at_ms: i64 = row.try_get("latest_at_ms")?;
                by_scope.push(json!({
                    "scope_kind": row.try_get::<String, _>("scope_kind")?,
                    "guild_id": row.try_get::<String, _>("guild_id")?,
                    "scope_id": row.try_get::<String, _>("scope_id")?,
                    "total": count,
                    "active": row.try_get::<i64, _>("active")?,
                    "failed": row.try_get::<i64, _>("failed")?,
                    "latest_at": timestamp(latest_at_ms),
                }));
            }
            _ => unreachable!("dashboard summary dimension is fixed by SQL"),
        }
    }
    by_state.sort_by(|left, right| {
        right["count"]
            .as_i64()
            .cmp(&left["count"].as_i64())
            .then_with(|| left["state"].as_str().cmp(&right["state"].as_str()))
    });
    by_kind.sort_by(|left, right| {
        right["count"]
            .as_i64()
            .cmp(&left["count"].as_i64())
            .then_with(|| left["kind"].as_str().cmp(&right["kind"].as_str()))
    });
    by_scope.sort_by(|left, right| {
        right["total"]
            .as_i64()
            .cmp(&left["total"].as_i64())
            .then_with(|| left["scope_id"].as_str().cmp(&right["scope_id"].as_str()))
    });
    Ok(json!({
        "total": total,
        "active": active,
        "terminal": total - active,
        "queued": queued,
        "running": running,
        "waiting": waiting,
        "failed": failed,
        "cancellable": cancellable,
        "byState": by_state,
        "byKind": by_kind,
        "byScope": by_scope,
        "window": {"from": isoformat_z(Some(now - chrono::Duration::hours(1))), "to": isoformat_z(Some(now))},
    }))
}

async fn dashboard_overview_charts(ctx: &Ctx, now: chrono::DateTime<chrono::Utc>) -> Result<Value> {
    let since_ms = instant_ms_dt(now - chrono::Duration::hours(1));
    let now_ms = instant_ms_dt(now);
    let job_rows = sqlx::query(
        r#"
            SELECT kind, state, COUNT(*)::BIGINT AS count
            FROM jobs
            WHERE updated_at_ms >= $1
               OR terminal = FALSE
            GROUP BY kind, state
            ORDER BY count DESC, kind, state
            "#,
    )
    .bind(since_ms)
    .fetch_all(&ctx.store.pool)
    .await?;
    let event_rows = sqlx::query(
        r#"
            SELECT started_at_ms - MOD(started_at_ms, 300000) AS bucket_at_ms,
                   event_kind, COUNT(*)::BIGINT AS count
            FROM timeline_events
            WHERE forgotten = FALSE
              AND started_at_ms >= $1
              AND started_at_ms <= $2
            GROUP BY bucket_at_ms, event_kind
            ORDER BY bucket_at_ms, event_kind
            "#,
    )
    .bind(since_ms)
    .bind(now_ms)
    .fetch_all(&ctx.store.pool)
    .await?;
    let scope_rows = sqlx::query(
        r#"
            WITH observed AS MATERIALIZED (
              SELECT scope_kind, guild_id, scope_id, COUNT(*)::BIGINT AS jobs,
                     0::BIGINT AS speech, 0::BIGINT AS transcripts, 0::BIGINT AS wake,
                     MAX(updated_at_ms) AS latest_at_ms
              FROM jobs
              WHERE updated_at_ms >= $1
                 OR terminal = FALSE
              GROUP BY scope_kind, guild_id, scope_id
              UNION ALL
              SELECT scope_kind, guild_id, scope_id, 0,
                     COUNT(*) FILTER (WHERE event_kind = 'speech_segment'),
                     COUNT(*) FILTER (WHERE event_kind = 'transcript'),
                     COUNT(*) FILTER (WHERE event_kind LIKE 'wake_%'),
                     MAX(started_at_ms)
              FROM timeline_events
              WHERE forgotten = FALSE
                AND started_at_ms >= $1
                AND started_at_ms <= $2
              GROUP BY scope_kind, guild_id, scope_id
            )
            SELECT scope_kind, guild_id, scope_id,
                   SUM(jobs)::BIGINT AS jobs,
                   SUM(speech)::BIGINT AS speech,
                   SUM(transcripts)::BIGINT AS transcripts,
                   SUM(wake)::BIGINT AS wake,
                   MAX(latest_at_ms) AS latest_at_ms
            FROM observed
            GROUP BY scope_kind, guild_id, scope_id
            ORDER BY SUM(jobs + speech + transcripts + wake) DESC,
                     scope_kind, guild_id, scope_id
            "#,
    )
    .bind(since_ms)
    .bind(now_ms)
    .fetch_all(&ctx.store.pool)
    .await?;

    let jobs_by_kind_state = job_rows
        .into_iter()
        .map(|row| {
            Ok(json!({
                "kind": row.try_get::<String, _>("kind")?,
                "state": row.try_get::<String, _>("state")?,
                "count": row.try_get::<i64, _>("count")?,
            }))
        })
        .collect::<Result<Vec<_>>>()?;
    let events_by_bucket_kind = event_rows
        .into_iter()
        .map(|row| {
            Ok(json!({
                "bucketAt": timestamp(row.try_get::<i64, _>("bucket_at_ms")?),
                "kind": row.try_get::<String, _>("event_kind")?,
                "count": row.try_get::<i64, _>("count")?,
            }))
        })
        .collect::<Result<Vec<_>>>()?;
    let scope_keys = scope_rows
        .iter()
        .map(|row| {
            Ok(ScopeKey {
                kind: row.try_get("scope_kind")?,
                guild_id: row.try_get("guild_id")?,
                id: row.try_get("scope_id")?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let labels = dashboard_scope_labels(ctx, &scope_keys).await?;
    let scope_activity = scope_rows
        .into_iter()
        .zip(scope_keys)
        .map(|(row, key)| {
            let jobs: i64 = row.try_get("jobs")?;
            let speech: i64 = row.try_get("speech")?;
            let transcripts: i64 = row.try_get("transcripts")?;
            let wake: i64 = row.try_get("wake")?;
            Ok(json!({
                "scopeKind": key.kind,
                "guildId": key.guild_id,
                "scopeId": key.id,
                "scopeLabel": scope_label(&key, labels.get(&key)),
                "jobs": jobs,
                "speech": speech,
                "transcripts": transcripts,
                "wake": wake,
                "total": jobs + speech + transcripts + wake,
                "latestAt": timestamp(row.try_get::<i64, _>("latest_at_ms")?),
            }))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(json!({
        "window": {
            "from": timestamp(since_ms),
            "to": timestamp(now_ms),
            "eventBucketSeconds": 300,
        },
        "jobsByKindState": jobs_by_kind_state,
        "eventsByBucketKind": events_by_bucket_kind,
        "scopeActivity": scope_activity,
    }))
}

pub async fn dashboard_timeline(ctx: &Ctx, request: DashboardTimelineRequest) -> Result<Value> {
    validate_record_types(&request.record_types)?;
    validate_categories(&request.categories)?;
    let cursor = parse_cursor(&request.cursor)?;
    let now = utc_now();
    let snapshot_ms = cursor
        .as_ref()
        .map(|cursor| cursor.snapshot_ms)
        .unwrap_or_else(|| instant_ms_dt(now));
    let window = resolve_window(&request.from, &request.to, snapshot_ms)?;
    let search_field = parse_search_field(&request.search_field)?;
    let metadata_mode = parse_metadata_mode(&request.metadata)?;
    let limit = request.limit.clamp(1, MAX_LIMIT);
    let include_events = request.record_types.includes("event")
        && !request.categories.is_none()
        && !request.kinds.is_none()
        && !request.event_kinds.is_none()
        && !request.states.is_none()
        && !request.scope_kinds.is_none()
        && !request.scope_ids.is_none()
        && !request.guild_ids.is_none();
    let include_jobs = request.record_types.includes("job")
        && !request.categories.is_none()
        && !request.kinds.is_none()
        && !request.job_kinds.is_none()
        && !request.states.is_none()
        && !request.scope_kinds.is_none()
        && !request.scope_ids.is_none()
        && !request.guild_ids.is_none();

    let event_count = if metadata_mode != MetadataMode::None && include_events {
        count_dashboard_events(ctx, &request, &window, search_field).await?
    } else {
        0
    };
    let job_count = if metadata_mode != MetadataMode::None && include_jobs {
        count_dashboard_jobs(
            ctx,
            &request.categories,
            &request.job_kinds,
            &request.kinds,
            &request.states,
            &request.scope_kinds,
            &request.scope_ids,
            &request.guild_ids,
            &request.search,
            search_field,
            &window,
        )
        .await?
    } else {
        0
    };
    let matched = event_count + job_count;
    let facets = if metadata_mode == MetadataMode::Full {
        Some(dashboard_facets(ctx, &window).await?)
    } else {
        None
    };

    let mut records = Vec::with_capacity(limit.saturating_mul(2).saturating_add(2));
    if include_events {
        records.extend(
            select_dashboard_events(
                ctx,
                &request,
                &window,
                search_field,
                cursor.as_ref(),
                limit + 1,
            )
            .await?,
        );
    }
    if include_jobs {
        records.extend(
            select_dashboard_jobs(
                ctx,
                &request.categories,
                &request.job_kinds,
                &request.kinds,
                &request.states,
                &request.scope_kinds,
                &request.scope_ids,
                &request.guild_ids,
                &request.search,
                search_field,
                &window,
                cursor.as_ref(),
                limit + 1,
            )
            .await?,
        );
    }
    records.sort_by(compare_records);
    let has_more = records.len() > limit;
    records.truncate(limit);
    let next_cursor = has_more.then(|| encode_cursor(snapshot_ms, records.last().unwrap()));
    let returned = records.len();
    let records = records.into_iter().map(record_payload).collect::<Vec<_>>();

    let mut payload = Map::from_iter([
        (
            "metadata".to_string(),
            json!(match metadata_mode {
                MetadataMode::Full => "full",
                MetadataMode::Count => "count",
                MetadataMode::None => "none",
            }),
        ),
        ("snapshotAt".to_string(), json!(timestamp(snapshot_ms))),
        ("returned".to_string(), json!(returned)),
        ("hasMore".to_string(), json!(has_more)),
        ("nextCursor".to_string(), json!(next_cursor)),
        ("records".to_string(), json!(records)),
    ]);
    if metadata_mode != MetadataMode::None {
        payload.insert("matched".to_string(), json!(matched));
    }
    if let Some(facets) = facets {
        payload.insert("facets".to_string(), facet_payload(facets));
    }
    Ok(Value::Object(payload))
}

pub async fn dashboard_jobs(ctx: &Ctx, request: DashboardJobsRequest) -> Result<Value> {
    let timeline = dashboard_timeline(
        ctx,
        DashboardTimelineRequest {
            record_types: DashboardFilter::Values(BTreeSet::from(["job".to_string()])),
            categories: request.categories,
            kinds: request.kinds,
            event_kinds: DashboardFilter::None,
            job_kinds: request.job_kinds,
            states: request.states,
            scope_kinds: request.scope_kinds,
            scope_ids: request.scope_ids,
            guild_ids: request.guild_ids,
            from: request.from,
            to: request.to,
            search: request.search,
            search_field: request.search_field,
            metadata: request.metadata,
            limit: request.limit,
            cursor: request.cursor,
        },
    )
    .await?;
    let mut payload = timeline.as_object().cloned().unwrap_or_default();
    let jobs = payload
        .remove("records")
        .and_then(|records| records.as_array().cloned())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|record| record.get("job").cloned())
        .collect::<Vec<_>>();
    payload.insert("jobs".to_string(), Value::Array(jobs));
    if let Some(facets) = payload.get_mut("facets").and_then(Value::as_object_mut) {
        facets.remove("recordTypes");
        facets.remove("eventKinds");
    }
    Ok(Value::Object(payload))
}

async fn count_dashboard_events(
    ctx: &Ctx,
    request: &DashboardTimelineRequest,
    window: &QueryWindow,
    search_field: SearchField,
) -> Result<i64> {
    let mut query =
        QueryBuilder::<Postgres>::new("SELECT COUNT(*) AS count FROM timeline_events e");
    let search_has_terms = !search_terms(&request.search).is_empty();
    if search_has_terms && matches!(search_field, SearchField::All | SearchField::Room) {
        push_event_room_join(&mut query);
    }
    push_event_member_joins(
        &mut query,
        search_has_terms && matches!(search_field, SearchField::All | SearchField::Actor),
        search_has_terms && matches!(search_field, SearchField::All | SearchField::Room),
    );
    query.push(" WHERE e.forgotten = FALSE");
    push_time_bounds(&mut query, "e.started_at_ms", window);
    push_event_category_filter(&mut query, &request.categories);
    push_event_unified_kind_filter(&mut query, &request.kinds);
    push_filter(&mut query, "e.event_kind", &request.event_kinds);
    push_filter(
        &mut query,
        "COALESCE(e.payload_json->>'state', '')",
        &request.states,
    );
    push_filter(&mut query, "e.scope_kind", &request.scope_kinds);
    push_filter(&mut query, "e.scope_id", &request.scope_ids);
    push_filter(&mut query, "e.guild_id", &request.guild_ids);
    push_event_search(&mut query, &request.search, search_field);
    let row = query.build().fetch_one(&ctx.store.pool).await?;
    Ok(row.try_get("count")?)
}

#[allow(clippy::too_many_arguments)]
async fn count_dashboard_jobs(
    ctx: &Ctx,
    categories: &DashboardFilter,
    job_kinds: &DashboardFilter,
    kinds: &DashboardFilter,
    states: &DashboardFilter,
    scope_kinds: &DashboardFilter,
    scope_ids: &DashboardFilter,
    guild_ids: &DashboardFilter,
    search: &str,
    search_field: SearchField,
    window: &QueryWindow,
) -> Result<i64> {
    let mut query = QueryBuilder::<Postgres>::new("SELECT COUNT(*) AS count FROM jobs j");
    let search_has_terms = !search_terms(search).is_empty();
    if search_has_terms && matches!(search_field, SearchField::All | SearchField::Room) {
        push_job_room_join(&mut query);
    }
    push_job_member_joins(
        &mut query,
        search_has_terms && matches!(search_field, SearchField::All | SearchField::Actor),
        search_has_terms && matches!(search_field, SearchField::All | SearchField::Room),
    );
    query.push(" WHERE TRUE");
    push_time_bounds(&mut query, "j.updated_at_ms", window);
    push_job_category_filter(&mut query, categories);
    push_filter(&mut query, "j.kind", kinds);
    push_filter(&mut query, "j.kind", job_kinds);
    push_filter(&mut query, "j.state", states);
    push_filter(&mut query, "j.scope_kind", scope_kinds);
    push_filter(&mut query, "j.scope_id", scope_ids);
    push_filter(&mut query, "j.guild_id", guild_ids);
    push_job_search(&mut query, search, search_field);
    let row = query.build().fetch_one(&ctx.store.pool).await?;
    Ok(row.try_get("count")?)
}

async fn select_dashboard_events(
    ctx: &Ctx,
    request: &DashboardTimelineRequest,
    window: &QueryWindow,
    search_field: SearchField,
    cursor: Option<&PageCursor>,
    limit: usize,
) -> Result<Vec<TimelineRecord>> {
    let mut query = QueryBuilder::<Postgres>::new(
        r#"SELECT e.*,
                      r.guild_slug AS room_guild_slug,
                      r.voice_channel_name AS room_voice_channel_name,
                      r.voice_channel_slug AS room_voice_channel_slug,
                      COALESCE(NULLIF(r.voice_channel_name, ''), NULLIF(r.voice_channel_slug, ''), NULLIF(scope_member.label, ''), '') AS resolved_scope_label,
                      COALESCE(
                        NULLIF(e.payload_json->>'requested_by_label', ''),
                        NULLIF(e.payload_json->>'requestedByLabel', ''),
                        NULLIF(requester_member.label, ''),
                        NULLIF(e.speaker_label, ''),
                        ''
                      ) AS requested_by_label
               FROM timeline_events e"#,
    );
    push_event_room_join(&mut query);
    push_event_member_joins(&mut query, true, true);
    query.push(" WHERE e.forgotten = FALSE");
    push_time_bounds(&mut query, "e.started_at_ms", window);
    push_event_category_filter(&mut query, &request.categories);
    push_event_unified_kind_filter(&mut query, &request.kinds);
    push_filter(&mut query, "e.event_kind", &request.event_kinds);
    push_filter(
        &mut query,
        "COALESCE(e.payload_json->>'state', '')",
        &request.states,
    );
    push_filter(&mut query, "e.scope_kind", &request.scope_kinds);
    push_filter(&mut query, "e.scope_id", &request.scope_ids);
    push_filter(&mut query, "e.guild_id", &request.guild_ids);
    push_event_search(&mut query, &request.search, search_field);
    push_event_cursor(&mut query, cursor);
    query
        .push(" ORDER BY e.started_at_ms DESC, e.sequence DESC LIMIT ")
        .push_bind(limit as i64);
    let rows = query.build().fetch_all(&ctx.store.pool).await?;
    rows.into_iter()
        .map(|row| {
            let sequence: i64 = row.try_get("sequence")?;
            let event_id: String = row.try_get("event_id")?;
            let sort_ms: i64 = row.try_get("started_at_ms")?;
            let key = ScopeKey {
                kind: row.try_get("scope_kind")?,
                guild_id: row.try_get("guild_id")?,
                id: row.try_get("scope_id")?,
            };
            let full_payload = timeline_event_payload(&row)?;
            let event_kind: String = row.try_get("event_kind")?;
            let related_job_kind = full_payload
                .get("job_kind")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let category = event_category_for_kind(&event_kind, related_job_kind);
            let requested_by_label: String = row.try_get("requested_by_label")?;
            let resolved_scope_label: String = row.try_get("resolved_scope_label")?;
            let payload_label = payload_scope_label(&key.kind, &full_payload);
            let payload = dashboard_event_payload(
                &full_payload,
                &event_id,
                &scope_label(
                    &key,
                    (!resolved_scope_label.is_empty())
                        .then_some(&resolved_scope_label)
                        .or(payload_label.as_ref()),
                ),
                &requested_by_label,
                category,
            );
            Ok(TimelineRecord {
                sort_ms,
                rank: EVENT_RANK,
                tie: sequence.to_string(),
                id: event_id,
                category: category.to_string(),
                payload,
            })
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
async fn select_dashboard_jobs(
    ctx: &Ctx,
    categories: &DashboardFilter,
    job_kinds: &DashboardFilter,
    kinds: &DashboardFilter,
    states: &DashboardFilter,
    scope_kinds: &DashboardFilter,
    scope_ids: &DashboardFilter,
    guild_ids: &DashboardFilter,
    search: &str,
    search_field: SearchField,
    window: &QueryWindow,
    cursor: Option<&PageCursor>,
    limit: usize,
) -> Result<Vec<TimelineRecord>> {
    let mut query = QueryBuilder::<Postgres>::new(
        r#"SELECT j.updated_at_ms, j.job_id, j.scope_kind, j.guild_id, j.scope_id,
                      p.payload_blob,
                      COALESCE(NULLIF(r.voice_channel_name, ''), NULLIF(r.voice_channel_slug, ''), NULLIF(scope_member.label, ''), '') AS resolved_scope_label,
                      COALESCE(NULLIF(requester_member.label, ''), '') AS requested_by_label
               FROM jobs j
               JOIN job_payloads p ON p.job_id = j.job_id"#,
    );
    push_job_room_join(&mut query);
    push_job_member_joins(&mut query, true, true);
    query.push(" WHERE TRUE");
    push_time_bounds(&mut query, "j.updated_at_ms", window);
    push_job_category_filter(&mut query, categories);
    push_filter(&mut query, "j.kind", kinds);
    push_filter(&mut query, "j.kind", job_kinds);
    push_filter(&mut query, "j.state", states);
    push_filter(&mut query, "j.scope_kind", scope_kinds);
    push_filter(&mut query, "j.scope_id", scope_ids);
    push_filter(&mut query, "j.guild_id", guild_ids);
    push_job_search(&mut query, search, search_field);
    push_job_cursor(&mut query, cursor);
    query
        .push(" ORDER BY j.updated_at_ms DESC, j.job_id DESC LIMIT ")
        .push_bind(limit as i64);
    let rows = query.build().fetch_all(&ctx.store.pool).await?;
    rows.into_iter()
        .map(|row| {
            let sort_ms: i64 = row.try_get("updated_at_ms")?;
            let job_id: String = row.try_get("job_id")?;
            let key = ScopeKey {
                kind: row.try_get("scope_kind")?,
                guild_id: row.try_get("guild_id")?,
                id: row.try_get("scope_id")?,
            };
            let blob: Vec<u8> = row.try_get("payload_blob")?;
            let job = Job::decode(&blob)?;
            let requested_by_label: String = row.try_get("requested_by_label")?;
            let resolved_scope_label: String = row.try_get("resolved_scope_label")?;
            let payload_label = payload_scope_label(&key.kind, &job.payload_value());
            let payload = dashboard_job_payload(
                &job,
                &scope_label(
                    &key,
                    (!resolved_scope_label.is_empty())
                        .then_some(&resolved_scope_label)
                        .or(payload_label.as_ref()),
                ),
                &requested_by_label,
                dashboard_job_category(job.kind.as_str()),
            );
            Ok(TimelineRecord {
                sort_ms,
                rank: JOB_RANK,
                tie: job_id.clone(),
                id: job_id,
                category: dashboard_job_category(job.kind.as_str()).to_string(),
                payload,
            })
        })
        .collect()
}

async fn dashboard_facets(ctx: &Ctx, window: &QueryWindow) -> Result<FacetAccumulator> {
    let mut query = QueryBuilder::<Postgres>::new(
        r#"
            SELECT 'event' AS record_type,
                   e.event_kind AS item_kind,
                   COALESCE(e.payload_json->>'job_kind', '') AS related_job_kind,
                   COALESCE(e.payload_json->>'state', '') AS state,
                   "#,
    );
    push_event_category_expression(&mut query);
    query.push(
        r#" AS category,
                   e.scope_kind,
                   e.guild_id,
                   e.scope_id,
                   COUNT(*) AS count
            FROM timeline_events e
            WHERE e.forgotten = FALSE
            "#,
    );
    push_time_bounds(&mut query, "e.started_at_ms", window);
    query.push(
        r#"
            GROUP BY category, e.event_kind, COALESCE(e.payload_json->>'job_kind', ''),
                     COALESCE(e.payload_json->>'state', ''),
                     e.scope_kind, e.guild_id, e.scope_id
            UNION ALL
            SELECT 'job' AS record_type,
                   j.kind AS item_kind,
                   '' AS related_job_kind,
                   j.state,
            "#,
    );
    push_job_category_expression(&mut query);
    query.push(
        r#" AS category,
                   j.scope_kind,
                   j.guild_id,
                   j.scope_id,
                   COUNT(*) AS count
            FROM jobs j
            WHERE TRUE
            "#,
    );
    push_time_bounds(&mut query, "j.updated_at_ms", window);
    query.push(
        r#"
            GROUP BY category, j.kind, j.state, j.scope_kind, j.guild_id, j.scope_id
            "#,
    );
    let rows = query.build().fetch_all(&ctx.store.pool).await?;
    let mut facets = FacetAccumulator::default();
    for row in rows {
        let record_type: String = row.try_get("record_type")?;
        let item_kind: String = row.try_get("item_kind")?;
        let related_job_kind: String = row.try_get("related_job_kind")?;
        let state: String = row.try_get("state")?;
        let category: String = row.try_get("category")?;
        let count: i64 = row.try_get("count")?;
        let key = ScopeKey {
            kind: row.try_get("scope_kind")?,
            guild_id: row.try_get("guild_id")?,
            id: row.try_get("scope_id")?,
        };
        *facets.record_types.entry(record_type.clone()).or_default() += count;
        *facets.categories.entry(category).or_default() += count;
        if record_type == "event" {
            *facets.event_kinds.entry(item_kind.clone()).or_default() += count;
            *facets.kinds.entry(item_kind.clone()).or_default() += count;
            facets
                .category_kinds
                .entry(event_category_for_kind(&item_kind, "").to_string())
                .or_default()
                .insert(item_kind);
            if !related_job_kind.is_empty() {
                *facets.kinds.entry(related_job_kind.clone()).or_default() += count;
                facets
                    .category_kinds
                    .entry(dashboard_job_category(&related_job_kind).to_string())
                    .or_default()
                    .insert(related_job_kind);
            }
        } else {
            *facets.job_kinds.entry(item_kind.clone()).or_default() += count;
            *facets.kinds.entry(item_kind.clone()).or_default() += count;
            facets
                .category_kinds
                .entry(dashboard_job_category(&item_kind).to_string())
                .or_default()
                .insert(item_kind);
        }
        if !state.is_empty() {
            let counts = facets.states.entry(state).or_default();
            if record_type == "event" {
                counts.0 += count;
            } else {
                counts.1 += count;
            }
        }
        let scope = facets.scopes.entry(key).or_default();
        scope.0 += count;
    }
    let keys = facets.scopes.keys().cloned().collect::<Vec<_>>();
    let labels = dashboard_scope_labels(ctx, &keys).await?;
    for (key, (_, label)) in &mut facets.scopes {
        *label = scope_label(key, labels.get(key));
    }
    Ok(facets)
}

async fn dashboard_scope_labels(
    ctx: &Ctx,
    keys: &[ScopeKey],
) -> Result<BTreeMap<ScopeKey, String>> {
    if keys.is_empty() {
        return Ok(BTreeMap::new());
    }
    let wanted = keys.iter().cloned().collect::<BTreeSet<_>>();
    let mut labels = BTreeMap::new();

    let voice_keys = keys
        .iter()
        .filter(|key| key.kind == "voice_channel")
        .cloned()
        .collect::<Vec<_>>();
    if !voice_keys.is_empty() {
        let mut query = QueryBuilder::<Postgres>::new(
            r#"
            SELECT guild_id, voice_channel_id,
                   COALESCE(NULLIF(voice_channel_name, ''), NULLIF(voice_channel_slug, ''), '') AS label
            FROM voice_rooms
            WHERE FALSE
            "#,
        );
        for key in &voice_keys {
            query
                .push(" OR (guild_id = ")
                .push_bind(key.guild_id.clone())
                .push(" AND voice_channel_id = ")
                .push_bind(key.id.clone())
                .push(")");
        }
        for row in query.build().fetch_all(&ctx.store.pool).await? {
            let key = ScopeKey {
                kind: "voice_channel".to_string(),
                guild_id: row.try_get("guild_id")?,
                id: row.try_get("voice_channel_id")?,
            };
            let label: String = row.try_get("label")?;
            if !label.is_empty() {
                labels.insert(key, label);
            }
        }
    }

    let dm_user_ids = keys
        .iter()
        .filter(|key| key.kind == "dm")
        .map(|key| key.id.clone())
        .collect::<BTreeSet<_>>();
    if !dm_user_ids.is_empty() {
        let mut query = QueryBuilder::<Postgres>::new(
            r#"
            SELECT DISTINCT ON (user_id) user_id,
                   COALESCE(NULLIF(display_name, ''), NULLIF(global_name, ''), NULLIF(username, ''), '') AS label
            FROM discord_members
            WHERE user_id IN (
            "#,
        );
        {
            let mut ids = query.separated(", ");
            for user_id in &dm_user_ids {
                ids.push_bind(user_id.clone());
            }
            ids.push_unseparated(")");
        }
        query.push(
            r#"
            ORDER BY user_id, updated_at_ms DESC
            "#,
        );
        for row in query.build().fetch_all(&ctx.store.pool).await? {
            let user_id: String = row.try_get("user_id")?;
            let label: String = row.try_get("label")?;
            if label.is_empty() {
                continue;
            }
            for key in keys
                .iter()
                .filter(|key| key.kind == "dm" && key.id == user_id)
            {
                labels.entry(key.clone()).or_insert_with(|| label.clone());
            }
        }
    }

    let unresolved = wanted
        .iter()
        .filter(|key| !labels.contains_key(*key))
        .cloned()
        .collect::<Vec<_>>();
    if !unresolved.is_empty() {
        let mut query = QueryBuilder::<Postgres>::new(
            r#"
            SELECT DISTINCT ON (scope_kind, guild_id, scope_id)
                   scope_kind, guild_id, scope_id, payload_json
            FROM timeline_events
            WHERE forgotten = FALSE AND (FALSE
            "#,
        );
        push_scope_key_predicates(&mut query, &unresolved, "");
        query.push(
            r#")
            ORDER BY scope_kind, guild_id, scope_id, started_at_ms DESC, sequence DESC
            "#,
        );
        for row in query.build().fetch_all(&ctx.store.pool).await? {
            let key = ScopeKey {
                kind: row.try_get("scope_kind")?,
                guild_id: row.try_get("guild_id")?,
                id: row.try_get("scope_id")?,
            };
            let payload: Value = row.try_get("payload_json")?;
            if let Some(label) = payload_scope_label(&key.kind, &payload) {
                labels.insert(key, label);
            }
        }
    }

    let unresolved = wanted
        .iter()
        .filter(|key| !labels.contains_key(*key))
        .cloned()
        .collect::<Vec<_>>();
    if !unresolved.is_empty() {
        let mut query = QueryBuilder::<Postgres>::new(
            r#"
            SELECT DISTINCT ON (j.scope_kind, j.guild_id, j.scope_id)
                   j.scope_kind, j.guild_id, j.scope_id, p.payload_blob
            FROM jobs j
            JOIN job_payloads p ON p.job_id = j.job_id
            WHERE FALSE
            "#,
        );
        push_scope_key_predicates(&mut query, &unresolved, "j.");
        query.push(
            r#"
            ORDER BY j.scope_kind, j.guild_id, j.scope_id, j.updated_at_ms DESC, j.job_id DESC
            "#,
        );
        for row in query.build().fetch_all(&ctx.store.pool).await? {
            let key = ScopeKey {
                kind: row.try_get("scope_kind")?,
                guild_id: row.try_get("guild_id")?,
                id: row.try_get("scope_id")?,
            };
            let blob: Vec<u8> = row.try_get("payload_blob")?;
            let job = Job::decode(&blob)?;
            if let Some(label) = payload_scope_label(&key.kind, &job.payload_value()) {
                labels.insert(key, label);
            }
        }
    }
    Ok(labels)
}

async fn dashboard_member_labels(
    ctx: &Ctx,
    members: &BTreeSet<(String, String)>,
) -> Result<BTreeMap<(String, String), String>> {
    if members.is_empty() {
        return Ok(BTreeMap::new());
    }
    let user_ids = members
        .iter()
        .map(|(_, user_id)| user_id.clone())
        .collect::<BTreeSet<_>>();
    let mut query = QueryBuilder::<Postgres>::new(
        r#"
            SELECT guild_id, user_id,
                   COALESCE(NULLIF(display_name, ''), NULLIF(global_name, ''), NULLIF(username, ''), '') AS label
            FROM discord_members
            WHERE user_id IN (
            "#,
    );
    {
        let mut ids = query.separated(", ");
        for user_id in user_ids {
            ids.push_bind(user_id.clone());
        }
        ids.push_unseparated(")");
    }
    query.push(" ORDER BY user_id, updated_at_ms DESC, guild_id");
    let rows = query.build().fetch_all(&ctx.store.pool).await?;
    let mut labels = BTreeMap::new();
    for row in rows {
        let guild_id: String = row.try_get("guild_id")?;
        let user_id: String = row.try_get("user_id")?;
        let label: String = row.try_get("label")?;
        if label.is_empty() {
            continue;
        }
        for key in members.iter().filter(|(wanted_guild, wanted_user)| {
            wanted_user == &user_id && (wanted_guild.is_empty() || wanted_guild == &guild_id)
        }) {
            labels.entry(key.clone()).or_insert_with(|| label.clone());
        }
    }
    Ok(labels)
}

async fn enrich_job_values(ctx: &Ctx, jobs: &[Job], values: &mut [Value]) -> Result<()> {
    let keys = jobs
        .iter()
        .map(|job| ScopeKey {
            kind: job.scope_kind.as_str().to_string(),
            guild_id: job.guild_id.clone(),
            id: job.scope_id.clone(),
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let labels = dashboard_scope_labels(ctx, &keys).await?;
    let requester_ids = jobs
        .iter()
        .map(|job| (job.guild_id.clone(), job.requested_by_user_id.clone()))
        .filter(|(_, user_id)| !user_id.is_empty())
        .collect::<BTreeSet<_>>();
    let requester_labels = dashboard_member_labels(ctx, &requester_ids).await?;
    for (job, value) in jobs.iter().zip(values) {
        let key = ScopeKey {
            kind: job.scope_kind.as_str().to_string(),
            guild_id: job.guild_id.clone(),
            id: job.scope_id.clone(),
        };
        let object = value
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("dashboard job payload must be an object"))?;
        object.insert(
            "scopeLabel".to_string(),
            json!(scope_label(&key, labels.get(&key))),
        );
        if let Some(label) =
            requester_labels.get(&(job.guild_id.clone(), job.requested_by_user_id.clone()))
        {
            object.insert("requestedByLabel".to_string(), json!(label));
        }
    }
    Ok(())
}

async fn enrich_event_values(ctx: &Ctx, events: &mut [Value]) -> Result<()> {
    let keys = events
        .iter()
        .map(|event| ScopeKey {
            kind: value_string(event, "scope_kind"),
            guild_id: value_string(event, "guild_id"),
            id: value_string(event, "scope_id"),
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let labels = dashboard_scope_labels(ctx, &keys).await?;
    let actor_ids = events
        .iter()
        .flat_map(|event| {
            let guild_id = value_string(event, "guild_id");
            [
                (
                    guild_id.clone(),
                    value_string(event, "requested_by_user_id"),
                ),
                (guild_id.clone(), value_string(event, "requestedByUserId")),
                (guild_id.clone(), value_string(event, "speaker_user_id")),
                (
                    String::new(),
                    if value_string(event, "scope_kind") == "dm" {
                        value_string(event, "scope_id")
                    } else {
                        Default::default()
                    },
                ),
            ]
        })
        .filter(|(_, id)| !id.is_empty())
        .collect::<BTreeSet<_>>();
    let actor_labels = dashboard_member_labels(ctx, &actor_ids).await?;
    for event in events {
        let key = ScopeKey {
            kind: value_string(event, "scope_kind"),
            guild_id: value_string(event, "guild_id"),
            id: value_string(event, "scope_id"),
        };
        let object = event
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("dashboard event payload must be an object"))?;
        object.insert(
            "scopeLabel".to_string(),
            json!(scope_label(&key, labels.get(&key))),
        );
        let actor_id = [
            object
                .get("requested_by_user_id")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            object
                .get("requestedByUserId")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            object
                .get("speaker_user_id")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            if key.kind == "dm" {
                key.id.as_str()
            } else {
                ""
            },
        ]
        .into_iter()
        .find(|id| !id.is_empty())
        .unwrap_or_default();
        if let Some(label) = actor_labels.get(&(key.guild_id.clone(), actor_id.to_string())) {
            object.insert("requestedByLabel".to_string(), json!(label));
        }
    }
    Ok(())
}

async fn enrich_job_summary(ctx: &Ctx, summary: &mut Value) -> Result<()> {
    let Some(scopes) = summary.get_mut("byScope").and_then(Value::as_array_mut) else {
        return Ok(());
    };
    let keys = scopes
        .iter()
        .map(|scope| ScopeKey {
            kind: value_string(scope, "scope_kind"),
            guild_id: value_string(scope, "guild_id"),
            id: value_string(scope, "scope_id"),
        })
        .collect::<Vec<_>>();
    let labels = dashboard_scope_labels(ctx, &keys).await?;
    for (scope, key) in scopes.iter_mut().zip(keys) {
        scope
            .as_object_mut()
            .expect("dashboard scope summary")
            .insert(
                "scopeLabel".to_string(),
                json!(scope_label(&key, labels.get(&key))),
            );
    }
    Ok(())
}

async fn enrich_agent_sessions(ctx: &Ctx, agents: &mut Value) -> Result<()> {
    let Some(sessions) = agents.get_mut("sessions").and_then(Value::as_array_mut) else {
        return Ok(());
    };
    let keys = sessions
        .iter()
        .map(|session| {
            let guild_id = value_string(session, "guild_id");
            let scope_id = value_string(session, "scope_id");
            let kind = value_string(session, "scope_kind");
            ScopeKey {
                kind,
                guild_id,
                id: scope_id,
            }
        })
        .collect::<Vec<_>>();
    let labels = dashboard_scope_labels(ctx, &keys).await?;
    for (session, key) in sessions.iter_mut().zip(keys) {
        session
            .as_object_mut()
            .expect("dashboard agent session")
            .insert(
                "scopeLabel".to_string(),
                json!(scope_label(&key, labels.get(&key))),
            );
    }
    Ok(())
}

async fn enrich_automation_payload(ctx: &Ctx, automations: &mut Value) -> Result<()> {
    let Some(records) = automations.get_mut("records").and_then(Value::as_array_mut) else {
        return Ok(());
    };
    let mut keys = Vec::new();
    for record in records.iter() {
        let scope = &record["spec"]["scope"];
        let guild_id = value_string(scope, "guild_id");
        keys.push(ScopeKey {
            kind: value_string(scope, "scope_kind"),
            guild_id: guild_id.clone(),
            id: value_string(scope, "scope_id"),
        });
        collect_automation_target_keys(record, &guild_id, &mut keys);
    }
    let keys = keys
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let labels = dashboard_scope_labels(ctx, &keys).await?;
    for record in records {
        let scope = &mut record["spec"]["scope"];
        let key = ScopeKey {
            kind: value_string(scope, "scope_kind"),
            guild_id: value_string(scope, "guild_id"),
            id: value_string(scope, "scope_id"),
        };
        scope
            .as_object_mut()
            .expect("dashboard automation scope")
            .insert(
                "scopeLabel".to_string(),
                json!(scope_label(&key, labels.get(&key))),
            );
        let guild_id = key.guild_id;
        apply_automation_target_labels(record, &guild_id, &labels);
    }
    Ok(())
}

pub(crate) async fn dashboard_scope_label_batch(
    ctx: &Ctx,
    scopes: &[(String, String, String)],
) -> Result<BTreeMap<(String, String, String), String>> {
    let keys = scopes
        .iter()
        .map(|(kind, guild_id, id)| ScopeKey {
            kind: kind.clone(),
            guild_id: guild_id.clone(),
            id: id.clone(),
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let labels = dashboard_scope_labels(ctx, &keys).await?;
    Ok(keys
        .into_iter()
        .map(|key| {
            let tuple = (key.kind.clone(), key.guild_id.clone(), key.id.clone());
            (tuple, scope_label(&key, labels.get(&key)))
        })
        .collect())
}

fn value_string(value: &Value, key: &str) -> String {
    match value.get(key) {
        Some(Value::String(value)) => value.clone(),
        Some(Value::Number(value)) => value.to_string(),
        _ => String::new(),
    }
}

fn collect_automation_target_keys(value: &Value, guild_id: &str, keys: &mut Vec<ScopeKey>) {
    match value {
        Value::Object(object) => {
            if let (Some(kind), Some(id)) = (
                object.get("kind").and_then(Value::as_str),
                object.get("id").and_then(Value::as_str),
            ) {
                match kind {
                    "dm" if !id.is_empty() => keys.push(ScopeKey {
                        kind: "dm".to_string(),
                        guild_id: String::new(),
                        id: id.to_string(),
                    }),
                    "channel" if !id.is_empty() => {
                        keys.push(ScopeKey {
                            kind: "text_channel".to_string(),
                            guild_id: guild_id.to_string(),
                            id: id.to_string(),
                        });
                        keys.push(ScopeKey {
                            kind: "voice_channel".to_string(),
                            guild_id: guild_id.to_string(),
                            id: id.to_string(),
                        });
                    }
                    _ => {}
                }
            }
            for child in object.values() {
                collect_automation_target_keys(child, guild_id, keys);
            }
        }
        Value::Array(values) => {
            for child in values {
                collect_automation_target_keys(child, guild_id, keys);
            }
        }
        _ => {}
    }
}

fn apply_automation_target_labels(
    value: &mut Value,
    guild_id: &str,
    labels: &BTreeMap<ScopeKey, String>,
) {
    match value {
        Value::Object(object) => {
            let target = object
                .get("kind")
                .and_then(Value::as_str)
                .zip(object.get("id").and_then(Value::as_str))
                .and_then(|(kind, id)| match kind {
                    "dm" if !id.is_empty() => Some(ScopeKey {
                        kind: "dm".to_string(),
                        guild_id: String::new(),
                        id: id.to_string(),
                    }),
                    "channel" if !id.is_empty() => Some(ScopeKey {
                        kind: "text_channel".to_string(),
                        guild_id: guild_id.to_string(),
                        id: id.to_string(),
                    }),
                    _ => None,
                });
            if let Some(target) = target {
                let voice_target = (target.kind == "text_channel").then(|| ScopeKey {
                    kind: "voice_channel".to_string(),
                    guild_id: target.guild_id.clone(),
                    id: target.id.clone(),
                });
                let label = labels
                    .get(&target)
                    .or_else(|| voice_target.as_ref().and_then(|key| labels.get(key)));
                object.insert("scopeLabel".to_string(), json!(scope_label(&target, label)));
            }
            for child in object.values_mut() {
                apply_automation_target_labels(child, guild_id, labels);
            }
        }
        Value::Array(values) => {
            for child in values {
                apply_automation_target_labels(child, guild_id, labels);
            }
        }
        _ => {}
    }
}

fn dashboard_agent_list_entry(job: &Job) -> Value {
    let mut job_value = jobs::public_interaction_job_context(job);
    let task = job.metadata.agent_task();
    let result_excerpt = task
        .map(|task| preview(&task.response_text, 1200))
        .unwrap_or_default();
    let error = preview(
        &first_non_empty([
            job.metadata.error.clone(),
            task.map(|task| task.dispatch_error_after_cancel.clone())
                .unwrap_or_default(),
            task.map(|task| task.dispatch_error.clone())
                .unwrap_or_default(),
            task.map(|task| task.dispatch_stderr.clone())
                .unwrap_or_default(),
        ]),
        1200,
    );
    let object = job_value.as_object_mut().expect("dashboard agent list job");
    object.insert(
        "durationMs".to_string(),
        json!(dashboard_job_duration_ms(job)),
    );
    object.insert("attempts".to_string(), json!(job.attempts));
    object.insert("resultExcerpt".to_string(), json!(result_excerpt));
    if !error.is_empty() {
        object.insert("error".to_string(), json!(error));
    }

    let codex = task.map_or_else(
        || json!({}),
        |task| {
            json!({
                "sessionId": task.agent.session_id,
                "provider": task.agent.provider,
                "model": task.agent.model,
                "reasoningEffort": task.agent.reasoning_effort,
                "fastMode": task.agent.fast_mode,
                "tokenUsage": task.agent.usage.to_json(),
            })
        },
    );
    json!({
        "job": job_value,
        "codex": codex,
        "detailUrl": format!("/v1/dashboard/agents/{}", job.id),
    })
}

pub(super) fn dashboard_job_duration_ms(job: &Job) -> i64 {
    let started = job
        .started_at
        .as_deref()
        .and_then(parse_instant)
        .or_else(|| parse_instant(&job.created_at));
    let ended = job
        .completed_at
        .as_deref()
        .and_then(parse_instant)
        .or_else(|| parse_instant(&job.updated_at));
    started
        .zip(ended)
        .map(|(started, ended)| (ended - started).num_milliseconds().max(0))
        .unwrap_or_default()
}

pub(super) fn dashboard_job_category(kind: &str) -> &'static str {
    match kind.parse::<crate::model::job::JobKind>() {
        Ok(kind) => crate::model::job::spec::spec(kind).dashboard.as_str(),
        Err(_) => "other",
    }
}

fn event_category_for_kind(event_kind: &str, related_job_kind: &str) -> &'static str {
    if BACKGROUND_EVENT_KINDS.contains(&event_kind) {
        return "background";
    }
    if VOICE_DETAIL_EVENT_KINDS.contains(&event_kind) || event_kind.starts_with("participant_") {
        return "voice_detail";
    }
    if CONVERSATION_EVENT_KINDS.contains(&event_kind) || event_kind.starts_with("wake_activation_")
    {
        return "conversation";
    }
    if AGENT_EVENT_KINDS.contains(&event_kind)
        || event_kind.starts_with("agent_session_")
        || event_kind.starts_with("agent_thread_")
    {
        return "agent";
    }
    if MESSAGING_CONTROL_EVENT_KINDS.contains(&event_kind) || event_kind.starts_with("room_") {
        return "messaging_control";
    }
    if AUTOMATION_EVENT_KINDS.contains(&event_kind) || event_kind.starts_with("automation_") {
        return "automation";
    }
    if OPERATIONS_EVENT_KINDS.contains(&event_kind) {
        return "operations";
    }
    if !related_job_kind.is_empty() {
        return dashboard_job_category(related_job_kind);
    }
    "other"
}

fn validate_categories(filter: &DashboardFilter) -> Result<()> {
    let DashboardFilter::Values(values) = filter else {
        return Ok(());
    };
    if let Some(value) = values.iter().find(|value| {
        !DASHBOARD_CATEGORIES
            .iter()
            .any(|(category, _, _)| value == category)
    }) {
        anyhow::bail!("unknown dashboard category: {value}");
    }
    Ok(())
}

fn job_kinds_for_category(category: &str) -> Vec<&'static str> {
    crate::model::job::JobKind::ALL
        .iter()
        .filter(|kind| crate::model::job::spec::spec(**kind).dashboard.as_str() == category)
        .map(|kind| kind.as_str())
        .collect()
}

fn event_kinds_for_category(category: &str) -> &'static [&'static str] {
    match category {
        "conversation" => CONVERSATION_EVENT_KINDS,
        "agent" => AGENT_EVENT_KINDS,
        "messaging_control" => MESSAGING_CONTROL_EVENT_KINDS,
        "automation" => AUTOMATION_EVENT_KINDS,
        "operations" => OPERATIONS_EVENT_KINDS,
        "voice_detail" => VOICE_DETAIL_EVENT_KINDS,
        "background" => BACKGROUND_EVENT_KINDS,
        "other" => &[],
        _ => unreachable!("dashboard categories are validated before SQL construction"),
    }
}

fn push_string_set_predicate(
    query: &mut QueryBuilder<'_, Postgres>,
    expression: &str,
    values: &[&str],
) {
    if values.is_empty() {
        query.push("FALSE");
        return;
    }
    query.push(expression).push(" IN (");
    let mut separated = query.separated(", ");
    for value in values {
        separated.push_bind((*value).to_string());
    }
    separated.push_unseparated(")");
}

fn push_job_category_expression(query: &mut QueryBuilder<'_, Postgres>) {
    query.push("CASE");
    for category in [
        "background",
        "voice_detail",
        "conversation",
        "agent",
        "messaging_control",
        "automation",
    ] {
        query.push(" WHEN ");
        push_string_set_predicate(query, "j.kind", &job_kinds_for_category(category));
        query.push(" THEN '").push(category).push("'");
    }
    query.push(" ELSE 'other' END");
}

fn push_event_explicit_category_predicate(query: &mut QueryBuilder<'_, Postgres>, category: &str) {
    query.push("(");
    push_string_set_predicate(query, "e.event_kind", event_kinds_for_category(category));
    match category {
        "conversation" => query.push(" OR strpos(e.event_kind, 'wake_activation_') = 1"),
        "agent" => query.push(
            " OR strpos(e.event_kind, 'agent_session_') = 1 OR strpos(e.event_kind, 'agent_thread_') = 1",
        ),
        "messaging_control" => {
            query.push(" OR strpos(e.event_kind, 'room_') = 1")
        }
        "automation" => query.push(" OR strpos(e.event_kind, 'automation_') = 1"),
        "voice_detail" => query.push(" OR strpos(e.event_kind, 'participant_') = 1"),
        _ => query,
    };
    query.push(")");
}

fn push_related_job_category_predicate(query: &mut QueryBuilder<'_, Postgres>, category: &str) {
    push_string_set_predicate(
        query,
        "COALESCE(e.payload_json->>'job_kind', '')",
        &job_kinds_for_category(category),
    );
}

fn push_event_category_expression(query: &mut QueryBuilder<'_, Postgres>) {
    query.push("CASE");
    for category in [
        "background",
        "voice_detail",
        "conversation",
        "agent",
        "messaging_control",
        "automation",
        "operations",
    ] {
        query.push(" WHEN ");
        push_event_explicit_category_predicate(query, category);
        query.push(" THEN '").push(category).push("'");
    }
    for category in [
        "background",
        "voice_detail",
        "conversation",
        "agent",
        "messaging_control",
        "automation",
        "operations",
    ] {
        query.push(" WHEN ");
        push_related_job_category_predicate(query, category);
        query.push(" THEN '").push(category).push("'");
    }
    query.push(" ELSE 'other' END");
}

fn push_job_category_filter(query: &mut QueryBuilder<'_, Postgres>, filter: &DashboardFilter) {
    push_category_filter(query, filter, push_job_category_expression);
}

fn push_event_category_filter(query: &mut QueryBuilder<'_, Postgres>, filter: &DashboardFilter) {
    push_category_filter(query, filter, push_event_category_expression);
}

fn push_category_filter(
    query: &mut QueryBuilder<'_, Postgres>,
    filter: &DashboardFilter,
    expression: fn(&mut QueryBuilder<'_, Postgres>),
) {
    match filter {
        DashboardFilter::All => {}
        DashboardFilter::None => {
            query.push(" AND FALSE");
        }
        DashboardFilter::Values(values) => {
            query.push(" AND (");
            expression(query);
            query.push(") IN (");
            let mut separated = query.separated(", ");
            for value in values {
                separated.push_bind(value.clone());
            }
            separated.push_unseparated(")");
        }
    }
}

fn validate_record_types(filter: &DashboardFilter) -> Result<()> {
    let DashboardFilter::Values(values) = filter else {
        return Ok(());
    };
    if let Some(value) = values
        .iter()
        .find(|value| !matches!(value.as_str(), "event" | "job"))
    {
        anyhow::bail!("unknown dashboard record type: {value}");
    }
    Ok(())
}

fn resolve_window(raw_from: &str, raw_to: &str, snapshot_ms: i64) -> Result<QueryWindow> {
    let snapshot = ms_to_datetime(snapshot_ms).ok_or_else(|| {
        anyhow::anyhow!("dashboard cursor snapshot is outside the supported range")
    })?;
    let raw_from = if raw_from.trim().is_empty() {
        DEFAULT_FROM
    } else {
        raw_from.trim()
    };
    let from_ms = if raw_from.eq_ignore_ascii_case("all") {
        None
    } else {
        Some(
            resolve_time_reference(raw_from, Some(snapshot))
                .map(instant_ms_dt)
                .ok_or_else(|| anyhow::anyhow!("invalid dashboard from time: {raw_from}"))?,
        )
    };
    let requested_to = if raw_to.trim().is_empty() {
        snapshot_ms
    } else {
        resolve_time_reference(raw_to.trim(), Some(snapshot))
            .map(instant_ms_dt)
            .ok_or_else(|| anyhow::anyhow!("invalid dashboard to time: {}", raw_to.trim()))?
            .min(snapshot_ms)
    };
    if from_ms.is_some_and(|from_ms| from_ms >= requested_to) {
        anyhow::bail!("dashboard to time must be after from time");
    }
    Ok(QueryWindow {
        from_ms,
        to_ms: requested_to,
        snapshot_ms,
    })
}

fn parse_search_field(raw: &str) -> Result<SearchField> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "" | "all" => Ok(SearchField::All),
        "detail" => Ok(SearchField::Detail),
        "feedback" => Ok(SearchField::Feedback),
        "kind" => Ok(SearchField::Kind),
        "job_kind" => Ok(SearchField::JobKind),
        "state" => Ok(SearchField::State),
        "command" => Ok(SearchField::Command),
        "room" => Ok(SearchField::Room),
        "actor" => Ok(SearchField::Actor),
        value => anyhow::bail!("invalid dashboard search field: {value}"),
    }
}

fn parse_metadata_mode(raw: &str) -> Result<MetadataMode> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "" | "full" => Ok(MetadataMode::Full),
        "count" => Ok(MetadataMode::Count),
        "none" => Ok(MetadataMode::None),
        value => anyhow::bail!("invalid dashboard metadata mode: {value}"),
    }
}

fn push_time_bounds(
    query: &mut QueryBuilder<'_, Postgres>,
    expression: &str,
    window: &QueryWindow,
) {
    if let Some(from_ms) = window.from_ms {
        query
            .push(" AND ")
            .push(expression)
            .push(" >= ")
            .push_bind(from_ms);
    }
    query
        .push(" AND ")
        .push(expression)
        .push(" <= ")
        .push_bind(window.to_ms)
        .push(" AND ")
        .push(expression)
        .push(" <= ")
        .push_bind(window.snapshot_ms);
}

fn push_filter(query: &mut QueryBuilder<'_, Postgres>, expression: &str, filter: &DashboardFilter) {
    match filter {
        DashboardFilter::All => {}
        DashboardFilter::None => {
            query.push(" AND FALSE");
        }
        DashboardFilter::Values(values) => {
            query.push(" AND ").push(expression).push(" IN (");
            let mut separated = query.separated(", ");
            for value in values {
                separated.push_bind(value.clone());
            }
            separated.push_unseparated(")");
        }
    }
}

fn push_event_unified_kind_filter(
    query: &mut QueryBuilder<'_, Postgres>,
    filter: &DashboardFilter,
) {
    match filter {
        DashboardFilter::All => {}
        DashboardFilter::None => {
            query.push(" AND FALSE");
        }
        DashboardFilter::Values(values) => {
            query.push(" AND (e.event_kind IN (");
            {
                let mut event_kinds = query.separated(", ");
                for value in values {
                    event_kinds.push_bind(value.clone());
                }
                event_kinds.push_unseparated(") OR COALESCE(e.payload_json->>'job_kind', '') IN (");
            }
            {
                let mut job_kinds = query.separated(", ");
                for value in values {
                    job_kinds.push_bind(value.clone());
                }
                job_kinds.push_unseparated("))");
            }
        }
    }
}

fn push_scope_key_predicates(
    query: &mut QueryBuilder<'_, Postgres>,
    keys: &[ScopeKey],
    prefix: &str,
) {
    for key in keys {
        query
            .push(" OR (")
            .push(prefix)
            .push("scope_kind = ")
            .push_bind(key.kind.clone())
            .push(" AND ")
            .push(prefix)
            .push("guild_id = ")
            .push_bind(key.guild_id.clone())
            .push(" AND ")
            .push(prefix)
            .push("scope_id = ")
            .push_bind(key.id.clone())
            .push(")");
    }
}

fn push_event_room_join(query: &mut QueryBuilder<'_, Postgres>) {
    query.push(
        r#" LEFT JOIN voice_rooms r
              ON e.scope_kind = 'voice_channel'
             AND r.guild_id = e.guild_id
             AND r.voice_channel_id = e.scope_id"#,
    );
}

fn push_job_room_join(query: &mut QueryBuilder<'_, Postgres>) {
    query.push(
        r#" LEFT JOIN voice_rooms r
              ON j.scope_kind = 'voice_channel'
             AND r.guild_id = j.guild_id
             AND r.voice_channel_id = j.scope_id"#,
    );
}

fn push_event_cursor(query: &mut QueryBuilder<'_, Postgres>, cursor: Option<&PageCursor>) {
    let Some(cursor) = cursor else {
        return;
    };
    query.push(" AND (");
    query.push("e.started_at_ms < ").push_bind(cursor.sort_ms);
    if cursor.rank == EVENT_RANK {
        let sequence = cursor.tie.parse::<i64>().unwrap_or(i64::MAX);
        query
            .push(" OR (e.started_at_ms = ")
            .push_bind(cursor.sort_ms)
            .push(" AND e.sequence < ")
            .push_bind(sequence)
            .push(")");
    }
    query.push(")");
}

fn push_job_cursor(query: &mut QueryBuilder<'_, Postgres>, cursor: Option<&PageCursor>) {
    let Some(cursor) = cursor else {
        return;
    };
    query.push(" AND (");
    query.push("j.updated_at_ms < ").push_bind(cursor.sort_ms);
    if cursor.rank == EVENT_RANK {
        query
            .push(" OR j.updated_at_ms = ")
            .push_bind(cursor.sort_ms);
    } else {
        query
            .push(" OR (j.updated_at_ms = ")
            .push_bind(cursor.sort_ms)
            .push(" AND j.job_id < ")
            .push_bind(cursor.tie.clone())
            .push(")");
    }
    query.push(")");
}

fn compare_records(left: &TimelineRecord, right: &TimelineRecord) -> Ordering {
    right
        .sort_ms
        .cmp(&left.sort_ms)
        .then_with(|| right.rank.cmp(&left.rank))
        .then_with(|| {
            if left.rank == EVENT_RANK {
                right
                    .tie
                    .parse::<i64>()
                    .unwrap_or_default()
                    .cmp(&left.tie.parse::<i64>().unwrap_or_default())
            } else {
                right.tie.cmp(&left.tie)
            }
        })
}

fn parse_cursor(raw: &str) -> Result<Option<PageCursor>> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(None);
    }
    let parts = raw.splitn(5, ':').collect::<Vec<_>>();
    if parts.len() != 5 || parts[0] != "v1" {
        anyhow::bail!("invalid dashboard cursor");
    }
    let snapshot_ms = parts[1]
        .parse::<i64>()
        .map_err(|_| anyhow::anyhow!("invalid dashboard cursor snapshot"))?;
    let sort_ms = parts[2]
        .parse::<i64>()
        .map_err(|_| anyhow::anyhow!("invalid dashboard cursor position"))?;
    let rank = parts[3]
        .parse::<i8>()
        .map_err(|_| anyhow::anyhow!("invalid dashboard cursor record type"))?;
    if !matches!(rank, EVENT_RANK | JOB_RANK) || parts[4].is_empty() {
        anyhow::bail!("invalid dashboard cursor record key");
    }
    if rank == EVENT_RANK && parts[4].parse::<i64>().is_err() {
        anyhow::bail!("invalid dashboard event cursor key");
    }
    Ok(Some(PageCursor {
        snapshot_ms,
        sort_ms,
        rank,
        tie: parts[4].to_string(),
    }))
}

fn encode_cursor(snapshot_ms: i64, record: &TimelineRecord) -> String {
    format!(
        "v1:{snapshot_ms}:{}:{}:{}",
        record.sort_ms, record.rank, record.tie
    )
}

fn record_payload(record: TimelineRecord) -> Value {
    let sort_at = timestamp(record.sort_ms);
    if record.rank == EVENT_RANK {
        json!({
            "recordType": "event",
            "category": record.category,
            "sortAt": sort_at,
            "id": record.id,
            "event": record.payload,
        })
    } else {
        json!({
            "recordType": "job",
            "category": record.category,
            "sortAt": sort_at,
            "id": record.id,
            "job": record.payload,
        })
    }
}

fn dashboard_job_payload(
    job: &Job,
    label: &str,
    requested_by_label: &str,
    category: &str,
) -> Value {
    let mut payload = jobs::public_interaction_job_context(job);
    let object = payload.as_object_mut().unwrap();
    object.insert("scopeLabel".to_string(), Value::String(label.to_string()));
    object.insert("category".to_string(), Value::String(category.to_string()));
    if !requested_by_label.is_empty() {
        object.insert(
            "requestedByLabel".to_string(),
            Value::String(requested_by_label.to_string()),
        );
    }
    let error = job.string_field("error");
    if !error.is_empty() {
        object.insert("error".to_string(), Value::String(error));
    }
    payload
}

fn dashboard_event_payload(
    full: &Value,
    event_id: &str,
    scope_label: &str,
    requested_by_label: &str,
    category: &str,
) -> Value {
    let mut payload = Map::new();
    payload.insert("event_id".to_string(), Value::String(event_id.to_string()));
    for key in [
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
        "conversation_id",
        "capture_run_id",
        "segment_index",
        "duration_ms",
        "parent_job_id",
        "root_job_id",
        "source_job_id",
        "target_job_id",
        "referenced_message_id",
        "discord_message_id",
        "discord_channel_id",
        "agent_session_id",
    ] {
        if let Some(value) = full.get(key).filter(|value| !value.is_null()) {
            payload.insert(key.to_string(), value.clone());
        }
    }
    if let Some(options) = full.get("options") {
        payload.insert("options".to_string(), compact_dashboard_json(options, 3));
    }
    for key in ["result", "command_result", "command_response"] {
        if let Some(result) = full.get(key).and_then(Value::as_object) {
            let projected = ["kind", "status", "reason", "action", "message", "summary"]
                .into_iter()
                .filter_map(|field| {
                    result
                        .get(field)
                        .filter(|value| !value.is_null())
                        .map(|value| (field.to_string(), compact_dashboard_json(value, 2)))
                })
                .collect::<Map<_, _>>();
            if !projected.is_empty() {
                payload.insert(key.to_string(), Value::Object(projected));
            }
        }
    }
    payload.insert(
        "scopeLabel".to_string(),
        Value::String(scope_label.to_string()),
    );
    payload.insert("category".to_string(), Value::String(category.to_string()));
    if !requested_by_label.is_empty() {
        payload.insert(
            "requestedByLabel".to_string(),
            Value::String(requested_by_label.to_string()),
        );
    }
    Value::Object(payload)
}

fn compact_dashboard_json(value: &Value, depth: usize) -> Value {
    match value {
        Value::String(text) => Value::String(text.chars().take(1000).collect()),
        Value::Array(values) if depth > 0 => Value::Array(
            values
                .iter()
                .take(30)
                .map(|value| compact_dashboard_json(value, depth - 1))
                .collect(),
        ),
        Value::Object(object) if depth > 0 => Value::Object(
            object
                .iter()
                .take(30)
                .map(|(key, value)| (key.clone(), compact_dashboard_json(value, depth - 1)))
                .collect(),
        ),
        Value::Array(_) | Value::Object(_) => Value::Null,
        value => value.clone(),
    }
}

fn payload_scope_label(scope_kind: &str, payload: &Value) -> Option<String> {
    let keys: &[&str] = match scope_kind {
        "voice_channel" => &[
            "voice_channel_name",
            "channelName",
            "voice_channel_slug",
            "channelSlug",
            "target_room_name",
        ],
        "dm" => &[
            "display_name",
            "member_display_name",
            "global_name",
            "recipient_name",
            "target_user_name",
            "username",
        ],
        "text_channel" => &["channel_name", "channelName", "channel_slug", "channelSlug"],
        "thread" => &["thread_name", "threadName"],
        _ => return None,
    };
    find_payload_label(payload, keys, 0)
}

fn find_payload_label(payload: &Value, keys: &[&str], depth: usize) -> Option<String> {
    if depth > 6 {
        return None;
    }
    match payload {
        Value::Object(object) => {
            for key in keys {
                if let Some(label) = object
                    .get(*key)
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|label| !label.is_empty())
                {
                    return Some(label.to_string());
                }
            }
            object
                .values()
                .find_map(|value| find_payload_label(value, keys, depth + 1))
        }
        Value::Array(values) => values
            .iter()
            .find_map(|value| find_payload_label(value, keys, depth + 1)),
        _ => None,
    }
}

fn scope_label(key: &ScopeKey, candidate: Option<&String>) -> String {
    if let Some(candidate) = candidate.filter(|candidate| !candidate.trim().is_empty()) {
        return match key.kind.as_str() {
            "dm" => format!("Direct message with {}", candidate.trim()),
            _ => candidate.trim().to_string(),
        };
    }
    match key.kind.as_str() {
        "voice_channel" => "Unconfigured voice room".to_string(),
        "dm" => "Direct message".to_string(),
        "text_channel" => "Text channel".to_string(),
        "thread" => "Thread".to_string(),
        "runtime" => "Ctx".to_string(),
        value if !value.is_empty() => value.replace('_', " "),
        _ => "Unknown scope".to_string(),
    }
}

fn facet_payload(mut facets: FacetAccumulator) -> Value {
    for record_type in ["event", "job"] {
        facets
            .record_types
            .entry(record_type.to_string())
            .or_default();
    }
    let record_types = facets.record_types.into_keys().collect::<Vec<_>>();
    let categories = DASHBOARD_CATEGORIES
        .iter()
        .map(|(id, label, _)| {
            json!({
                "id": id,
                "label": label,
                "count": facets.categories.remove(*id).unwrap_or_default(),
                "kinds": facets.category_kinds.remove(*id).unwrap_or_default(),
            })
        })
        .collect::<Vec<_>>();
    let default_categories = DASHBOARD_CATEGORIES
        .iter()
        .filter(|(_, _, selected)| *selected)
        .map(|(id, _, _)| *id)
        .collect::<Vec<_>>();
    let kinds = facets.kinds.into_keys().collect::<Vec<_>>();
    let event_kinds = facets.event_kinds.into_keys().collect::<Vec<_>>();
    let job_kinds = facets.job_kinds.into_keys().collect::<Vec<_>>();
    let states = facets.states.into_keys().collect::<Vec<_>>();
    let scopes = facets
        .scopes
        .into_iter()
        .map(|(key, (count, label))| {
            json!({
                "kind": key.kind,
                "guildId": key.guild_id,
                "id": key.id,
                "label": label,
                "count": count,
            })
        })
        .collect::<Vec<_>>();
    json!({
        "recordTypes": record_types,
        "categories": categories,
        "defaultCategories": default_categories,
        "kinds": kinds,
        "eventKinds": event_kinds,
        "jobKinds": job_kinds,
        "states": states,
        "scopes": scopes,
    })
}

fn timestamp(milliseconds: i64) -> String {
    isoformat_z(Some(
        ms_to_datetime(milliseconds).expect("validated dashboard timestamp"),
    ))
}
