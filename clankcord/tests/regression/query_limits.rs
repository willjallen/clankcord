//! Pins for dashboard query fixes: filters and search apply before row
//! limits, and latency stats exclude phase-contaminated intervals
//! (dcc87d7, 46b3341, 6b504e2).

use crate::support::dashboard::append_dashboard_speech;
use crate::support::initialize_test_config;
use crate::support::test_store;
use chrono::Duration;
use chrono::Utc;
use clankcord::domain::Ctx;
use clankcord::views::DashboardFilter;
use clankcord::views::DashboardTimelineRequest;
use clankcord::views::DashboardTranscriptRequest;
use serde_json::json;

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
    let runtime = Ctx::new(store);

    let overview = clankcord::views::dashboard::dashboard_transcript(
        &runtime,
        DashboardTranscriptRequest {
            limit: 10,
            channel: "code".to_string(),
            search: "needle".to_string(),
            ..DashboardTranscriptRequest::default()
        },
    )
    .await
    .unwrap();
    let events = overview["transcript"]["events"].as_array().unwrap();

    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["voice_channel_id"], json!("code"));
    assert_eq!(events[0]["text"], json!("needle code transcript"));
}

#[tokio::test(flavor = "current_thread")]
async fn dashboard_timeline_limit_returns_newest_events_across_scopes() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(raw.path()).await;
    let base = Utc::now() - Duration::minutes(30);
    for index in 0..20 {
        let (channel_id, channel_name, channel_slug) = if index % 2 == 0 {
            ("code", "Code Lounge", "code-lounge")
        } else {
            ("art", "Art Lounge", "art-lounge")
        };
        append_dashboard_speech(
            &store,
            raw.path(),
            channel_id,
            channel_name,
            channel_slug,
            base + Duration::minutes(index),
            &format!("bounded timeline event {index}"),
            index + 1,
        )
        .await;
    }
    let runtime = Ctx::new(store);

    let overview = clankcord::views::dashboard::dashboard_timeline(
        &runtime,
        DashboardTimelineRequest {
            record_types: DashboardFilter::Values(std::collections::BTreeSet::from([
                "event".to_string()
            ])),
            categories: DashboardFilter::All,
            job_kinds: DashboardFilter::None,
            metadata: "none".to_string(),
            limit: 10,
            ..DashboardTimelineRequest::default()
        },
    )
    .await
    .unwrap();
    let events = overview["records"]
        .as_array()
        .unwrap()
        .iter()
        .map(|record| &record["event"])
        .collect::<Vec<_>>();

    assert_eq!(events.len(), 10);
    assert_eq!(events[0]["text"], json!("bounded timeline event 19"));
    assert_eq!(events[9]["text"], json!("bounded timeline event 10"));
    assert!(
        events
            .iter()
            .all(|event| event["text"] != json!("bounded timeline event 9"))
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dashboard_timeline_search_applies_before_limit() {
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
        "needle timeline event",
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
            "newer unrelated timeline event",
            index + 2,
        )
        .await;
    }
    let runtime = Ctx::new(store);

    let overview = clankcord::views::dashboard::dashboard_timeline(
        &runtime,
        DashboardTimelineRequest {
            record_types: DashboardFilter::Values(std::collections::BTreeSet::from([
                "event".to_string()
            ])),
            categories: DashboardFilter::All,
            job_kinds: DashboardFilter::None,
            search: "needle".to_string(),
            search_field: "detail".to_string(),
            metadata: "none".to_string(),
            limit: 10,
            ..DashboardTimelineRequest::default()
        },
    )
    .await
    .unwrap();
    let events = overview["records"]
        .as_array()
        .unwrap()
        .iter()
        .map(|record| &record["event"])
        .collect::<Vec<_>>();

    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["voice_channel_id"], json!("code"));
    assert_eq!(events[0]["text"], json!("needle timeline event"));
}

#[tokio::test(flavor = "current_thread")]
async fn dashboard_latency_stats_exclude_phase_contaminated_intervals() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(raw.path()).await;
    let base_ms = Utc::now().timestamp_millis() - 30_000;

    sqlx::query(
        r#"
        INSERT INTO operational_job_outcomes(
          job_id, scope_kind, guild_id, scope_id, kind, state, failed,
          lane, ready_at_ms, created_at_ms, observed_at_ms, started_at_ms,
          completed_at_ms, error_text
        )
        VALUES
          ('job_latency_clean', 'voice_channel', 'guild', 'code', 'wake_activation', 'complete', FALSE,
           'voice_control', $1, $2, $5, $3, $4, ''),
          ('job_latency_phase', 'voice_channel', 'guild', 'code', 'wake_activation', 'complete', FALSE,
           'voice_control', $8, $6, $10, $7, $9, '')
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

    let runtime = Ctx::new(store);
    let overview =
        clankcord::views::health::dashboard_health_payload(&runtime, json!({}), json!({}))
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
