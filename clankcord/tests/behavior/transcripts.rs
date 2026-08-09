//! Transcript rendering, search, windowing, and payload compaction over speech events.

use std::collections::BTreeSet;

use serde_json::json;

use clankcord::store::CaptureRunInput;

use crate::support::{append_speech, dt, test_store};

fn string_field(value: &serde_json::Value, key: &str) -> String {
    match value.get(key) {
        Some(serde_json::Value::String(text)) => text.trim().to_string(),
        Some(serde_json::Value::Number(number)) => number.to_string(),
        Some(serde_json::Value::Bool(boolean)) => boolean.to_string(),
        _ => String::new(),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn transcript_render_and_search_use_speech_events() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    let start = dt(2026, 5, 12, 16, 0, 0);
    let end = start + chrono::Duration::seconds(4);
    store
        .create_capture_run(CaptureRunInput {
            guild_id: "guild".to_string(),
            guild_slug: "guild".to_string(),
            voice_channel_id: "code".to_string(),
            voice_channel_name: "Code Lounge".to_string(),
            voice_channel_slug: "code-lounge".to_string(),
            voice_bot_id: "clanky-vc1".to_string(),
            voice_bot_discord_user_id: "bot-user".to_string(),
            started_at: Some(start),
            ..Default::default()
        })
        .await
        .unwrap();
    append_speech(
        &store,
        raw.path(),
        start,
        end,
        "draft fixed piont words",
        1,
        None,
    )
    .await;
    let _materialized = store
        .materialize(
            "guild",
            "code",
            start,
            end,
            "relative_time",
            "-10m",
            "",
            "local",
            false,
            None,
        )
        .await
        .unwrap();
    let rendered = store
        .render_transcript("guild", "code", start, end, "", "markdown")
        .await
        .unwrap();
    assert!(rendered.content.contains("# Transcript"));
    assert!(rendered.content.contains("guild_id: guild"));
    assert!(rendered.content.contains("voice_channel_id: code"));
    assert!(rendered.content.contains("event_count: 1"));
    assert!(rendered.content.contains("first_event_id: evt_"));
    assert!(rendered.content.contains("last_event_id: evt_"));
    assert!(rendered.content.contains("participants:\n- user-a: Will"));
    assert!(rendered.content.contains("## Conversation"));
    assert!(rendered.content.contains("draft fixed piont"));
    let error = store
        .render_transcript("guild", "code", start, end, "", "yaml")
        .await
        .expect_err("unsupported transcript format is rejected");
    assert!(
        error
            .to_string()
            .contains("transcript render format must be json or markdown")
    );
    assert_eq!(
        string_field(
            &store
                .search("guild", Some("code"), "fixed piont", None, 10)
                .await
                .unwrap()[0],
            "kind"
        ),
        "speech_segment"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn timeline_finds_existing_speech_segment_for_audio_retry() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    let start = dt(2026, 5, 12, 16, 0, 0);
    let event = append_speech(
        &store,
        raw.path(),
        start,
        start + chrono::Duration::seconds(2),
        "retry-safe words",
        4,
        None,
    )
    .await;
    let found = store
        .speech_event_for_segment("guild", "code", "cap_test", "user-a", 4)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found["event_id"], event["event_id"]);
    let (count, last) = store
        .speech_stats_for_capture_run("guild", "code", "cap_test")
        .await
        .unwrap();
    assert_eq!(count, 1);
    assert_eq!(last.unwrap(), start + chrono::Duration::seconds(2));
}

#[tokio::test(flavor = "current_thread")]
async fn timeline_primary_store_keeps_payload_compact() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    let start = dt(2026, 5, 12, 16, 0, 0);
    append_speech(
        &store,
        raw.path(),
        start,
        start + chrono::Duration::seconds(1),
        "postgres indexed compact words",
        1,
        None,
    )
    .await;
    assert!(
        !raw.path()
            .join("ephemeral/guild-guild/channel-code/timeline.jsonl")
            .exists()
    );
    assert_eq!(
        string_field(
            &store
                .search("guild", Some("code"), "indexed", None, 10)
                .await
                .unwrap()[0],
            "kind"
        ),
        "speech_segment"
    );

    let payload_json: serde_json::Value = sqlx::query_scalar(
        "SELECT payload_json FROM timeline_events WHERE event_kind = 'speech_segment'",
    )
    .fetch_one(&store.pool)
    .await
    .unwrap();
    let payload = payload_json;
    assert!(payload.get("text").is_none());
    assert!(payload.get("text_draft").is_none());
    assert!(payload.get("guildId").is_none());
    assert!(payload.get("channelId").is_none());
    assert!(payload.get("speakerLabel").is_none());
    let kinds = BTreeSet::from(["speech_segment".to_string()]);
    let event = store
        .load_events("guild", "code", None, None, Some(&kinds), None, false)
        .await
        .unwrap()[0]
        .clone();
    assert_eq!(event["text"], json!("postgres indexed compact words"));
    assert_eq!(event["channelName"], json!("Code Lounge"));
    assert_eq!(event["speakerLabel"], json!("Will"));
}

#[tokio::test(flavor = "current_thread")]
async fn window_end_boundary_excludes_next_segment() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    let start = dt(2026, 5, 12, 16, 0, 0);
    let window_end = start + chrono::Duration::seconds(10);
    let inside = append_speech(
        &store,
        raw.path(),
        window_end - chrono::Duration::seconds(1),
        window_end,
        "inside final words",
        1,
        None,
    )
    .await;
    append_speech(
        &store,
        raw.path(),
        window_end,
        window_end + chrono::Duration::seconds(1),
        "outside next words",
        2,
        None,
    )
    .await;
    let window = store
        .create_window(
            "guild",
            "code",
            start,
            window_end,
            "absolute",
            "2026-05-12T16:00:00Z/2026-05-12T16:00:10Z",
            "single_channel",
        )
        .await
        .unwrap();
    let rendered = store
        .render_transcript("guild", "code", start, window_end, "", "markdown")
        .await
        .unwrap();
    assert_eq!(window["event_id_end"], inside["event_id"]);
    assert!(rendered.content.contains("inside final words"));
    assert!(!rendered.content.contains("outside next words"));
}
