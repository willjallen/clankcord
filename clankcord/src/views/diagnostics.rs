//! Operational diagnostics: the Postgres DBA block, job/event windows, latency, backlog, and failure summaries.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use serde_json::{Map, Value, json};
use sqlx::Row;

use crate::Result;
use crate::domain::Ctx;
use crate::store::OPERATIONAL_JOB_OUTCOME_RETENTION_SECONDS;
use crate::time::{instant_ms_dt, isoformat_z, ms_to_datetime};
use crate::util::round3;
use crate::util::string_field;
use crate::views::render;

const HEALTH_WINDOWS: &[(&str, i64)] = &[("5m", 5 * 60), ("15m", 15 * 60), ("1h", 60 * 60)];
pub(crate) const FAILURE_WINDOW_SECONDS: i64 = 60 * 60;
const FAILURE_RECENT_LIMIT: i64 = 25;
const OPERATIONAL_COVERAGE_START_KEY: &str = "operational_job_outcomes_coverage_start_ms";

#[derive(Debug, Clone)]
pub(crate) struct JobDiagnosticRow {
    pub(crate) job_id: String,
    pub(crate) kind: String,
    pub(crate) state: String,
    pub(crate) lane: String,
    pub(crate) created_at_ms: i64,
    pub(crate) updated_at_ms: i64,
    pub(crate) ready_at_ms: i64,
    pub(crate) started_at_ms: Option<i64>,
    pub(crate) completed_at_ms: Option<i64>,
    pub(crate) terminal: bool,
    pub(crate) failed: bool,
    pub(crate) cancellable: bool,
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
            "category": render::dashboard_job_category(&self.kind),
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
pub(crate) struct OperationalDiagnostics {
    pub(crate) payload: Value,
    pub(crate) job_rows: Vec<JobDiagnosticRow>,
    pub(crate) failure_summary: Value,
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
    pub(crate) fn activity_ms(&self) -> i64 {
        let mut activity = self.created_at_ms.max(self.updated_at_ms);
        if let Some(started_at_ms) = self.started_at_ms {
            activity = activity.max(started_at_ms);
        }
        if let Some(completed_at_ms) = self.completed_at_ms {
            activity = activity.max(completed_at_ms);
        }
        activity
    }

    pub(crate) fn is_active(&self) -> bool {
        !self.terminal
    }

    pub(crate) fn is_failed(&self) -> bool {
        self.failed
    }

    pub(crate) fn terminal_at_ms(&self) -> Option<i64> {
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
pub(crate) struct BacklogKindSummary {
    pub(crate) kind: String,
    pub(crate) active: usize,
    pub(crate) queued: usize,
    pub(crate) due_queued: usize,
    pub(crate) running: usize,
    pub(crate) waiting: usize,
    pub(crate) cancel_requested: usize,
    pub(crate) confirmation_pending: usize,
    pub(crate) cancellable: usize,
    pub(crate) oldest_queued_age_seconds: i64,
    pub(crate) oldest_running_age_seconds: i64,
    pub(crate) oldest_active_age_seconds: i64,
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

    pub(crate) fn to_json(&self) -> Value {
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

pub(crate) async fn operational_failure_summary(
    runtime: &Ctx,
    now: DateTime<Utc>,
    include_recent: bool,
) -> Result<Value> {
    let now_ms = instant_ms_dt(now);
    let since_ms = now_ms - FAILURE_WINDOW_SECONDS * 1000;
    let coverage_start_ms = operational_coverage_start_ms(runtime).await?;
    let count = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM operational_job_outcomes WHERE failed = TRUE AND observed_at_ms >= $1",
    )
    .bind(since_ms)
    .fetch_one(&runtime.store.pool)
    .await?;
    let recent = if include_recent {
        recent_failure_rows(runtime, since_ms, FAILURE_RECENT_LIMIT).await?
    } else {
        Vec::new()
    };
    Ok(failure_summary_payload(
        since_ms,
        count as usize,
        coverage_start_ms,
        recent,
    ))
}

fn failure_summary_payload(
    since_ms: i64,
    count: usize,
    coverage_start_ms: i64,
    recent: Vec<FailureDiagnosticRow>,
) -> Value {
    json!({
        "window": "1h",
        "since": ms_iso(since_ms),
        "count": count,
        "complete": coverage_start_ms <= since_ms,
        "coverageStartsAt": ms_iso(coverage_start_ms),
        "recent": recent.into_iter().map(|row| row.to_json()).collect::<Vec<_>>(),
    })
}

pub(crate) async fn database_diagnostics(runtime: &Ctx) -> Value {
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

pub(crate) fn postgres_pool_payload(runtime: &Ctx) -> Value {
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

pub(crate) async fn operational_diagnostics(
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

pub(crate) async fn dashboard_latency_by_kind_payload(
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
    let failures = operational_failure_summary(runtime, now, true).await?;
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
    Ok(failure_summary_payload(
        since_ms,
        count,
        coverage_start_ms,
        recent,
    ))
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
    let scope_labels = render::dashboard_scope_label_batch(runtime, &scope_keys).await?;
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
                .filter(|&event| !event.speaker_user_id.trim().is_empty() ).map(|event| event.speaker_user_id.clone())
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

pub(crate) fn age_seconds(now_ms: i64, then_ms: i64) -> i64 {
    ((now_ms - then_ms) / 1000).max(0)
}

pub(crate) fn ms_iso(value: i64) -> String {
    ms_to_datetime(value)
        .map(|instant| isoformat_z(Some(instant)))
        .expect("health timestamp is representable")
}

pub(crate) fn count_pair_rows(
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

pub(crate) fn json_usize(value: &Value, key: &str) -> usize {
    value.get(key).and_then(Value::as_u64).unwrap_or(0) as usize
}

pub(crate) fn count_rows(counts: BTreeMap<String, usize>, label_key: &str) -> Vec<Value> {
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
