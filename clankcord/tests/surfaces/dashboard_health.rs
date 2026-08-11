//! Dashboard health and summary payloads: diagnostics, reasons, and groupings an operator reads.

use crate::support::initialize_test_config;
use crate::support::test_store;
use chrono::Duration;
use chrono::Utc;
use clankcord::domain::Ctx;
use clankcord::model::job::CommandRequest;
use clankcord::model::job::Job;
use clankcord::model::job::JobState;
use clankcord::model::scope::RuntimeScope;
use clankcord::model::voice::VoiceBotStatus;
use clankcord::model::voice::VoiceCaptureSessionStatus;
use clankcord::views::DashboardFilter;
use clankcord::views::DashboardOverviewRequest;
use clankcord::views::DashboardTimelineRequest;
use serde_json::json;

#[tokio::test(flavor = "current_thread")]
async fn dashboard_health_reports_postgres_diagnostics() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(raw.path()).await;
    let runtime = Ctx::new(store);

    let overview =
        clankcord::views::operations::dashboard_health_payload(&runtime, json!({}), json!({}))
            .await
            .unwrap();
    let database = &overview["database"];

    assert_eq!(database["ok"], json!(true));
    let components = overview["health"]["components"].as_array().unwrap();
    let postgres = components
        .iter()
        .find(|component| component["component"] == "postgres")
        .unwrap();
    assert_eq!(postgres["status"], json!("ok"));
    assert!(
        postgres["reason"]
            .as_str()
            .unwrap()
            .ends_with(" connections")
    );
    assert!(!postgres["reason"].as_str().unwrap().contains("Reachable"));
    assert_eq!(postgres["details"]["queryErrorCount"], json!(0));
    assert!(postgres["details"].get("diagnosticErrorCount").is_none());
    let wake_provider = components
        .iter()
        .find(|component| component["component"] == "wake_provider")
        .unwrap();
    assert_eq!(wake_provider["status"], json!("ok"));
    assert_eq!(wake_provider["details"]["status"], json!("closed"));
    assert_eq!(wake_provider["details"]["available"], json!(true));
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
async fn dashboard_health_reasons_are_terse_measured_operator_state() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(raw.path()).await;
    let mut heartbeat = Job::runtime_maintenance(15_000);
    heartbeat.mark_complete();
    store.create_job(heartbeat).await.unwrap();
    store
        .record_voice_adapter_snapshot(0, 0, 0, 0)
        .await
        .unwrap();
    let runtime = Ctx::new(store);

    let payload = clankcord::views::operations::dashboard_summary_payload(&runtime)
        .await
        .unwrap();
    let components = payload["health"]["components"].as_array().unwrap();
    let scheduler = components
        .iter()
        .find(|component| component["component"] == "scheduler")
        .unwrap();
    assert!(
        scheduler["reason"]
            .as_str()
            .unwrap()
            .starts_with("Heartbeat ")
    );
    assert!(
        scheduler["reason"]
            .as_str()
            .unwrap()
            .ends_with(" · no due backlog")
    );
    let voice = components
        .iter()
        .find(|component| component["component"] == "voice_gateway")
        .unwrap();
    assert!(
        voice["reason"]
            .as_str()
            .unwrap()
            .starts_with("0/0 ready · snapshot ")
    );

    for component in components {
        let reason = component["reason"].as_str().unwrap();
        assert!(
            reason.chars().count() <= 100,
            "verbose health reason: {reason}"
        );
        let normalized = reason.to_lowercase();
        for rejected in [
            "dashboard",
            "diagnostic",
            "reachable",
            "unreachable",
            "available",
            "unavailable",
            "one or more",
            "has been",
            "is current",
            "are current",
            "progressing",
            "freshness threshold",
        ] {
            assert!(
                !normalized.contains(rejected),
                "health reason contains `{rejected}`: {reason}"
            );
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn dashboard_health_includes_http_request_snapshot() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(raw.path()).await;
    let runtime = Ctx::new(store);
    let requests = json!({
        "totalStarted": 9,
        "completed": 8,
        "inFlight": 1,
        "routes": [{"route": "GET /dashboard", "totalStarted": 3}]
    });

    let overview = clankcord::views::operations::dashboard_health_payload(
        &runtime,
        requests.clone(),
        serde_json::json!({}),
    )
    .await
    .unwrap();

    assert_eq!(overview["requests"], requests);
}

#[tokio::test(flavor = "current_thread")]
async fn dashboard_summary_uses_active_projection_aggregates_and_bounded_failures() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(raw.path()).await;
    let queued = Job::new(
        RuntimeScope::dm("user-a"),
        "user-a",
        JobState::Queued,
        clankcord::model::job::JobPayload::Command(clankcord::model::job::CommandPayload {
            command: CommandRequest::agent_task("", "user-a", "user-a", "summary projection"),
        }),
    );
    store.create_job(queued).await.unwrap();
    let mut failed = Job::runtime_maintenance(15_000);
    failed.set_state(JobState::Failed);
    failed.metadata.error = "summary maintenance failure".to_string();
    store.create_job(failed).await.unwrap();
    let coverage_start = (Utc::now() - Duration::hours(2)).timestamp_millis();
    sqlx::query(
        "UPDATE runtime_metadata SET value = $1, updated_at_ms = $1 WHERE key = 'operational_job_outcomes_coverage_start_ms'",
    )
    .bind(coverage_start)
    .execute(&store.pool)
    .await
    .unwrap();
    let runtime = Ctx::new(store);

    let summary = clankcord::views::operations::dashboard_summary_payload(&runtime)
        .await
        .unwrap();

    let keys = summary
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        keys,
        std::collections::BTreeSet::from(["generatedAt", "health", "jobs", "operations"])
    );
    assert_eq!(summary["jobs"]["summary"]["total"], json!(1));
    assert_eq!(summary["jobs"]["summary"]["active"], json!(1));
    assert_eq!(summary["jobs"]["summary"]["terminal"], json!(0));
    assert_eq!(summary["jobs"]["summary"]["failed"], json!(0));
    assert_eq!(summary["operations"]["backlog"]["total"], json!(1));
    assert_eq!(summary["health"]["failures"]["count"], json!(1));
    assert_eq!(summary["health"]["failures"]["complete"], json!(true));
    assert!(
        summary["health"]["failures"]["recent"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dashboard_job_summary_groups_by_runtime_scope() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(raw.path()).await;
    store
        .create_job(Job::new(
            RuntimeScope::voice_channel("guild", "voice"),
            "system",
            JobState::Queued,
            clankcord::model::job::JobPayload::Command(clankcord::model::job::CommandPayload {
                command: CommandRequest::agent_task("guild", "voice", "system", "voice"),
            }),
        ))
        .await
        .unwrap();
    store
        .create_job(Job::new(
            RuntimeScope::dm("user"),
            "system",
            JobState::Failed,
            clankcord::model::job::JobPayload::Command(clankcord::model::job::CommandPayload {
                command: CommandRequest::agent_task("", "user", "system", "dm"),
            }),
        ))
        .await
        .unwrap();
    let runtime = Ctx::new(store);

    let overview = clankcord::views::dashboard::dashboard_overview(
        &runtime,
        DashboardOverviewRequest::default(),
    )
    .await
    .unwrap();
    let summary = &overview["jobs"]["summary"];
    let scopes = summary["byScope"].as_array().unwrap();
    let timeline = clankcord::views::dashboard::dashboard_timeline(
        &runtime,
        DashboardTimelineRequest {
            record_types: DashboardFilter::Values(std::collections::BTreeSet::from([
                "event".to_string()
            ])),
            categories: DashboardFilter::All,
            job_kinds: DashboardFilter::None,
            metadata: "none".to_string(),
            ..DashboardTimelineRequest::default()
        },
    )
    .await
    .unwrap();
    let events = timeline["records"]
        .as_array()
        .unwrap()
        .iter()
        .map(|record| &record["event"])
        .collect::<Vec<_>>();

    assert!(summary.get("byRoom").is_none());
    assert!(scopes.iter().any(|scope| {
        scope["scope_kind"] == "voice_channel"
            && scope["guild_id"] == "guild"
            && scope["scope_id"] == "voice"
            && scope["total"] == 1
    }));
    assert!(scopes.iter().any(|scope| {
        scope["scope_kind"] == "dm" && scope["scope_id"] == "user" && scope["failed"] == 1
    }));
    assert!(events.iter().any(|event| {
        event["kind"] == "job_created"
            && event["scope_kind"] == "dm"
            && event["scope_id"] == "user"
            && event.get("voice_channel_id").is_none()
    }));
}

#[tokio::test(flavor = "current_thread")]
async fn stale_voice_rows_are_separated_from_current_dashboard_status() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(raw.path()).await;
    store
        .upsert_voice_bot_state(&VoiceBotStatus {
            bot_id: "voice-1".to_string(),
            ready: true,
            gateway_running: true,
            ..VoiceBotStatus::default()
        })
        .await
        .unwrap();
    store
        .upsert_capture_session_status(&VoiceCaptureSessionStatus {
            session_id: "session-1".to_string(),
            guild_id: "guild".to_string(),
            voice_channel_id: "code".to_string(),
            bot_id: "voice-1".to_string(),
            active: true,
            ..VoiceCaptureSessionStatus::default()
        })
        .await
        .unwrap();
    store
        .record_voice_adapter_snapshot(1, 1, 1, 1)
        .await
        .unwrap();
    let stale_at_ms = (Utc::now() - Duration::minutes(2)).timestamp_millis();
    sqlx::query("UPDATE bot_states SET updated_at_ms = $1 WHERE bot_id = 'voice-1'")
        .bind(stale_at_ms)
        .execute(&store.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE capture_sessions SET updated_at_ms = $1 WHERE session_id = 'session-1'")
        .bind(stale_at_ms)
        .execute(&store.pool)
        .await
        .unwrap();
    let runtime = Ctx::new(store);

    let rooms = clankcord::views::operations::dashboard_rooms_payload(&runtime)
        .await
        .unwrap();
    let overview = clankcord::views::operations::dashboard_summary_payload(&runtime)
        .await
        .unwrap();

    assert!(rooms["status"]["bots"].as_array().unwrap().is_empty());
    assert!(rooms["status"]["sessions"].as_array().unwrap().is_empty());
    assert_eq!(
        rooms["status"]["staleObservations"]["bots"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        rooms["status"]["staleObservations"]["sessions"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(rooms["status"]["pool"]["observedBots"], json!(0));
    let components = overview["health"]["components"].as_array().unwrap();
    let voice = components
        .iter()
        .find(|component| component["component"] == "voice_gateway")
        .unwrap();
    let capture = components
        .iter()
        .find(|component| component["component"] == "capture")
        .unwrap();
    assert_eq!(voice["status"], json!("down"));
    assert_eq!(capture["status"], json!("stale"));
}
