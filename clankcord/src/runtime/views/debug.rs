use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;

use anyhow::Context;
use chrono::{DateTime, Duration, Utc};
use serde_json::{Map, Value, json};
use sqlx::Row;

use crate::Result;
use crate::adapters::codex::{codex_usage_payload, parse_codex_jsonl};
use crate::config;
use crate::runtime::agents::{AgentSession, AgentSessionStatus};
use crate::runtime::automations::{AutomationRecord, AutomationTrigger};
use crate::runtime::jobs::AgentTaskMetadata;
use crate::runtime::timeline::{
    event_start, instant_ms_dt, isoformat_z, ms_to_datetime, parse_instant, resolve_time_reference,
    utc_now,
};
use crate::runtime::util::{first_non_empty, non_empty, preview, string_field};
use crate::runtime::{AgentRuntime, Job, JobKind, JobState, Runtime};

const AGENT_ARTIFACT_MAX_BYTES: usize = 2 * 1024 * 1024;
const AGENT_SESSION_ARTIFACT_MAX_BYTES: usize = 256 * 1024;
const AGENT_SESSION_JOB_LIMIT: usize = 100;
const DEBUG_VALUE_MAX_STRING_CHARS: usize = 4000;
const DEBUG_VALUE_MAX_ARRAY_ITEMS: usize = 100;
const HEALTH_WINDOWS: &[(&str, i64)] = &[("5m", 5 * 60), ("15m", 15 * 60), ("1h", 60 * 60)];

#[derive(Debug, Clone)]
struct DebugTimeRange {
    start: Option<DateTime<Utc>>,
    end: Option<DateTime<Utc>>,
    label: String,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum DebugSearchField {
    All,
    Detail,
    Feedback,
    Kind,
    JobKind,
    State,
    Command,
    Room,
    Actor,
}

#[derive(Debug, Clone)]
pub struct DebugOverviewRequest {
    pub jobs_limit: usize,
    pub agent_limit: usize,
    pub timeline_window: String,
    pub timeline_start: String,
    pub timeline_end: String,
    pub timeline_limit: usize,
    pub timeline_query: String,
    pub timeline_query_field: String,
    pub transcript_since: String,
    pub transcript_limit: usize,
    pub publication_limit: usize,
}

impl Default for DebugOverviewRequest {
    fn default() -> Self {
        Self {
            jobs_limit: 120,
            agent_limit: 120,
            timeline_window: "-1h".to_string(),
            timeline_start: String::new(),
            timeline_end: String::new(),
            timeline_limit: 120,
            timeline_query: String::new(),
            timeline_query_field: "all".to_string(),
            transcript_since: "-24h".to_string(),
            transcript_limit: 250,
            publication_limit: 120,
        }
    }
}

impl Runtime {
    pub async fn debug_overview(&self, request: DebugOverviewRequest) -> Result<Value> {
        let now = utc_now();
        let timeline_range = resolve_debug_time_range(
            &request.timeline_window,
            &request.timeline_start,
            &request.timeline_end,
            "-1h",
            now,
        )?;
        let transcript_since = resolve_debug_since(&request.transcript_since, "-24h", now)?;
        let timeline_query_field = parse_debug_search_field(&request.timeline_query_field)?;
        let jobs_limit = request.jobs_limit.clamp(10, 500);
        let agent_limit = request.agent_limit.clamp(10, 500);
        let timeline_limit = request.timeline_limit.clamp(10, 1000);
        let transcript_limit = request.transcript_limit.clamp(10, 5000);
        let publication_limit = request.publication_limit.clamp(10, 500);
        let mut status = self.status_payload(None).await?;
        if let Value::Object(object) = &mut status {
            object.insert(
                "liveVoiceOccupancy".to_string(),
                self.timeline_store
                    .voice_occupancy_snapshot()
                    .await
                    .context("loading live voice occupancy for debug overview")?,
            );
        }
        let active_job_records = self
            .timeline_store
            .list_jobs_by_states(
                None,
                &[
                    JobState::Queued,
                    JobState::Running,
                    JobState::Waiting,
                    JobState::CancelRequested,
                    JobState::ConfirmationPending,
                ],
            )
            .await?;
        let failed_job_records = self
            .timeline_store
            .list_jobs_by_states(
                None,
                &[
                    JobState::ApprovalFailed,
                    JobState::Failed,
                    JobState::FailedTimeout,
                    JobState::AgentDispatchFailed,
                    JobState::FailedDraftRetained,
                ],
            )
            .await?;
        let recent_job_records = self
            .timeline_store
            .list_recent_jobs(None, jobs_limit)
            .await?;
        let agent_job_records = self
            .timeline_store
            .list_jobs_by_kind(JobKind::AgentTask, agent_limit)
            .await?;
        let recent_events = self
            .recent_events(
                timeline_range.start,
                timeline_range.end,
                timeline_limit,
                &request.timeline_query,
                timeline_query_field,
            )
            .await
            .context("loading recent timeline events for debug overview")?;
        let timeline_context_job_records = self
            .timeline_context_jobs(&recent_events, jobs_limit)
            .await
            .context("loading timeline context jobs for debug overview")?;
        let timeline_querying = !request.timeline_query.trim().is_empty();
        let summary_jobs = merge_jobs(
            active_job_records
                .iter()
                .chain(failed_job_records.iter())
                .chain(recent_job_records.iter())
                .chain(agent_job_records.iter())
                .chain(timeline_context_job_records.iter()),
        );
        let recent_job_records = if timeline_querying {
            timeline_context_job_records
        } else if let Some(start) = timeline_range.start {
            self.timeline_store
                .list_jobs_updated_between(start, timeline_range.end.unwrap_or(now), jobs_limit)
                .await?
        } else {
            merge_jobs(
                recent_job_records
                    .iter()
                    .chain(timeline_context_job_records.iter()),
            )
        };
        let active_jobs = active_job_records
            .iter()
            .map(debug_job_value)
            .collect::<Vec<_>>();
        let recent_jobs = recent_job_records
            .iter()
            .map(debug_job_value)
            .collect::<Vec<_>>();
        let transcript_events = self
            .recent_transcript_events(transcript_since, transcript_limit)
            .await
            .context("loading recent transcript events for debug overview")?;
        let event_kind_counts = event_kind_counts(&recent_events);
        let summary = job_summary(&summary_jobs);
        let database = database_diagnostics(self).await;
        let health = runtime_health(self, &summary_jobs, &database);
        let operations = operational_diagnostics(self, now)
            .await
            .context("loading operational health diagnostics")?;
        let publications = self
            .timeline_store
            .list_publications(None, None, None)
            .await
            .context("loading publications for debug overview")?
            .into_iter()
            .take(publication_limit)
            .collect::<Vec<_>>();
        let automations = self
            .timeline_store
            .list_automations(None, None, None)
            .await
            .context("loading automations for debug overview")?;
        Ok(json!({
            "generatedAt": isoformat_z(Some(now)),
            "process": {
                "startedAt": isoformat_z(Some(self.started_at)),
                "uptimeSeconds": (now - self.started_at).num_seconds(),
                "autoJoin": {"enabled": self.auto_join_enabled},
                "load": process_load_payload(),
            },
            "health": health,
            "database": database,
            "load": load_payload(&active_job_records, now),
            "operations": operations,
            "agents": agent_dashboard_payload(&agent_job_records, agent_limit),
            "status": status,
            "jobs": {
                "summary": summary,
                "active": active_jobs,
                "recent": recent_jobs,
            },
            "timeline": {
                "window": timeline_range.label,
                "start": debug_time_label(timeline_range.start),
                "end": debug_time_label(timeline_range.end),
                "recentEvents": recent_events,
                "eventKindCounts": event_kind_counts,
            },
            "transcript": {
                "since": debug_since_label(transcript_since),
                "events": transcript_events,
            },
            "automations": automation_dashboard_payload(&automations),
            "publications": publications,
            "links": {
                "json": "/v1/voice/debug/overview",
                "poolStatus": "/v1/voice/pool/status",
                "timelineTail": "/v1/voice/timeline/tail",
                "jobs": "/v1/voice/jobs",
            }
        }))
    }

