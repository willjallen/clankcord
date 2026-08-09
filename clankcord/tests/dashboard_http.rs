use axum::http::StatusCode;
use serde_json::json;

use clankcord::app::http::{dashboard_asset_router, health_error_payload, readiness_http_status};

#[test]
fn dashboard_readiness_status_and_error_copy_are_explicit() {
    for status in ["ok", "degraded"] {
        assert_eq!(
            readiness_http_status(&json!({"status": status})),
            StatusCode::OK
        );
    }
    for status in ["down", "stale", "unknown", "unexpected"] {
        assert_eq!(
            readiness_http_status(&json!({"status": status})),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }
    assert_eq!(
        readiness_http_status(&json!({})),
        StatusCode::SERVICE_UNAVAILABLE
    );

    let payload = health_error_payload(&anyhow::anyhow!("database password leaked here"));
    assert_eq!(payload["status"], json!("down"));
    assert_eq!(
        payload["components"][0]["reason"],
        json!("Health query failed")
    );
    assert_eq!(
        payload["components"][0]["details"]["error"],
        json!("database password leaked here")
    );
}

#[tokio::test]
async fn dashboard_assets_resolve_and_legacy_debug_routes_are_absent() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, dashboard_asset_router::<()>())
            .await
            .unwrap();
    });
    let client = reqwest::Client::new();
    for path in [
        "/dashboard",
        "/dashboard/dashboard.css",
        "/dashboard/dashboard.js",
        "/dashboard/dashboard-explorer.js",
        "/dashboard/dashboard-charts.js",
        "/dashboard/dashboard-tables.js",
        "/dashboard/dashboard-json.js",
        "/dashboard/echarts.min.js",
        "/dashboard/tabulator.min.js",
        "/dashboard/tabulator_midnight.min.css",
        "/dashboard/alpine.min.js",
    ] {
        let response = client
            .get(format!("http://{address}{path}"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "asset route {path}");
        assert!(
            !response.bytes().await.unwrap().is_empty(),
            "asset route {path}"
        );
    }
    for path in ["/debug", "/debug/dashboard.js", "/v1/debug/overview"] {
        assert_eq!(
            client
                .get(format!("http://{address}{path}"))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND,
            "legacy route {path}"
        );
    }
    server.abort();

    let source = include_str!("../src/app/http.rs");
    for path in [
        "/v1/dashboard/timeline",
        "/v1/dashboard/jobs",
        "/v1/dashboard/summary",
        "/v1/dashboard/overview",
        "/v1/dashboard/agents",
        "/v1/dashboard/agents/{job_id}",
        "/v1/dashboard/automations",
        "/v1/dashboard/health",
        "/v1/dashboard/rooms",
        "/v1/dashboard/transcript",
    ] {
        assert!(
            source.contains(&format!(".route(\"{path}\"")),
            "dashboard API route {path} must be registered"
        );
    }
    assert!(!source.contains(".route(\"/debug"));
    assert!(!source.contains(".route(\"/v1/debug"));
}
