use chrono::{Duration, Utc};
use serde_json::json;

mod common;

use clankcord::runtime::timeline::SpeechEventInput;
use clankcord::runtime::{
    CommandRequest, DebugOverviewRequest, Job, JobState, Runtime, RuntimeScope,
};

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
async fn dashboard_transcript_channel_filter_applies_before_limit() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(raw.path()).await;
    let base = Utc::now() - Duration::minutes(30);
    append_dashboard_speech(
        &store,
        raw.path(),
        "code",
        "Code Lounge",
        "code-lounge",
        base,
        "needle code transcript",
        1,
    )
    .await;
    for index in 0..15 {
        append_dashboard_speech(
            &store,
            raw.path(),
            "art",
            "Art Lounge",
            "art-lounge",
            base + Duration::minutes(index + 1),
            "newer art transcript",
            index + 2,
        )
        .await;
    }
    let runtime = Runtime::from_store(store).unwrap();

    let overview = runtime
        .debug_overview(DebugOverviewRequest {
            transcript_limit: 10,
            transcript_channel: "code".to_string(),
            transcript_query: "needle".to_string(),
            ..DebugOverviewRequest::default()
        })
        .await
        .unwrap();
    let events = overview["transcript"]["events"].as_array().unwrap();

    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["voice_channel_id"], json!("code"));
    assert_eq!(events[0]["text"], json!("needle code transcript"));
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
            clankcord::runtime::JobPayload::Command(clankcord::runtime::CommandPayload {
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
            clankcord::runtime::JobPayload::Command(clankcord::runtime::CommandPayload {
                command: CommandRequest::agent_task("", "user", "system", "dm"),
            }),
        ))
        .await
        .unwrap();
    let runtime = Runtime::from_store(store).unwrap();

    let overview = runtime
        .debug_overview(DebugOverviewRequest::default())
        .await
        .unwrap();
    let summary = &overview["jobs"]["summary"];
    let scopes = summary["byScope"].as_array().unwrap();
    let events = overview["timeline"]["recentEvents"].as_array().unwrap();

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

async fn append_dashboard_speech(
    store: &clankcord::runtime::timeline::TimelineStore,
    raw_root: &std::path::Path,
    voice_channel_id: &str,
    voice_channel_name: &str,
    voice_channel_slug: &str,
    start: chrono::DateTime<Utc>,
    text: &str,
    segment_index: i64,
) {
    store
        .append_speech_event(SpeechEventInput {
            guild_id: "guild".to_string(),
            guild_slug: "guild".to_string(),
            voice_channel_id: voice_channel_id.to_string(),
            voice_channel_name: voice_channel_name.to_string(),
            voice_channel_slug: voice_channel_slug.to_string(),
            capture_run_id: format!("cap_{voice_channel_id}"),
            voice_bot_id: "clanky-vc1".to_string(),
            voice_bot_discord_user_id: "bot-user".to_string(),
            speaker_user_id: "user-a".to_string(),
            speaker_label: "Will".to_string(),
            speaker_username: "will".to_string(),
            segment_start_time: start,
            segment_end_time: start + Duration::seconds(1),
            text_draft: text.to_string(),
            source_audio_path: raw_root.join(format!("dashboard-{segment_index}.wav")),
            audio_checksum: "sha256:test".to_string(),
            segment_index,
            duration_ms: 1000,
            ..Default::default()
        })
        .await
        .unwrap();
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
          job_id, scope_kind, guild_id, scope_id, kind, state, terminal, failed,
          ephemeral, cancellable, lane, ordering_key, ready_at_ms, created_at_ms,
          updated_at_ms, started_at_ms, completed_at_ms
        )
        VALUES
          ('job_latency_clean', 'voice_channel', 'guild', 'code', 'wake_activation', 'complete', TRUE, FALSE,
           FALSE, FALSE, 'voice_control', 'latency-test', $1, $2, $5, $3, $4),
          ('job_latency_phase', 'voice_channel', 'guild', 'code', 'wake_activation', 'complete', TRUE, FALSE,
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