    async fn recent_events(
        &self,
        start: Option<DateTime<Utc>>,
        end: Option<DateTime<Utc>>,
        limit: usize,
        query: &str,
        query_field: DebugSearchField,
    ) -> Result<Vec<Value>> {
        self.recent_events_by_kind(start, end, limit, None, query, query_field)
            .await
    }

    pub async fn recent_transcript_events(
        &self,
        since: Option<DateTime<Utc>>,
        limit: usize,
    ) -> Result<Vec<Value>> {
        let kinds = BTreeSet::from(["speech_segment".to_string(), "transcript".to_string()]);
        self.recent_events_by_kind(since, None, limit, Some(&kinds), "", DebugSearchField::All)
            .await
    }

    async fn recent_events_by_kind(
        &self,
        start: Option<DateTime<Utc>>,
        end: Option<DateTime<Utc>>,
        limit: usize,
        kinds: Option<&BTreeSet<String>>,
        query: &str,
        query_field: DebugSearchField,
    ) -> Result<Vec<Value>> {
        let mut events = Vec::new();
        for (guild_id, channel_id) in self.debug_timeline_event_channels(start, end).await? {
            let mut room_events = self
                .timeline_store
                .load_events(&guild_id, &channel_id, start, end, kinds, None, false)
                .await?;
            events.append(&mut room_events);
        }
        events.sort_by_key(|event| event_start(event).unwrap_or_else(utc_now));
        let query = query.trim();
        if !query.is_empty() {
            let matched_indexes = events
                .iter()
                .enumerate()
                .filter_map(|(index, event)| {
                    debug_event_matches_query(event, query, query_field).then_some(index)
                })
                .collect::<Vec<_>>();
            let mut selected_indexes = BTreeSet::new();
            let context_each_side = limit.saturating_sub(1).min(80) / 2;
            for index in matched_indexes.into_iter().rev() {
                let start = index.saturating_sub(context_each_side);
                let end = (index + context_each_side).min(events.len().saturating_sub(1));
                for selected in start..=end {
                    selected_indexes.insert(selected);
                }
                if selected_indexes.len() >= limit {
                    break;
                }
            }
            events = selected_indexes
                .into_iter()
                .filter_map(|index| events.get(index).cloned())
                .collect();
        }
        events.reverse();
        events.truncate(limit);
        Ok(events.into_iter().map(compact_debug_event).collect())
    }

    async fn debug_timeline_event_channels(
        &self,
        start: Option<DateTime<Utc>>,
        end: Option<DateTime<Utc>>,
    ) -> Result<Vec<(String, String)>> {
        let mut query = sqlx::QueryBuilder::<sqlx::Postgres>::new(
            r#"
            SELECT DISTINCT guild_id, voice_channel_id
            FROM timeline_events
            WHERE forgotten = FALSE
            "#,
        );
        if let Some(start) = start {
            query
                .push(" AND COALESCE(ended_at_ms, started_at_ms, created_at_ms) > ")
                .push_bind(instant_ms_dt(start));
        }
        if let Some(end) = end {
            query
                .push(" AND COALESCE(started_at_ms, created_at_ms) < ")
                .push_bind(instant_ms_dt(end));
        }
        query.push(" ORDER BY guild_id, voice_channel_id");
        let rows = query.build().fetch_all(&self.timeline_store.pool).await?;
        rows.into_iter()
            .map(|row| Ok((row.try_get("guild_id")?, row.try_get("voice_channel_id")?)))
            .collect()
    }

    async fn timeline_context_jobs(&self, events: &[Value], limit: usize) -> Result<Vec<Job>> {
        let Some(first) = events.iter().filter_map(event_start).min() else {
            return Ok(Vec::new());
        };
        let Some(last) = events.iter().filter_map(event_start).max() else {
            return Ok(Vec::new());
        };
        self.timeline_store
            .list_jobs_updated_between(
                first - Duration::minutes(10),
                last + Duration::minutes(10),
                limit,
            )
            .await
    }

