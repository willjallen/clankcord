use chrono::Utc;
use serde_json::json;

mod common;

use clankcord::runtime::{DebugOverviewRequest, Runtime};

use common::{initialize_test_config, test_store};

#[tokio::test(flavor = "current_thread")]
async fn dashboard_health_reports_postgres_diagnostics() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(raw.path()).await;
    let runtime = Runtime::from_store(store).unwrap();

    let overview = runtime
        .debug_overview(DebugOverviewRequest::default())
        .await
        .unwrap();
    let database = &overview["database"];

    assert_eq!(database["ok"], json!(true));
    assert!(
        database["statistics"]["databaseSizeBytes"]
            .as_i64()
            .is_some_and(|bytes| bytes > 0),
        "database diagnostics payload: {database}"
    );
    assert!(
        database["pool"]["configuredMaxConnections"]
            .as_u64()
            .is_some_and(|connections| connections > 0)
    );
    assert!(
        database["activity"]
            .as_array()
            .is_some_and(|rows| !rows.is_empty())
    );
    assert!(
        database["tableActivity"]
            .as_array()
            .is_some_and(|rows| !rows.is_empty())
    );
    assert!(
        database["tables"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["table"] == "jobs" && row["totalBytes"].as_i64().unwrap() > 0)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dashboard_health_includes_http_request_snapshot() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(raw.path()).await;
    let runtime = Runtime::from_store(store).unwrap();
    let requests = json!({
        "totalStarted": 9,
        "completed": 8,
        "inFlight": 1,
        "routes": [{"route": "GET /debug", "totalStarted": 3}]
    });

    let overview = runtime
        .debug_overview(DebugOverviewRequest {
            http_requests: requests.clone(),
            ..DebugOverviewRequest::default()
        })
        .await
        .unwrap();

    assert_eq!(overview["requests"], requests);
}

#[tokio::test(flavor = "current_thread")]
async fn dashboard_latency_stats_exclude_phase_contaminated_intervals() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(raw.path()).await;
    let base_ms = Utc::now().timestamp_millis() - 30_000;

    sqlx::query(
        r#"
        INSERT INTO jobs(
          job_id, guild_id, voice_channel_id, kind, state, terminal, failed,
          ephemeral, cancellable, lane, ordering_key, ready_at_ms, created_at_ms,
          updated_at_ms, started_at_ms, completed_at_ms
        )
        VALUES
          ('job_latency_clean', 'guild', 'code', 'wake_activation', 'complete', TRUE, FALSE,
           FALSE, FALSE, 'voice_control', 'latency-test', $1, $2, $5, $3, $4),
          ('job_latency_phase', 'guild', 'code', 'wake_activation', 'complete', TRUE, FALSE,
           FALSE, FALSE, 'voice_control', 'latency-test', $8, $6, $10, $7, $9)
        "#,
    )
    .bind(base_ms + 250)
    .bind(base_ms)
    .bind(base_ms + 500)
    .bind(base_ms + 1000)
    .bind(base_ms + 1000)
    .bind(base_ms + 2000)
    .bind(base_ms + 3000)
    .bind(base_ms + 5000)
    .bind(base_ms + 6000)
    .bind(base_ms + 6000)
    .execute(&store.pool)
    .await
    .unwrap();

    let runtime = Runtime::from_store(store).unwrap();
    let overview = runtime
        .debug_overview(DebugOverviewRequest::default())
        .await
        .unwrap();
    let latency_rows = overview["operations"]["latencies"]["byKind"]
        .as_array()
        .unwrap();
    let wake_activation = latency_rows
        .iter()
        .find(|row| row["kind"].as_str() == Some("wake_activation"))
        .unwrap();

    assert_eq!(wake_activation["count"], json!(2));
    assert_eq!(wake_activation["totalMs"]["count"], json!(2));
    assert_eq!(wake_activation["totalMs"]["max"], json!(4000));
    assert_eq!(wake_activation["readyDelayMs"]["count"], json!(1));
    assert_eq!(wake_activation["readyDelayMs"]["p50"], json!(250));
    assert_eq!(wake_activation["queueMs"]["count"], json!(1));
    assert_eq!(wake_activation["queueMs"]["p50"], json!(250));
    assert_eq!(wake_activation["runMs"]["count"], json!(2));
    assert_eq!(wake_activation["runMs"]["max"], json!(3000));
    assert_eq!(wake_activation["excluded"]["phaseContaminated"], json!(1));
    assert_eq!(wake_activation["excluded"]["readyDelayMs"], json!(1));
    assert_eq!(wake_activation["excluded"]["queueMs"], json!(1));
}
