use super::*;

/// A recurring-work declaration: one row per scheduled unit of work.
///
/// Schedules are the system's single clock for recurring jobs — maintenance,
/// sweeps, syncs, and any future automated task (conversation summaries,
/// memory backends, cron-style work). A row names the `JobKind` to mint, the
/// payload to build it from, and the cadence; the engine claims due rows and
/// submits jobs through the bus like any other work.
#[derive(Debug, Clone)]
pub struct JobScheduleRow {
    pub schedule_id: String,
    pub kind: String,
    pub payload_json: Value,
    pub interval_ms: i64,
    pub enabled: bool,
    pub next_due_at_ms: i64,
    pub last_submitted_at_ms: Option<i64>,
    pub last_job_id: String,
}

impl TimelineStore {
    /// Idempotent declaration. A new row becomes due immediately; an existing
    /// row keeps its cadence position but adopts the new interval, payload,
    /// and enablement.
    pub async fn upsert_job_schedule(
        &self,
        schedule_id: &str,
        kind: &str,
        payload_json: &Value,
        interval_ms: i64,
        enabled: bool,
    ) -> Result<()> {
        let now_ms = instant_ms_dt(utc_now());
        sqlx::query(
            r#"
            INSERT INTO job_schedules(
              schedule_id, kind, payload_json, interval_ms, enabled,
              next_due_at_ms, created_at_ms, updated_at_ms
            )
            VALUES ($1, $2, $3, $4, $5, $6, $6, $6)
            ON CONFLICT(schedule_id) DO UPDATE SET
              kind = EXCLUDED.kind,
              payload_json = EXCLUDED.payload_json,
              interval_ms = EXCLUDED.interval_ms,
              enabled = EXCLUDED.enabled,
              updated_at_ms = EXCLUDED.updated_at_ms
            "#,
        )
        .bind(schedule_id)
        .bind(kind)
        .bind(payload_json)
        .bind(interval_ms.max(1000))
        .bind(enabled)
        .bind(now_ms)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Claims every due schedule in one atomic statement: the reschedule to
    /// `now + interval` happens in the same UPDATE that selects the row, so a
    /// schedule fires exactly once per due window and a long outage yields
    /// one catch-up run, not a backlog.
    pub async fn claim_due_job_schedules(&self, now_ms: i64) -> Result<Vec<JobScheduleRow>> {
        let rows = sqlx::query(
            r#"
            UPDATE job_schedules
            SET next_due_at_ms = $1 + interval_ms,
                last_submitted_at_ms = $1,
                updated_at_ms = $1
            WHERE enabled AND next_due_at_ms <= $1
            RETURNING *
            "#,
        )
        .bind(now_ms)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(job_schedule_from_row).collect()
    }

    pub async fn record_job_schedule_submission(
        &self,
        schedule_id: &str,
        job_id: &str,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE job_schedules SET last_job_id = $2, updated_at_ms = $3 WHERE schedule_id = $1",
        )
        .bind(schedule_id)
        .bind(job_id)
        .bind(instant_ms_dt(utc_now()))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn next_due_job_schedule_at_ms(&self) -> Result<Option<i64>> {
        let row = sqlx::query(
            r#"
            SELECT next_due_at_ms
            FROM job_schedules
            WHERE enabled
            ORDER BY next_due_at_ms
            LIMIT 1
            "#,
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(row
            .map(|row| row.try_get::<i64, _>("next_due_at_ms"))
            .transpose()?)
    }

    pub async fn list_job_schedules(&self) -> Result<Vec<JobScheduleRow>> {
        let rows = sqlx::query("SELECT * FROM job_schedules ORDER BY schedule_id")
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(job_schedule_from_row).collect()
    }

    pub async fn set_job_schedule_enabled(&self, schedule_id: &str, enabled: bool) -> Result<bool> {
        let changed = sqlx::query(
            "UPDATE job_schedules SET enabled = $2, updated_at_ms = $3 WHERE schedule_id = $1",
        )
        .bind(schedule_id)
        .bind(enabled)
        .bind(instant_ms_dt(utc_now()))
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok(changed == 1)
    }
}

fn job_schedule_from_row(row: &PgRow) -> Result<JobScheduleRow> {
    Ok(JobScheduleRow {
        schedule_id: row.try_get("schedule_id")?,
        kind: row.try_get("kind")?,
        payload_json: row.try_get("payload_json")?,
        interval_ms: row.try_get("interval_ms")?,
        enabled: row.try_get("enabled")?,
        next_due_at_ms: row.try_get("next_due_at_ms")?,
        last_submitted_at_ms: row.try_get("last_submitted_at_ms")?,
        last_job_id: row.try_get("last_job_id")?,
    })
}
