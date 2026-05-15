use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::path::PathBuf;

use anyhow::Context;
use chrono::{DateTime, Utc};
use serde_json::{Map, Value, json};
use sqlx::Row;

use crate::Result;
use crate::adapters::codex::{codex_usage_payload, parse_codex_jsonl};
use crate::config::{non_empty, string_field};
use crate::runtime::agents::{AgentSession, AgentSessionStatus};
use crate::runtime::timeline::{
    event_start, isoformat_z, parse_instant, resolve_time_reference, utc_now,
};
use crate::runtime::util::first_non_empty;
use crate::runtime::{AgentRuntime, Job, JobKind, JobState, Runtime};

const AGENT_ARTIFACT_MAX_BYTES: usize = 2 * 1024 * 1024;
const AGENT_SESSION_ARTIFACT_MAX_BYTES: usize = 256 * 1024;
const AGENT_SESSION_JOB_LIMIT: usize = 100;

#[derive(Debug, Clone)]
pub struct DebugOverviewRequest {
    pub jobs_limit: usize,
    pub agent_limit: usize,
    pub timeline_since: String,
    pub timeline_limit: usize,
    pub transcript_since: String,
    pub transcript_limit: usize,
    pub publication_limit: usize,
}

impl Default for DebugOverviewRequest {
    fn default() -> Self {
        Self {
            jobs_limit: 120,
            agent_limit: 120,
            timeline_since: "-1h".to_string(),
            timeline_limit: 120,
            transcript_since: "-24h".to_string(),
            transcript_limit: 500,
            publication_limit: 120,
        }
    }
}

impl Runtime {
    pub async fn debug_overview(&self, request: DebugOverviewRequest) -> Result<Value> {
        let now = utc_now();
        let timeline_since = resolve_debug_since(&request.timeline_since, "-1h", now)?;
        let transcript_since = resolve_debug_since(&request.transcript_since, "-24h", now)?;
        let jobs_limit = request.jobs_limit.clamp(10, 500);
        let agent_limit = request.agent_limit.clamp(10, 500);
        let timeline_limit = request.timeline_limit.clamp(10, 1000);
        let transcript_limit = request.transcript_limit.clamp(10, 5000);
        let publication_limit = request.publication_limit.clamp(10, 500);
        let status = self.status_payload(None).await;
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
        let summary_jobs = merge_jobs(
            active_job_records
                .iter()
                .chain(failed_job_records.iter())
                .chain(recent_job_records.iter())
                .chain(agent_job_records.iter()),
        );
        let active_jobs = active_job_records
            .iter()
            .map(debug_job_value)
            .collect::<Vec<_>>();
        let recent_jobs = recent_job_records
            .iter()
            .map(debug_job_value)
            .collect::<Vec<_>>();
        let recent_events = self
            .recent_events(timeline_since, timeline_limit)
            .await
            .context("loading recent timeline events for debug overview")?;
        let transcript_events = self
            .recent_transcript_events(transcript_since, transcript_limit)
            .await
            .context("loading recent transcript events for debug overview")?;
        let event_kind_counts = event_kind_counts(&recent_events);
        let summary = job_summary(&summary_jobs);
        let database = database_diagnostics(self).await;
        let health = runtime_health(self, &summary_jobs, &database);
        let publications = self
            .timeline_store
            .list_publications(None, None, None)
            .await
            .context("loading publications for debug overview")?
            .into_iter()
            .take(publication_limit)
            .collect::<Vec<_>>();
        Ok(json!({
            "generatedAt": isoformat_z(Some(now)),
            "process": {
                "startedAt": isoformat_z(Some(self.started_at)),
                "uptimeSeconds": (now - self.started_at).num_seconds(),
                "autoJoin": {"enabled": self.auto_join_enabled},
            },
            "health": health,
            "database": database,
            "load": load_payload(&active_job_records, now),
            "agents": agent_dashboard_payload(&agent_job_records, agent_limit),
            "status": status,
            "jobs": {
                "summary": summary,
                "active": active_jobs,
                "recent": recent_jobs,
            },
            "timeline": {
                "since": debug_since_label(timeline_since),
                "recentEvents": recent_events,
                "eventKindCounts": event_kind_counts,
            },
            "transcript": {
                "since": debug_since_label(transcript_since),
                "events": transcript_events,
            },
            "publications": publications,
            "links": {
                "json": "/v1/voice/debug/overview",
                "poolStatus": "/v1/voice/pool/status",
                "timelineTail": "/v1/voice/timeline/tail",
                "jobs": "/v1/voice/jobs",
            }
        }))
    }

