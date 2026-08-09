use super::*;

pub(crate) const WAKE_CIRCUIT_ID: &str = "wake_provider";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeCircuitAdmission {
    Closed,
    HalfOpen,
}

#[derive(Debug, Clone, Default)]
pub struct WakeCircuitRow {
    pub consecutive_failures: i64,
    pub open_count: i64,
    pub open_until_ms: Option<i64>,
    pub half_open_started_at_ms: Option<i64>,
    pub last_failure_at_ms: Option<i64>,
    pub last_success_at_ms: Option<i64>,
    pub last_error: String,
    pub suppressed_probes: i64,
}

/// Durable wake-provider circuit breaker. The row is the source of truth:
/// a restart keeps an open circuit open instead of hammering a provider the
/// previous process already knew was down.
///
/// The half-open probe slot is leased rather than flagged — a process that
/// dies mid-probe releases the slot when `half_open_started_at_ms` ages past
/// the lease instead of suppressing probes forever.
impl TimelineStore {
    pub async fn wake_circuit_admit(
        &self,
        now_ms: i64,
        half_open_lease_ms: i64,
    ) -> Result<Option<WakeCircuitAdmission>> {
        self.ensure_wake_circuit_row().await?;
        let row = self.wake_circuit_row().await?;
        let Some(open_until_ms) = row.open_until_ms else {
            return Ok(Some(WakeCircuitAdmission::Closed));
        };
        if now_ms >= open_until_ms {
            let lease_floor = now_ms.saturating_sub(half_open_lease_ms);
            let claimed = sqlx::query(
                r#"
                UPDATE wake_circuit
                SET half_open_started_at_ms = $2, updated_at_ms = $2
                WHERE circuit_id = $1
                  AND open_until_ms IS NOT NULL
                  AND open_until_ms <= $2
                  AND (half_open_started_at_ms IS NULL OR half_open_started_at_ms < $3)
                "#,
            )
            .bind(WAKE_CIRCUIT_ID)
            .bind(now_ms)
            .bind(lease_floor)
            .execute(&self.pool)
            .await?
            .rows_affected();
            if claimed == 1 {
                return Ok(Some(WakeCircuitAdmission::HalfOpen));
            }
        }
        sqlx::query(
            r#"
            UPDATE wake_circuit
            SET suppressed_probes = suppressed_probes + 1, updated_at_ms = $2
            WHERE circuit_id = $1
            "#,
        )
        .bind(WAKE_CIRCUIT_ID)
        .bind(now_ms)
        .execute(&self.pool)
        .await?;
        Ok(None)
    }

    pub async fn wake_circuit_record_success(&self, now_ms: i64) -> Result<()> {
        self.ensure_wake_circuit_row().await?;
        sqlx::query(
            r#"
            UPDATE wake_circuit
            SET consecutive_failures = 0,
                open_count = 0,
                open_until_ms = NULL,
                half_open_started_at_ms = NULL,
                last_success_at_ms = $2,
                last_error = '',
                updated_at_ms = $2
            WHERE circuit_id = $1
            "#,
        )
        .bind(WAKE_CIRCUIT_ID)
        .bind(now_ms)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn wake_circuit_record_failure(
        &self,
        now_ms: i64,
        error: &str,
        failure_threshold: i64,
        open_initial_seconds: i64,
        open_max_seconds: i64,
    ) -> Result<()> {
        self.ensure_wake_circuit_row().await?;
        let mut transaction = self.pool.begin().await?;
        let row = sqlx::query(
            "SELECT * FROM wake_circuit WHERE circuit_id = $1 FOR UPDATE",
        )
        .bind(WAKE_CIRCUIT_ID)
        .fetch_one(transaction.as_mut())
        .await?;
        let mut state = wake_circuit_row_from(&row)?;
        state.last_failure_at_ms = Some(now_ms);
        state.last_error = error.to_string();
        state.consecutive_failures = state.consecutive_failures.saturating_add(1);
        let half_open_in_flight = state.half_open_started_at_ms.is_some();
        let currently_open = state.open_until_ms.is_some_and(|until| now_ms < until);
        if half_open_in_flight {
            state.half_open_started_at_ms = None;
            state.open_count = state.open_count.saturating_add(1).max(1);
            open_circuit(&mut state, now_ms, open_initial_seconds, open_max_seconds);
        } else if !currently_open && state.consecutive_failures >= failure_threshold.max(1) {
            state.open_count = state.open_count.max(1);
            open_circuit(&mut state, now_ms, open_initial_seconds, open_max_seconds);
        }
        sqlx::query(
            r#"
            UPDATE wake_circuit
            SET consecutive_failures = $2,
                open_count = $3,
                open_until_ms = $4,
                half_open_started_at_ms = $5,
                last_failure_at_ms = $6,
                last_error = $7,
                updated_at_ms = $8
            WHERE circuit_id = $1
            "#,
        )
        .bind(WAKE_CIRCUIT_ID)
        .bind(state.consecutive_failures)
        .bind(state.open_count)
        .bind(state.open_until_ms)
        .bind(state.half_open_started_at_ms)
        .bind(state.last_failure_at_ms)
        .bind(&state.last_error)
        .bind(now_ms)
        .execute(transaction.as_mut())
        .await?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn wake_circuit_row(&self) -> Result<WakeCircuitRow> {
        self.ensure_wake_circuit_row().await?;
        let row = sqlx::query("SELECT * FROM wake_circuit WHERE circuit_id = $1")
            .bind(WAKE_CIRCUIT_ID)
            .fetch_one(&self.pool)
            .await?;
        wake_circuit_row_from(&row)
    }

    async fn ensure_wake_circuit_row(&self) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO wake_circuit(circuit_id, updated_at_ms)
            VALUES ($1, $2)
            ON CONFLICT(circuit_id) DO NOTHING
            "#,
        )
        .bind(WAKE_CIRCUIT_ID)
        .bind(instant_ms_dt(utc_now()))
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

fn wake_circuit_row_from(row: &PgRow) -> Result<WakeCircuitRow> {
    Ok(WakeCircuitRow {
        consecutive_failures: row.try_get("consecutive_failures")?,
        open_count: row.try_get("open_count")?,
        open_until_ms: row.try_get("open_until_ms")?,
        half_open_started_at_ms: row.try_get("half_open_started_at_ms")?,
        last_failure_at_ms: row.try_get("last_failure_at_ms")?,
        last_success_at_ms: row.try_get("last_success_at_ms")?,
        last_error: row.try_get("last_error")?,
        suppressed_probes: row.try_get("suppressed_probes")?,
    })
}

fn open_circuit(
    state: &mut WakeCircuitRow,
    now_ms: i64,
    open_initial_seconds: i64,
    open_max_seconds: i64,
) {
    let open_initial_seconds = open_initial_seconds.max(1);
    let open_max_seconds = open_max_seconds.max(open_initial_seconds);
    let exponent = (state.open_count.saturating_sub(1)).clamp(0, 30) as u32;
    let seconds = open_initial_seconds
        .saturating_mul(2_i64.saturating_pow(exponent))
        .min(open_max_seconds);
    state.open_until_ms = Some(now_ms.saturating_add(seconds.saturating_mul(1000)));
}
