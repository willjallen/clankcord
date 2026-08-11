//! Agent session and job detail views: session rollups, agent-job payloads, artifact readers, and codex usage.

use std::fs;

use chrono::{DateTime, Utc};
use serde_json::{Map, Value, json};
use sqlx::Row;

use crate::Result;
use crate::adapters::codex::{parse_codex_trace, usage_payload_info};
use crate::domain::Ctx;
use crate::model::agents::task_session_key;
use crate::model::job::{Job, JobKind, JobState};
use crate::time::{isoformat_z, ms_to_datetime, parse_instant};
use crate::util::{first_non_empty, non_empty, preview, string_field};
use crate::views::render::dashboard_job_duration_ms;

const AGENT_ARTIFACT_MAX_BYTES: usize = 2 * 1024 * 1024;
const AGENT_SESSION_JOB_LIMIT: usize = 100;

/// The per-scope agent-session rollup: one implementation, one status
/// precedence (an active job wins; otherwise the latest job's failure
/// shows; otherwise idle). `scope` narrows to a single guild/scope pair.
pub(crate) async fn agent_session_rollups(
    ctx: &Ctx,
    scope: Option<(&str, &str)>,
) -> Result<Vec<Value>> {
    let scope_filter = if scope.is_some() {
        " AND guild_id = $1 AND scope_id = $2"
    } else {
        ""
    };
    let sql = format!(
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
              WHERE kind = 'agent_task'{scope_filter}
              GROUP BY scope_kind, guild_id, scope_id
            ), latest AS MATERIALIZED (
              SELECT DISTINCT ON (j.scope_kind, j.guild_id, j.scope_id)
                     j.scope_kind, j.guild_id, j.scope_id, p.payload_blob
              FROM jobs j
              JOIN job_payloads p ON p.job_id = j.job_id
              WHERE j.kind = 'agent_task'{scope_filter2}
              ORDER BY j.scope_kind, j.guild_id, j.scope_id,
                       j.updated_at_ms DESC, j.job_id DESC
            )
            SELECT grouped.*, latest.payload_blob
            FROM grouped
            JOIN latest USING (scope_kind, guild_id, scope_id)
            ORDER BY grouped.last_used_at_ms DESC, grouped.guild_id, grouped.scope_id
            "#,
        scope_filter = scope_filter,
        scope_filter2 = scope_filter
            .replace(" guild_id", " j.guild_id")
            .replace(" scope_id", " j.scope_id"),
    );
    let mut query = sqlx::query(&sql);
    if let Some((guild_id, scope_id)) = scope {
        query = query.bind(guild_id).bind(scope_id);
    }
    let rows = query.fetch_all(&ctx.store.pool).await?;
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

fn timestamp(milliseconds: i64) -> String {
    isoformat_z(Some(
        ms_to_datetime(milliseconds).expect("validated session timestamp"),
    ))
}

pub async fn dashboard_agent_job(ctx: &Ctx, job_id: &str) -> Result<Value> {
    let job = ctx.store.get_job(job_id).await?;
    if job.kind != JobKind::AgentTask {
        anyhow::bail!("job {job_id} is not an agent task");
    }
    agent_job_payload(ctx, &job).await
}

pub(crate) fn agent_usage_payload(jobs: &[Job], now: DateTime<Utc>) -> Value {
    codex_usage_rollup(jobs, now)
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

    for job in jobs.iter().filter(|job| job.kind == JobKind::AgentTask) {
        let usage = codex_usage_for_job(job);
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
    let key = task_session_key(&selected.guild_id, &selected.scope_id);
    let mut jobs = runtime
        .store
        .list_jobs_by_scope_kind(&selected.guild_id, &selected.scope_id, JobKind::AgentTask)
        .await?;
    let current = agent_session_rollups(runtime, Some((&selected.guild_id, &selected.scope_id)))
        .await?
        .into_iter()
        .find(|session| session.get("key").and_then(Value::as_str) == Some(key.as_str()));
    let selected_session_id = agent_job_session_id(selected, selected_codex);
    jobs.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then_with(|| left.id.cmp(&right.id))
    });
    let scope_job_count = jobs.len();
    let mut rows = jobs
        .iter()
        .filter(|job| agent_job_matches_selected_session(job, selected, &selected_session_id))
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
            if metadata.is_file()
                && metadata.len() <= 4096
                && let Ok(text) = fs::read_to_string(entry.path())
            {
                object.insert("preview".to_string(), json!(preview(&text, 1200)));
            }
            Some(Value::Object(object))
        })
        .collect::<Vec<_>>();
    files.sort_by_key(|left| string_field(left, "name"));
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
