use std::collections::BTreeSet;

use serde_json::json;

mod common;

use clankcord::runtime::timeline::{CaptureRunInput, string_field};

use common::{append_speech, dt, test_store};

#[tokio::test(flavor = "current_thread")]
async fn refined_span_overlays_draft_render_and_search() {
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
    let materialized = store
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
            false,
            true,
            None,
        )
        .await
        .unwrap();
    let pub_id = string_field(&materialized["publication"], "publication_id");
    let refined_path = store
        .durable_publications_dir()
        .join(&pub_id)
        .join("transcript.refined.txt");
    std::fs::write(&refined_path, "Will: refined fixed point words\n").unwrap();
    let alignment_path = store
        .durable_publications_dir()
        .join(&pub_id)
        .join("speaker_alignment.json");
    std::fs::write(&alignment_path, "{}\n").unwrap();
    store
        .create_authoritative_span(
            "guild",
            "code",
            &string_field(&materialized["window"], "window_id"),
            &pub_id,
            "elevenlabs",
            start,
            end,
            &refined_path,
            &alignment_path,
            vec!["cap_test".to_string()],
            vec!["clanky-vc1".to_string()],
        )
        .await
        .unwrap();

    let rendered = store
        .render_transcript("guild", "code", start, end, "", true, "markdown")
        .await
        .unwrap();
    assert!(rendered.content.contains("refined fixed point"));
    assert!(!rendered.content.contains("draft fixed piont"));
    assert_eq!(
        string_field(
            &store
                .search("guild", Some("code"), "fixed point", None, true, 10)
                .await
                .unwrap()[0],
            "kind"
        ),
        "refined_span"
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
async fn voice_state_updates_are_durable_and_emit_participant_events() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;

    let joined = voice_state("code", "user-a", "Will", false, false);
    let events = store
        .record_voice_state_update(None, joined.clone())
        .await
        .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(string_field(&events[0], "event_kind"), "participant_joined");
    assert_eq!(string_field(&events[0], "user_id"), "user-a");

    let occupants = store.room_occupants("guild", "code").await.unwrap();
    assert_eq!(occupants.len(), 1);
    assert_eq!(string_field(&occupants[0], "display_name"), "Will");
    let snapshot = store.voice_occupancy_snapshot().await.unwrap();
    assert_eq!(snapshot["rooms"][0]["occupants"][0]["user_id"], "user-a");

    let muted = voice_state("code", "user-a", "Will", true, false);
    let events = store
        .record_voice_state_update(Some(joined), muted.clone())
        .await
        .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(
        string_field(&events[0], "event_kind"),
        "participant_mute_changed"
    );
    assert_eq!(events[0]["current"], true);

    let duplicate = store.record_voice_state_update(None, muted).await.unwrap();
    assert!(duplicate.is_empty());

    let left = voice_state("", "user-a", "Will", false, false);
    let events = store.record_voice_state_update(None, left).await.unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(string_field(&events[0], "event_kind"), "participant_left");
    assert!(
        store
            .room_occupants("guild", "code")
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn forget_tombstone_filters_unpublished_speech() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    let start = dt(2026, 5, 12, 16, 0, 0);
    let source = raw.path().join("source.wav");
    std::fs::write(&source, b"audio").unwrap();
    append_speech(
        &store,
        raw.path(),
        start,
        start + chrono::Duration::seconds(2),
        "forget this",
        1,
        Some(source.clone()),
    )
    .await;
    let result = store
        .apply_forget(
            "guild",
            "code",
            start,
            start + chrono::Duration::seconds(3),
            "",
            true,
        )
        .await
        .unwrap();
    assert_eq!(result["forgotten_event_count"], json!(1));
    assert!(!source.exists());
    let kinds = BTreeSet::from(["speech_segment".to_string()]);
    assert!(
        store
            .load_events("guild", "code", None, None, Some(&kinds), None, false)
            .await
            .unwrap()
            .is_empty()
    );
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
                .search("guild", Some("code"), "indexed", None, true, 10)
                .await
                .unwrap()[0],
            "kind"
        ),
        "draft_event"
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
        .render_transcript("guild", "code", start, window_end, "", false, "markdown")
        .await
        .unwrap();
    assert_eq!(window["event_id_end"], inside["event_id"]);
    assert!(rendered.content.contains("inside final words"));
    assert!(!rendered.content.contains("outside next words"));
}

#[tokio::test(flavor = "current_thread")]
async fn retention_sweep_dry_run_and_idempotence() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    let start = dt(2026, 4, 1, 16, 0, 0);
    let source = raw.path().join("old-source.wav");
    std::fs::write(&source, b"audio").unwrap();
    append_speech(
        &store,
        raw.path(),
        start,
        start + chrono::Duration::seconds(2),
        "old draft words",
        1,
        Some(source.clone()),
    )
    .await;
    let dry = store
        .retention_sweep(Some(start + chrono::Duration::days(8)), true)
        .await
        .unwrap();
    assert_eq!(dry["retired_events"], json!(1));
    assert!(source.exists());
    let kinds = BTreeSet::from(["speech_segment".to_string()]);
    assert_eq!(
        store
            .load_events("guild", "code", None, None, Some(&kinds), None, false)
            .await
            .unwrap()
            .len(),
        1
    );

    let first = store
        .retention_sweep(Some(start + chrono::Duration::days(8)), false)
        .await
        .unwrap();
    let second = store
        .retention_sweep(Some(start + chrono::Duration::days(8)), false)
        .await
        .unwrap();
    assert_eq!(first["retired_events"], json!(1));
    assert!(!source.exists());
    assert!(
        store
            .load_events("guild", "code", None, None, Some(&kinds), None, false)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(second["retired_events"], json!(0));
}

fn voice_state(
    voice_channel_id: &str,
    user_id: &str,
    display_name: &str,
    self_mute: bool,
    self_deaf: bool,
) -> serde_json::Value {
    json!({
        "guild_id": "guild",
        "voice_channel_id": voice_channel_id,
        "user_id": user_id,
        "display_name": display_name,
        "member_display_name": display_name,
        "username": display_name.to_lowercase(),
        "mute": false,
        "deaf": false,
        "self_mute": self_mute,
        "self_deaf": self_deaf,
        "self_stream": false,
        "self_video": false,
        "suppress": false,
    })
}
