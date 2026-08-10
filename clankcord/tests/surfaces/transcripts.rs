//! Rendered transcript content: speech-event rendering, search, and window boundaries.

use crate::support::append_speech;
use crate::support::dt;
use crate::support::test_store;
use crate::support::voice::string_field;
use clankcord::store::CaptureRunInput;

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