    pub async fn recent_events(
        &self,
        since: Option<DateTime<Utc>>,
        limit: usize,
    ) -> Result<Vec<Value>> {
        self.recent_events_by_kind(since, limit, None).await
    }

    pub async fn recent_transcript_events(
        &self,
        since: Option<DateTime<Utc>>,
        limit: usize,
    ) -> Result<Vec<Value>> {
        let kinds = BTreeSet::from(["speech_segment".to_string(), "transcript".to_string()]);
        self.recent_events_by_kind(since, limit, Some(&kinds)).await
    }

    async fn recent_events_by_kind(
        &self,
        since: Option<DateTime<Utc>>,
        limit: usize,
        kinds: Option<&BTreeSet<String>>,
    ) -> Result<Vec<Value>> {
        let mut events = Vec::new();
        for room in self.known_rooms() {
            let mut room_events = self
                .timeline_store
                .load_events(
                    &room.guild_id,
                    &room.channel_id,
                    since,
                    None,
                    kinds,
                    None,
                    false,
                )
                .await?;
            events.append(&mut room_events);
        }
        events.sort_by_key(|event| event_start(event).unwrap_or_else(utc_now));
        events.reverse();
        events.truncate(limit);
        Ok(events)
    }

    pub async fn debug_agent_job(&self, job_id: &str) -> Result<Value> {
        let job = self.timeline_store.get_job(job_id).await?;
        if job.kind != JobKind::AgentTask {
            anyhow::bail!("job {job_id} is not an agent task");
        }
        agent_job_payload(self, &job).await
    }
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

fn debug_job_value(job: &Job) -> Value {
    let mut value = job.to_value();
    if let Value::Object(object) = &mut value {
        let command_kind = job.command_kind();
        if !command_kind.trim().is_empty() {
            object.insert("command_kind".to_string(), json!(command_kind));
        }
    }
    value
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
    env::var("CODEX_HOME")
        .or_else(|_| env::var("HOME").map(|home| format!("{home}/.codex")))
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(".codex"))
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
    Ok(json!({
        "job": job.to_value(),
        "paths": {
            "packet": metadata.packet_path,
            "prompt": metadata.prompt_path,
            "result": metadata.result_path,
            "raw": metadata.raw_result_path,
        },
        "packet": read_json_artifact(&metadata.packet_path, AGENT_ARTIFACT_MAX_BYTES),
        "prompt": read_text_artifact(&metadata.prompt_path, AGENT_ARTIFACT_MAX_BYTES),
        "result": read_text_artifact(&metadata.result_path, AGENT_ARTIFACT_MAX_BYTES),
        "raw": raw,
        "codex": codex,
        "session": agent_session_payload(runtime, job).await?,
    }))
}

