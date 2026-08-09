use chrono::{DateTime, Utc};
use serde_json::{Value, json};

use crate::Result;
use crate::config;
use crate::runtime::timeline::store::WakeCircuitAdmission;
use crate::runtime::timeline::{TimelineStore, instant_ms_dt, ms_to_datetime, utc_now};

/// Extra slack past the provider timeout before an abandoned half-open probe
/// lease is considered expired and another probe may claim it.
const HALF_OPEN_LEASE_SLACK_SECONDS: u64 = 30;

pub(crate) async fn acquire_wake_probe_admission(
    store: &TimelineStore,
) -> Result<Option<WakeCircuitAdmission>> {
    let lease_ms =
        (config::wake_timeout_seconds() + HALF_OPEN_LEASE_SLACK_SECONDS).saturating_mul(1000);
    store
        .wake_circuit_admit(instant_ms_dt(utc_now()), lease_ms as i64)
        .await
}

pub(crate) async fn record_wake_provider_success(store: &TimelineStore) -> Result<()> {
    store
        .wake_circuit_record_success(instant_ms_dt(utc_now()))
        .await
}

pub(crate) async fn record_wake_provider_failure(
    store: &TimelineStore,
    error: &anyhow::Error,
) -> Result<()> {
    store
        .wake_circuit_record_failure(
            instant_ms_dt(utc_now()),
            &sanitize_provider_error(&error.to_string()),
            config::wake_circuit_failure_threshold() as i64,
            config::wake_circuit_open_initial_seconds() as i64,
            config::wake_circuit_open_max_seconds() as i64,
        )
        .await
}

pub(crate) async fn wake_provider_health(store: &TimelineStore) -> Result<Value> {
    let row = store.wake_circuit_row().await?;
    let now_ms = instant_ms_dt(utc_now());
    let status = match row.open_until_ms {
        None => "closed",
        Some(open_until_ms) if now_ms < open_until_ms => "open",
        Some(_) => "half_open",
    };
    Ok(json!({
        "status": status,
        "available": status == "closed",
        "consecutiveFailures": row.consecutive_failures,
        "failureThreshold": config::wake_circuit_failure_threshold(),
        "openCount": row.open_count,
        "nextProbeAt": format_ms(row.open_until_ms),
        "halfOpenProbeInFlight": row.half_open_started_at_ms.is_some(),
        "lastFailureAt": format_ms(row.last_failure_at_ms),
        "lastSuccessAt": format_ms(row.last_success_at_ms),
        "lastError": row.last_error,
        "suppressedProbes": row.suppressed_probes,
    }))
}

fn format_ms(value: Option<i64>) -> String {
    value
        .and_then(ms_to_datetime)
        .map(format_instant)
        .unwrap_or_default()
}

fn format_instant(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn sanitize_provider_error(error: &str) -> String {
    error
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(240)
        .collect()
}