    pub async fn debug_agent_job(&self, job_id: &str) -> Result<Value> {
        let job = self.timeline_store.get_job(job_id).await?;
        if job.kind != JobKind::AgentTask {
            anyhow::bail!("job {job_id} is not an agent task");
        }
        agent_job_payload(self, &job).await
    }
}

fn debug_event_matches_query(event: &Value, query: &str, field: DebugSearchField) -> bool {
    let haystack = debug_event_search_values(event, field)
        .join(" ")
        .to_lowercase();
    query
        .split_whitespace()
        .map(debug_search_term)
        .all(|term| haystack.contains(&term))
}

fn debug_event_search_values(event: &Value, field: DebugSearchField) -> Vec<String> {
    match field {
        DebugSearchField::All => [
            DebugSearchField::Detail,
            DebugSearchField::Feedback,
            DebugSearchField::Kind,
            DebugSearchField::JobKind,
            DebugSearchField::State,
            DebugSearchField::Command,
            DebugSearchField::Room,
            DebugSearchField::Actor,
        ]
        .into_iter()
        .flat_map(|field| debug_event_search_values(event, field))
        .collect(),
        DebugSearchField::Detail => {
            debug_non_empty_fields(event, &["text", "feedback_message", "reason", "quality"])
                .into_iter()
                .chain(debug_result_search_values(event))
                .collect()
        }
        DebugSearchField::Feedback => debug_non_empty_fields(
            event,
            &["kind", "event_kind", "feedback_message", "text", "reason"],
        )
        .into_iter()
        .filter(|value| {
            debug_non_empty_fields(event, &["kind", "event_kind"])
                .iter()
                .any(|kind| kind == "feedback")
                || value == "feedback"
        })
        .collect(),
        DebugSearchField::Kind => debug_non_empty_fields(event, &["kind", "event_kind"]),
        DebugSearchField::JobKind => debug_non_empty_fields(event, &["job_kind"]),
        DebugSearchField::State => debug_non_empty_fields(event, &["state"]),
        DebugSearchField::Command => debug_non_empty_fields(event, &["command_kind"]),
        DebugSearchField::Room => debug_non_empty_fields(
            event,
            &["guild_slug", "voice_channel_name", "voice_channel_slug"],
        ),
        DebugSearchField::Actor => {
            debug_non_empty_fields(event, &["speaker_label", "speaker_username"])
        }
    }
}

fn debug_non_empty_fields(event: &Value, fields: &[&str]) -> Vec<String> {
    fields
        .iter()
        .map(|field| string_field(event, field))
        .filter(|value| !value.trim().is_empty())
        .collect()
}

fn debug_result_search_values(event: &Value) -> Vec<String> {
    let mut values = Vec::new();
    for result_key in ["result", "command_result", "command_response"] {
        let Some(result) = event.get(result_key) else {
            continue;
        };
        for field in ["kind", "status", "reason", "action", "message", "summary"] {
            let value = string_field(result, field);
            if !value.trim().is_empty() {
                values.push(value);
            }
        }
    }
    values
}

fn parse_debug_search_field(raw: &str) -> Result<DebugSearchField> {
    let field = non_empty(raw.trim().to_lowercase(), "all".to_string());
    match field.as_str() {
        "all" => Ok(DebugSearchField::All),
        "detail" => Ok(DebugSearchField::Detail),
        "feedback" => Ok(DebugSearchField::Feedback),
        "kind" => Ok(DebugSearchField::Kind),
        "job_kind" => Ok(DebugSearchField::JobKind),
        "state" => Ok(DebugSearchField::State),
        "command" => Ok(DebugSearchField::Command),
        "room" => Ok(DebugSearchField::Room),
        "actor" => Ok(DebugSearchField::Actor),
        _ => anyhow::bail!("invalid dashboard timeline search field: {field}"),
    }
}

fn debug_search_term(term: &str) -> String {
    term.trim_start_matches('/').to_lowercase()
}

fn resolve_debug_time_range(
    raw_window: &str,
    raw_start: &str,
    raw_end: &str,
    default_window: &str,
    now: DateTime<Utc>,
) -> Result<DebugTimeRange> {
    let window = non_empty(raw_window.trim().to_string(), default_window.to_string());
    if window.eq_ignore_ascii_case("all") {
        return Ok(DebugTimeRange {
            start: None,
            end: None,
            label: "all".to_string(),
        });
    }
    if window.eq_ignore_ascii_case("custom") {
        let start = resolve_debug_bound(raw_start, "timeline start")?;
        let end = resolve_debug_bound(raw_end, "timeline end")?;
        if let (Some(start), Some(end)) = (start, end) {
            if end < start {
                anyhow::bail!("timeline end must be after timeline start");
            }
        }
        return Ok(DebugTimeRange {
            start,
            end,
            label: "custom".to_string(),
        });
    }
    let start = resolve_time_reference(&window, Some(now))
        .ok_or_else(|| anyhow::anyhow!("invalid dashboard timeline window: {window}"))?;
    Ok(DebugTimeRange {
        start: Some(start),
        end: None,
        label: window,
    })
}

fn resolve_debug_bound(raw: &str, label: &str) -> Result<Option<DateTime<Utc>>> {
    let value = raw.trim();
    if value.is_empty() {
        return Ok(None);
    }
    resolve_time_reference(value, None)
        .map(Some)
        .ok_or_else(|| anyhow::anyhow!("invalid dashboard {label}: {value}"))
}

fn resolve_debug_since(
    raw: &str,
    default: &str,
    now: DateTime<Utc>,
) -> Result<Option<DateTime<Utc>>> {
    let value = non_empty(raw.trim().to_string(), default.to_string());
    if value.eq_ignore_ascii_case("all") {
        return Ok(None);
    }
    resolve_time_reference(&value, Some(now))
        .map(Some)
        .ok_or_else(|| anyhow::anyhow!("invalid dashboard time window: {value}"))
}

fn debug_since_label(since: Option<DateTime<Utc>>) -> String {
    since
        .map(|since| isoformat_z(Some(since)))
        .unwrap_or_else(|| "all".to_string())
}

fn debug_time_label(time: Option<DateTime<Utc>>) -> String {
    time.map(|time| isoformat_z(Some(time)))
        .unwrap_or_else(|| "open".to_string())
}

fn debug_job_value(job: &Job) -> Value {
    let mut value = compact_debug_value(job.to_value(), 5);
    if let Value::Object(object) = &mut value {
        let command_kind = job.command_kind();
        if !command_kind.trim().is_empty() {
            object.insert("command_kind".to_string(), json!(command_kind));
        }
    }
    value
}

fn compact_debug_event(event: Value) -> Value {
    let compact = compact_debug_value(event, 4);
    if let Value::Object(object) = &compact {
        let mut result = Map::new();
        for key in [
            "event_id",
            "event_kind",
            "kind",
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
                result.insert(key.to_string(), compact_debug_value(value.clone(), 2));
            }
        }
        return Value::Object(result);
    }
    compact
}