async fn agent_session_payload(runtime: &Runtime, selected: &Job) -> Result<Value> {
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
    jobs.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then_with(|| left.id.cmp(&right.id))
    });
    let truncated = jobs.len() > AGENT_SESSION_JOB_LIMIT;
    if truncated {
        jobs = jobs[jobs.len().saturating_sub(AGENT_SESSION_JOB_LIMIT)..].to_vec();
    }
    let rows = jobs
        .iter()
        .map(agent_session_job_payload)
        .collect::<Vec<_>>();
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
            }
            timeline.push(event);
        }
    }
    Ok(json!({
        "key": key,
        "current": current,
        "jobCount": rows.len(),
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
    let usage = if metadata.agent.usage.is_empty() {
        json!({})
    } else {
        usage_payload_info(&metadata.agent.usage.to_json())
    };
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
            "packet": metadata.packet_path,
            "prompt": metadata.prompt_path,
            "result": metadata.result_path,
            "raw": metadata.raw_result_path,
        },
        "packet": artifact_stub(&metadata.packet_path),
        "prompt": artifact_stub(&metadata.prompt_path),
        "result": artifact_stub(&metadata.result_path),
        "raw": artifact_stub(&metadata.raw_result_path),
        "codex": {
            "sessionId": metadata.agent.session_id,
            "model": metadata.agent.model,
            "tokenUsage": usage,
            "contextUsedTokens": token_usage_input_tokens(&usage),
            "modelContextWindow": context_window_from_usage(&usage).unwrap_or(0),
            "contextUsedPercent": context_used_percent(&usage),
            "eventCount": 0,
            "timeline": [],
            "messages": [],
            "toolCalls": [],
        },
        "detailUrl": format!("/v1/voice/debug/agents/{}", job.id),
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

fn read_json_artifact(path: &str, max_bytes: usize) -> Value {
    let artifact = read_text_artifact(path, max_bytes);
    let parsed = artifact
        .get("content")
        .and_then(Value::as_str)
        .and_then(|content| serde_json::from_str::<Value>(content).ok())
        .unwrap_or_else(|| json!({}));
    json!({
        "path": artifact.get("path").cloned().unwrap_or_else(|| json!("")),
        "exists": artifact.get("exists").cloned().unwrap_or_else(|| json!(false)),
        "bytes": artifact.get("bytes").cloned().unwrap_or_else(|| json!(0)),
        "truncated": artifact.get("truncated").cloned().unwrap_or_else(|| json!(false)),
        "value": parsed,
        "content": artifact.get("content").cloned().unwrap_or_else(|| json!("")),
    })
}

fn parse_codex_trace(raw: &str) -> Value {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_codex_jsonl_populates_agent_debug_trace() {
        let raw = r#"
{"type":"thread.started","thread_id":"019e270d-878f-70c0-855e-456d2225d85c"}
{"type":"turn.started"}
{"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"I will answer and publish through Clankcord."}}
{"type":"item.started","item":{"id":"item_1","type":"command_execution","command":"clankcord responses submit --job job_1 --stdin","aggregated_output":"","exit_code":null,"status":"in_progress"}}
{"type":"item.completed","item":{"id":"item_1","type":"command_execution","command":"clankcord responses submit --job job_1 --stdin","aggregated_output":"{\"job_ids\":[\"job_response\"]}\n","exit_code":0,"status":"completed"}}
{"type":"item.completed","item":{"id":"item_2","type":"agent_message","text":"RESPONSE_SUBMITTED"}}
{"type":"turn.completed","usage":{"input_tokens":60770,"cached_input_tokens":36096,"output_tokens":2492,"reasoning_output_tokens":1876}}
"#;

        let trace = parse_codex_trace(raw);

        assert_eq!(
            trace.get("sessionId").and_then(Value::as_str),
            Some("019e270d-878f-70c0-855e-456d2225d85c")
        );
        assert_eq!(trace.get("eventCount").and_then(Value::as_u64), Some(7));
        assert_eq!(
            trace
                .get("messages")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(2)
        );
        assert_eq!(
            trace
                .get("toolCalls")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(2)
        );
        assert_eq!(
            trace.get("contextUsedTokens").and_then(Value::as_i64),
            Some(60770)
        );
        assert_eq!(
            trace
                .get("tokenUsage")
                .and_then(|value| value.get("total_token_usage"))
                .and_then(|value| value.get("cached_input_tokens"))
                .and_then(Value::as_i64),
            Some(36096)
        );
    }
}
