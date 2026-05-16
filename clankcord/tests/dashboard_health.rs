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