fn compact_debug_value(value: Value, depth: usize) -> Value {
    match value {
        Value::Object(object) => {
            if depth == 0 {
                return json!({"truncated": true, "fields": object.len()});
            }
            let mut compact = Map::new();
            for (key, value) in object {
                if omit_debug_key(&key) {
                    continue;
                }
                compact.insert(key, compact_debug_value(value, depth - 1));
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
                .take(DEBUG_VALUE_MAX_ARRAY_ITEMS)
                .map(|value| compact_debug_value(value, depth - 1))
                .collect::<Vec<_>>();
            if original_len > compact.len() {
                compact.push(json!({"truncated": true, "remaining": original_len - compact.len()}));
            }
            Value::Array(compact)
        }
        Value::String(value) => Value::String(truncate_debug_string(value)),
        value => value,
    }
}

fn omit_debug_key(key: &str) -> bool {
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

fn truncate_debug_string(value: String) -> String {
    if value.chars().count() <= DEBUG_VALUE_MAX_STRING_CHARS {
        return value;
    }
    value
        .chars()
        .take(DEBUG_VALUE_MAX_STRING_CHARS)
        .collect::<String>()
        + "...[truncated]"
}

fn merge_jobs<'a>(jobs: impl Iterator<Item = &'a Job>) -> Vec<Job> {
    let mut merged = BTreeMap::new();
    for job in jobs {
        merged.entry(job.id.clone()).or_insert_with(|| job.clone());
    }
    let mut jobs = merged.into_values().collect::<Vec<_>>();
    jobs.sort_by(|left, right| {
        first_non_empty([right.updated_at.clone(), right.created_at.clone()])
            .cmp(&first_non_empty([
                left.updated_at.clone(),
                left.created_at.clone(),
            ]))
            .then_with(|| right.id.cmp(&left.id))
    });
    jobs
}

#[derive(Debug, Default)]
struct RoomJobSummary {
    guild_id: String,
    voice_channel_id: String,
    total: usize,
    active: usize,
    failed: usize,
    latest_at: String,
}

#[derive(Debug, Default)]
struct EventKindSummary {
    event_kind: String,
    count: usize,
    latest_at: String,
}

#[derive(Debug, Clone)]
struct JobDiagnosticRow {
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

fn runtime_health(runtime: &Runtime, jobs: &[Job], database: &Value) -> Value {
    let database_ok = database.get("ok").and_then(Value::as_bool).unwrap_or(false);
    let failed_jobs = jobs
        .iter()
        .filter(|job| is_failed_state(job.state.as_str()))
        .count();
    let active_agent_jobs = jobs
        .iter()
        .filter(|job| job.kind == JobKind::AgentTask && !job.state.is_terminal())
        .count();
    json!({
        "ok": database_ok,
        "postgres": database_ok,
        "configuredBots": runtime.bots.len(),
        "readyBots": runtime.bots.values().filter(|bot| bot.ready).count(),
        "activeSessions": runtime.sessions.len(),
        "configuredRooms": runtime.rooms.len(),
        "activeAgentJobs": active_agent_jobs,
        "failedJobs": failed_jobs,
        "automationsLoaded": runtime.automations.len(),
    })
}

async fn database_diagnostics(runtime: &Runtime) -> Value {
    if let Err(error) = sqlx::query("SELECT 1")
        .execute(&runtime.timeline_store.pool)
        .await
    {
        return json!({
            "ok": false,
            "url": runtime.timeline_store.database_url,
            "root": runtime.timeline_store.root.display().to_string(),
            "error": error.to_string(),
            "tables": [],
        });
    }
    let row = sqlx::query(
        "SELECT current_database() AS database_name, current_user AS user_name, version() AS version",
    )
    .fetch_one(&runtime.timeline_store.pool)
    .await
    .ok();
    let table_rows = table_counts(runtime).await;
    json!({
        "ok": true,
        "url": runtime.timeline_store.database_url,
        "root": runtime.timeline_store.root.display().to_string(),
        "database": row.as_ref().and_then(|row| row.try_get::<String, _>("database_name").ok()).unwrap_or_default(),
        "user": row.as_ref().and_then(|row| row.try_get::<String, _>("user_name").ok()).unwrap_or_default(),
        "version": row.as_ref().and_then(|row| row.try_get::<String, _>("version").ok()).unwrap_or_default(),
        "tables": table_rows,
    })
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
        "timeline_events",
        "conversations",
        "windows",
        "publications",
        "authoritative_spans",
        "jobs",
        "job_payloads",
        "job_dependencies",
        "automations",
    ]
}

async fn table_counts(runtime: &Runtime) -> Vec<Value> {
    let mut rows = Vec::new();
    for table in observed_tables() {
        let count = sqlx::query_scalar::<_, i64>(&format!("SELECT COUNT(*) FROM {table}"))
            .fetch_one(&runtime.timeline_store.pool)
            .await
            .unwrap_or(0);
        rows.push(json!({"table": table, "rows": count}));
    }
    rows
}

async fn operational_diagnostics(runtime: &Runtime, now: DateTime<Utc>) -> Result<Value> {
    let since_ms = instant_ms_dt(now - chrono::Duration::seconds(max_health_window_seconds()));
    let job_rows = diagnostic_job_rows(runtime, since_ms).await?;
    let event_rows = diagnostic_event_rows(runtime, since_ms).await?;
    Ok(json!({
        "backlog": backlog_payload(&job_rows, now),
        "windows": operational_windows(&job_rows, &event_rows, now),
        "latencies": latency_payload(&job_rows, now),
    }))
}

async fn diagnostic_job_rows(runtime: &Runtime, since_ms: i64) -> Result<Vec<JobDiagnosticRow>> {
    let rows = sqlx::query(
        r#"
        SELECT kind, state, lane, created_at_ms, updated_at_ms, ready_at_ms,
               started_at_ms, completed_at_ms, terminal, failed, cancellable
        FROM jobs
        WHERE terminal = FALSE
           OR created_at_ms >= $1
           OR updated_at_ms >= $1
           OR completed_at_ms >= $1
        ORDER BY updated_at_ms DESC
        "#,
    )
    .bind(since_ms)
    .fetch_all(&runtime.timeline_store.pool)
    .await?;

    rows.into_iter()
        .map(|row| {
            Ok(JobDiagnosticRow {
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

async fn diagnostic_event_rows(
    runtime: &Runtime,
    since_ms: i64,
) -> Result<Vec<EventDiagnosticRow>> {
    let rows = sqlx::query(
        r#"
        SELECT event_kind, COALESCE(started_at_ms, created_at_ms) AS at_ms,
               ended_at_ms, speaker_user_id
        FROM timeline_events
        WHERE forgotten = FALSE
          AND COALESCE(started_at_ms, created_at_ms) >= $1
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
        ORDER BY COALESCE(started_at_ms, created_at_ms) DESC
        "#,
    )
    .bind(since_ms)
    .fetch_all(&runtime.timeline_store.pool)
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

fn latency_payload(rows: &[JobDiagnosticRow], now: DateTime<Utc>) -> Value {
    let now_ms = instant_ms_dt(now);
    let windows = HEALTH_WINDOWS
        .iter()
        .map(|(label, seconds)| {
            let since_ms = now_ms - (seconds * 1000);
            json!({
                "label": *label,
                "since": ms_iso(since_ms),
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
            row.completed_at_ms
                .is_some_and(|completed| completed >= since_ms)
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
    let mut count = 0_usize;
    let mut failed = 0_usize;
    let mut latest_ms = None::<i64>;

    for row in rows.iter().filter(|row| {
        kind.is_none_or(|kind| row.kind == kind)
            && row
                .completed_at_ms
                .is_some_and(|completed_at_ms| completed_at_ms >= since_ms)
    }) {
        let completed_at_ms = row
            .completed_at_ms
            .expect("latency row has completed_at_ms");
        count += 1;
        if row.is_failed() {
            failed += 1;
        }
        ready_delay_ms.push((row.ready_at_ms - row.created_at_ms).max(0));
        total_ms.push((completed_at_ms - row.ready_at_ms).max(0));
        if let Some(started_at_ms) = row.started_at_ms {
            queue_ms.push((started_at_ms - row.ready_at_ms).max(0));
            run_ms.push((completed_at_ms - started_at_ms).max(0));
        }
        latest_ms = Some(latest_ms.map_or(completed_at_ms, |latest| latest.max(completed_at_ms)));
    }

    json!({
        "count": count,
        "failed": failed,
        "readyDelayMs": latency_metric(ready_delay_ms),
        "queueMs": latency_metric(queue_ms),
        "runMs": latency_metric(run_ms),
        "totalMs": latency_metric(total_ms),
        "latestAt": latest_ms.map(ms_iso).unwrap_or_default(),
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

fn process_load_payload() -> Value {
    let status = proc_status_fields();
    let meminfo = proc_meminfo_fields();
    json!({
        "pid": std::process::id(),
        "threads": status.get("Threads").copied(),
        "openFileDescriptors": open_file_descriptor_count(),
        "loadAverage": proc_load_average_payload(),
        "memory": {
            "rssBytes": status.get("VmRSS").copied(),
            "vmSizeBytes": status.get("VmSize").copied(),
            "vmPeakBytes": status.get("VmPeak").copied(),
            "hostTotalBytes": meminfo.get("MemTotal").copied(),
            "hostAvailableBytes": meminfo.get("MemAvailable").copied(),
            "cgroupCurrentBytes": read_u64_file("/sys/fs/cgroup/memory.current"),
            "cgroupMaxBytes": cgroup_memory_max(),
        },
        "cpu": {
            "process": proc_self_stat_payload(),
            "cgroup": cgroup_cpu_stat_payload(),
        },
    })
}

fn proc_status_fields() -> BTreeMap<String, u64> {
    let mut fields = BTreeMap::new();
    let Ok(content) = fs::read_to_string("/proc/self/status") else {
        return fields;
    };
    for line in content.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        if key == "Threads" {
            if let Some(value) = value.split_whitespace().next().and_then(parse_u64) {
                fields.insert(key.to_string(), value);
            }
            continue;
        }
        if key.starts_with("Vm") {
            if let Some(bytes) = parse_kb_value(value) {
                fields.insert(key.to_string(), bytes);
            }
        }
    }
    fields
}

fn proc_meminfo_fields() -> BTreeMap<String, u64> {
    let mut fields = BTreeMap::new();
    let Ok(content) = fs::read_to_string("/proc/meminfo") else {
        return fields;
    };
    for line in content.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        if matches!(key, "MemTotal" | "MemAvailable") {
            if let Some(bytes) = parse_kb_value(value) {
                fields.insert(key.to_string(), bytes);
            }
        }
    }
    fields
}

fn proc_load_average_payload() -> Value {
    let Ok(content) = fs::read_to_string("/proc/loadavg") else {
        return json!({});
    };
    let parts = content.split_whitespace().collect::<Vec<_>>();
    let (runnable, total_threads) = parts
        .get(3)
        .and_then(|value| value.split_once('/'))
        .map(|(running, total)| (parse_u64(running), parse_u64(total)))
        .unwrap_or((None, None));
    json!({
        "oneMinute": parts.first().and_then(|value| value.parse::<f64>().ok()),
        "fiveMinute": parts.get(1).and_then(|value| value.parse::<f64>().ok()),
        "fifteenMinute": parts.get(2).and_then(|value| value.parse::<f64>().ok()),
        "runnableThreads": runnable,
        "totalThreads": total_threads,
        "lastPid": parts.get(4).and_then(|value| parse_u64(value)),
    })
}

fn proc_self_stat_payload() -> Value {
    let Ok(content) = fs::read_to_string("/proc/self/stat") else {
        return json!({});
    };
    let Some(close_comm) = content.rfind(')') else {
        return json!({});
    };
    let fields = content[close_comm + 1..]
        .split_whitespace()
        .collect::<Vec<_>>();
    let user_ticks = fields.get(11).and_then(|value| parse_u64(value));
    let system_ticks = fields.get(12).and_then(|value| parse_u64(value));
    json!({
        "userTicks": user_ticks,
        "systemTicks": system_ticks,
        "totalTicks": user_ticks.zip(system_ticks).map(|(user, system)| user + system),
        "startTimeTicks": fields.get(19).and_then(|value| parse_u64(value)),
    })
}

fn cgroup_cpu_stat_payload() -> Value {
    let Ok(content) = fs::read_to_string("/sys/fs/cgroup/cpu.stat") else {
        return json!({});
    };
    let mut object = Map::new();
    for line in content.lines() {
        let mut parts = line.split_whitespace();
        let Some(key) = parts.next() else {
            continue;
        };
        let Some(value) = parts.next().and_then(parse_u64) else {
            continue;
        };
        object.insert(key.to_string(), json!(value));
    }
    Value::Object(object)
}

fn cgroup_memory_max() -> Value {
    let Ok(raw) = fs::read_to_string("/sys/fs/cgroup/memory.max") else {
        return Value::Null;
    };
    let value = raw.trim();
    if value == "max" {
        Value::Null
    } else {
        parse_u64(value).map_or(Value::Null, |value| json!(value))
    }
}

fn read_u64_file(path: &str) -> Option<u64> {
    fs::read_to_string(path)
        .ok()
        .and_then(|content| parse_u64(content.trim()))
}

fn open_file_descriptor_count() -> Option<usize> {
    fs::read_dir("/proc/self/fd")
        .ok()
        .map(|entries| entries.filter_map(std::result::Result::ok).count())
}

fn parse_kb_value(value: &str) -> Option<u64> {
    value
        .split_whitespace()
        .next()
        .and_then(parse_u64)
        .map(|kb| kb.saturating_mul(1024))
}

fn parse_u64(value: &str) -> Option<u64> {
    value.parse::<u64>().ok()
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

fn agent_dashboard_payload(jobs: &[Job], limit: usize) -> Value {
    let agent_jobs = jobs
        .iter()
        .filter(|job| job.kind == JobKind::AgentTask)
        .take(limit)
        .map(compact_agent_job_payload)
        .collect::<Vec<_>>();
    let sessions = agent_sessions_from_jobs(jobs)
        .into_iter()
        .map(|session| session.to_json())
        .collect::<Vec<_>>();
    json!({
        "sessions": sessions,
        "jobs": agent_jobs,
        "summary": agent_summary(jobs),
        "codex": {
            "auth": codex_auth_payload(),
            "usage": codex_usage_rollup(jobs, utc_now()),
        },
    })
}

fn automation_dashboard_payload(records: &[AutomationRecord]) -> Value {
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

fn codex_auth_payload() -> Value {
    let home = codex_home();
    let auth_path = home.join("auth.json");
    let version_path = home.join("version.json");
    let auth = fs::read_to_string(&auth_path)
        .ok()
        .and_then(|content| serde_json::from_str::<Value>(&content).ok())
        .unwrap_or_else(|| json!({}));
    let tokens = auth.get("tokens").cloned().unwrap_or_else(|| json!({}));
    let now = utc_now();
    let id_claims = jwt_payload(&string_field(&tokens, "id_token"));
    let access_claims = jwt_payload(&string_field(&tokens, "access_token"));
    let expiry = jwt_expiry_payload(&access_claims, now);
    let id_expiry = jwt_expiry_payload(&id_claims, now);
    let version = fs::read_to_string(&version_path)
        .ok()
        .and_then(|content| serde_json::from_str::<Value>(&content).ok())
        .unwrap_or_else(|| json!({}));

    json!({
        "home": home.display().to_string(),
        "authPath": auth_path.display().to_string(),
        "authPresent": auth.is_object() && !auth.as_object().is_none_or(Map::is_empty),
        "loginType": if !string_field(&tokens, "access_token").is_empty() { "chatgpt_oauth" } else if auth.get("OPENAI_API_KEY").and_then(Value::as_str).is_some_and(|value| !value.trim().is_empty()) { "api_key" } else { "unknown" },
        "apiKeyPresent": auth.get("OPENAI_API_KEY").and_then(Value::as_str).is_some_and(|value| !value.trim().is_empty()),
        "account": {
            "accountId": string_field(&tokens, "account_id"),
            "subject": string_field(&id_claims, "sub"),
            "email": string_field(&id_claims, "email"),
            "name": string_field(&id_claims, "name"),
            "organizationId": first_non_empty([
                string_field(&id_claims, "org_id"),
                string_field(&id_claims, "orgId"),
                string_field(&id_claims, "organization_id"),
            ]),
        },
        "lastRefresh": string_field(&auth, "last_refresh"),
        "accessToken": expiry,
        "idToken": id_expiry,
        "version": {
            "latest": string_field(&version, "latest_version"),
            "lastCheckedAt": string_field(&version, "last_checked_at"),
        },
    })
}

fn codex_home() -> PathBuf {
    config::codex_home()
}

fn jwt_payload(token: &str) -> Value {
    let Some(payload) = token.split('.').nth(1) else {
        return json!({});
    };
    decode_base64_url(payload)
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .unwrap_or_else(|| json!({}))
}

fn jwt_expiry_payload(claims: &Value, now: DateTime<Utc>) -> Value {
    let expires_at = claims
        .get("exp")
        .and_then(Value::as_i64)
        .and_then(|seconds| DateTime::<Utc>::from_timestamp(seconds, 0));
    json!({
        "expiresAt": isoformat_z(expires_at),
        "expiresInSeconds": expires_at.map(|expires_at| (expires_at - now).num_seconds()).unwrap_or(0),
        "expired": expires_at.is_some_and(|expires_at| expires_at <= now),
    })
}

fn decode_base64_url(input: &str) -> Option<Vec<u8>> {
    let mut output = Vec::new();
    let mut buffer = 0_u32;
    let mut bits = 0_u8;
    for byte in input.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'-' => 62,
            b'_' => 63,
            b'=' => break,
            _ => return None,
        } as u32;
        buffer = (buffer << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            output.push(((buffer >> bits) & 0xff) as u8);
        }
    }
    Some(output)
}

fn agent_summary(jobs: &[Job]) -> Value {
    let mut total = 0_usize;
    let mut active = 0_usize;
    let mut failed = 0_usize;
    let mut completed = 0_usize;
    for job in jobs.iter().filter(|job| job.kind == JobKind::AgentTask) {
        total += 1;
        if !job.state.is_terminal() {
            active += 1;
        }
        if is_failed_state(job.state.as_str()) {
            failed += 1;
        }
        if job.state == JobState::Complete {
            completed += 1;
        }
    }
    json!({
        "total": total,
        "active": active,
        "failed": failed,
        "completed": completed,
    })
}

fn agent_sessions_from_jobs(jobs: &[Job]) -> Vec<AgentSession> {
    let mut ordered = jobs
        .iter()
        .filter(|job| job.kind == JobKind::AgentTask)
        .collect::<Vec<_>>();
    ordered.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then_with(|| left.id.cmp(&right.id))
    });
    let mut sessions = BTreeMap::<String, AgentSession>::new();
    for job in ordered {
        let key = AgentRuntime::task_session_key(&job.guild_id, &job.voice_channel_id);
        let entry = sessions.entry(key.clone()).or_insert_with(|| AgentSession {
            key,
            role: "task".to_string(),
            guild_id: job.guild_id.clone(),
            voice_channel_id: job.voice_channel_id.clone(),
            created_at: job.created_at.clone(),
            ..AgentSession::default()
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

async fn agent_job_payload(runtime: &Runtime, job: &Job) -> Result<Value> {
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
    runtime: &Runtime,
    selected: &Job,
    selected_codex: &Value,
) -> Result<Value> {
    let key = AgentRuntime::task_session_key(&selected.guild_id, &selected.voice_channel_id);
    let mut jobs = runtime
        .timeline_store
        .list_jobs_by_scope_kind(
            &selected.guild_id,
            &selected.voice_channel_id,
            JobKind::AgentTask,
        )
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
    let channel_job_count = jobs.len();
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
    let transcript = agent_session_transcript(&rows);
    let mut timeline = Vec::new();
    for row in &rows {
        let job_id = string_field(row, "job_id");
        let events = row
            .get("codex")
            .and_then(|codex| codex.get("timeline"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for mut event in events {
            if let Value::Object(object) = &mut event {
                object.insert("job_id".to_string(), json!(job_id));
                object.insert(
                    "selected".to_string(),
                    json!(job_id == selected.id.as_str()),
                );
            }
            timeline.push(event);
        }
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
        "channelJobCount": channel_job_count,
        "truncated": truncated,
        "jobs": rows,
        "transcript": {
            "content": transcript,
            "bytes": transcript.len(),
        },
        "codex": {
            "timeline": timeline,
        },
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
    let metadata = job.metadata.agent_task().cloned().unwrap_or_default();
    let prompt = read_text_artifact(&metadata.prompt_path, AGENT_SESSION_ARTIFACT_MAX_BYTES);
    let result = read_text_artifact(&metadata.result_path, AGENT_SESSION_ARTIFACT_MAX_BYTES);
    let raw = read_text_artifact(&metadata.raw_result_path, AGENT_SESSION_ARTIFACT_MAX_BYTES);
    let codex = parse_codex_trace(raw.get("content").and_then(Value::as_str).unwrap_or(""));
    json!({
        "job_id": job.id.clone(),
        "state": job.state.as_str(),
        "created_at": job.created_at.clone(),
        "updated_at": job.updated_at.clone(),
        "request": job.command().map(|command| command.arguments.request_text()).unwrap_or_default(),
        "session_id": metadata.agent.session_id,
        "model": metadata.agent.model,
        "prompt": prompt,
        "result": result,
        "raw": raw,
        "codex": codex,
    })
}

fn agent_session_transcript(rows: &[Value]) -> String {
    let mut parts = Vec::new();
    for row in rows {
        let job_id = string_field(row, "job_id");
        let state = string_field(row, "state");
        let created_at = string_field(row, "created_at");
        let request = string_field(row, "request");
        parts.push(format!("JOB {job_id} [{state}] {created_at}"));
        if !request.trim().is_empty() {
            parts.push(format!("REQUEST:\n{request}"));
        }
        let prompt = row
            .get("prompt")
            .and_then(|artifact| artifact.get("content"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if !prompt.is_empty() {
            parts.push(format!("PROMPT:\n{prompt}"));
        }
        let result = row
            .get("result")
            .and_then(|artifact| artifact.get("content"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if !result.is_empty() {
            parts.push(format!("RESULT:\n{result}"));
        }
        let messages = row
            .get("codex")
            .and_then(|codex| codex.get("messages"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let message_text = messages
            .iter()
            .filter_map(|message| {
                let role = string_field(message, "role");
                let text = string_field(message, "text");
                (!text.trim().is_empty()).then(|| format!("{role}: {text}"))
            })
            .collect::<Vec<_>>()
            .join("\n");
        if !message_text.is_empty() {
            parts.push(format!("VISIBLE MESSAGES:\n{message_text}"));
        }
        parts.push(String::new());
    }
    parts.join("\n\n").trim().to_string()
}

fn compact_agent_job_payload(job: &Job) -> Value {
    let metadata = job.metadata.agent_task().cloned().unwrap_or_default();
    let codex = compact_agent_codex_summary(&metadata);
    let mut job_value = Runtime::public_interaction_job_context(job);
    if let Value::Object(object) = &mut job_value {
        let metadata = job.metadata.to_json();
        if metadata
            .as_object()
            .is_some_and(|object| !object.is_empty())
        {
            object.insert("metadata".to_string(), metadata);
        }
    }
    json!({
        "job": job_value,
        "paths": {
            "workdir": metadata.workdir_path,
            "prompt": metadata.prompt_path,
            "result": metadata.result_path,
            "raw": metadata.raw_result_path,
        },
        "workdir": workspace_artifact(&metadata.workdir_path),
        "prompt": artifact_stub(&metadata.prompt_path),
        "result": artifact_stub(&metadata.result_path),
        "raw": artifact_stub(&metadata.raw_result_path),
        "codex": codex,
        "detailUrl": format!("/v1/voice/debug/agents/{}", job.id),
    })
}

fn compact_agent_codex_summary(metadata: &AgentTaskMetadata) -> Value {
    let needs_raw = metadata.agent.session_id.trim().is_empty()
        || metadata.agent.model.trim().is_empty()
        || metadata.agent.usage.is_empty();
    let raw_trace = if needs_raw && !metadata.raw_result_path.trim().is_empty() {
        let raw = read_text_artifact(&metadata.raw_result_path, AGENT_ARTIFACT_MAX_BYTES);
        parse_codex_trace(raw.get("content").and_then(Value::as_str).unwrap_or(""))
    } else {
        json!({})
    };
    let usage = if metadata.agent.usage.is_empty() {
        raw_trace
            .get("tokenUsage")
            .cloned()
            .unwrap_or_else(|| json!({}))
    } else {
        usage_payload_info(&metadata.agent.usage.to_json())
    };
    json!({
        "sessionId": first_non_empty([
            metadata.agent.session_id.clone(),
            string_field(&raw_trace, "sessionId"),
        ]),
        "model": first_non_empty([
            metadata.agent.model.clone(),
            string_field(&raw_trace, "model"),
        ]),
        "tokenUsage": usage,
        "contextUsedTokens": token_usage_input_tokens(&usage),
        "modelContextWindow": context_window_from_usage(&usage).unwrap_or(0),
        "contextUsedPercent": context_used_percent(&usage),
        "eventCount": raw_trace.get("eventCount").and_then(Value::as_u64).unwrap_or(0),
        "timeline": [],
        "messages": [],
        "toolCalls": [],
    })
}

fn context_used_percent(usage: &Value) -> f64 {
    let input = token_usage_input_tokens(usage);
    let window = context_window_from_usage(usage).unwrap_or(0);
    if input > 0 && window > 0 {
        (input as f64 / window as f64) * 100.0
    } else {
        0.0
    }
}

fn artifact_stub(path: &str) -> Value {
    let path = path.trim();
    if path.is_empty() {
        return json!({"path": "", "exists": false, "bytes": 0, "truncated": false});
    }
    match fs::metadata(path) {
        Ok(metadata) => json!({
            "path": path,
            "exists": true,
            "bytes": metadata.len(),
            "truncated": false,
        }),
        Err(_) => json!({"path": path, "exists": false, "bytes": 0, "truncated": false}),
    }
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

fn job_summary(jobs: &[Job]) -> Value {
    let mut by_state = BTreeMap::new();
    let mut by_kind = BTreeMap::new();
    let mut by_room = BTreeMap::<String, RoomJobSummary>::new();
    let mut active = 0;
    let mut queued = 0;
    let mut running = 0;
    let mut waiting = 0;
    let mut failed = 0;
    let mut cancellable = 0;

    for job in jobs {
        let state = job.state.as_str().to_string();
        let kind = job.kind.as_str().to_string();
        *by_state.entry(state.clone()).or_insert(0) += 1;
        *by_kind.entry(kind).or_insert(0) += 1;
        if !job.state.is_terminal() {
            active += 1;
        }
        if job.state.is_cancellable() {
            cancellable += 1;
        }
        match state.as_str() {
            "queued" => queued += 1,
            "running" => running += 1,
            "waiting" => waiting += 1,
            _ => {}
        }
        if is_failed_state(&state) {
            failed += 1;
        }

        let room_key = format!("{}\n{}", job.guild_id, job.voice_channel_id);
        let room = by_room.entry(room_key).or_insert_with(|| RoomJobSummary {
            guild_id: job.guild_id.clone(),
            voice_channel_id: job.voice_channel_id.clone(),
            ..RoomJobSummary::default()
        });
        room.total += 1;
        if !job.state.is_terminal() {
            room.active += 1;
        }
        if is_failed_state(&state) {
            room.failed += 1;
        }
        let latest_at = first_non_empty([
            job.updated_at.clone(),
            job.created_at.clone(),
            job.started_at.clone().unwrap_or_default(),
        ]);
        if latest_at > room.latest_at {
            room.latest_at = latest_at;
        }
    }

    json!({
        "total": jobs.len(),
        "active": active,
        "terminal": jobs.len().saturating_sub(active),
        "queued": queued,
        "running": running,
        "waiting": waiting,
        "failed": failed,
        "cancellable": cancellable,
        "byState": count_rows(by_state, "state"),
        "byKind": count_rows(by_kind, "kind"),
        "byRoom": room_job_rows(by_room),
    })
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

fn room_job_rows(rooms: BTreeMap<String, RoomJobSummary>) -> Vec<Value> {
    let mut rows = rooms.into_values().collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        right
            .active
            .cmp(&left.active)
            .then_with(|| right.failed.cmp(&left.failed))
            .then_with(|| right.total.cmp(&left.total))
            .then_with(|| right.latest_at.cmp(&left.latest_at))
    });
    rows.into_iter()
        .map(|room| {
            json!({
                "guild_id": room.guild_id,
                "voice_channel_id": room.voice_channel_id,
                "total": room.total,
                "active": room.active,
                "failed": room.failed,
                "latest_at": room.latest_at,
            })
        })
        .collect()
}

fn event_kind_counts(events: &[Value]) -> Vec<Value> {
    let mut summaries = BTreeMap::<String, EventKindSummary>::new();
    for event in events {
        let event_kind = first_non_empty([
            string_field(event, "event_kind"),
            string_field(event, "kind"),
            "event".to_string(),
        ]);
        let latest_at = first_non_empty([
            string_field(event, "startedAt"),
            string_field(event, "started_at"),
            string_field(event, "created_at"),
            string_field(event, "timestamp"),
        ]);
        let summary = summaries
            .entry(event_kind.clone())
            .or_insert_with(|| EventKindSummary {
                event_kind,
                ..EventKindSummary::default()
            });
        summary.count += 1;
        if latest_at > summary.latest_at {
            summary.latest_at = latest_at;
        }
    }
    let mut rows = summaries.into_values().collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        right
            .count
            .cmp(&left.count)
            .then_with(|| right.latest_at.cmp(&left.latest_at))
    });
    rows.into_iter()
        .map(|summary| {
            json!({
                "eventKind": summary.event_kind,
                "count": summary.count,
                "latestAt": summary.latest_at,
            })
        })
        .collect()
}
