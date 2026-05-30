use std::collections::BTreeSet;

use serde_json::json;

mod common;

use clankcord::runtime::timeline::{CaptureRunInput, SpeechEventInput};
use clankcord::runtime::{CommandRequest, Job, JobState, RuntimeScope};

use common::{append_speech, dt, test_store};

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
    assert!(rendered.content.contains("draft fixed piont"));
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

#[tokio::test(flavor = "current_thread")]
async fn retention_sweep_dry_run_and_idempotence() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    let start = dt(2026, 4, 1, 16, 0, 0);
    let run = store
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
    let capture_run_id = string_field(&run, "capture_run_id");
    let source = store
        .capture_run_scratch_dir("guild", "code", start, &capture_run_id)
        .join("segments")
        .join("speaker-user-a")
        .join("old-source.wav");
    std::fs::create_dir_all(source.parent().unwrap()).unwrap();
    std::fs::write(&source, b"audio").unwrap();
    store
        .append_speech_event(SpeechEventInput {
            guild_id: "guild".to_string(),
            guild_slug: "guild".to_string(),
            voice_channel_id: "code".to_string(),
            voice_channel_name: "Code Lounge".to_string(),
            voice_channel_slug: "code-lounge".to_string(),
            capture_run_id: capture_run_id.clone(),
            voice_bot_id: "clanky-vc1".to_string(),
            voice_bot_discord_user_id: "bot-user".to_string(),
            speaker_user_id: "user-a".to_string(),
            speaker_label: "Will".to_string(),
            speaker_username: "will".to_string(),
            segment_start_time: start,
            segment_end_time: start + chrono::Duration::seconds(2),
            text_draft: "old draft words".to_string(),
            source_audio_path: source.clone(),
            audio_checksum: "sha256:test".to_string(),
            segment_index: 1,
            duration_ms: 2000,
            ..Default::default()
        })
        .await
        .unwrap();
    let dry = store
        .retention_sweep(Some(start + chrono::Duration::days(8)), true)
        .await
        .unwrap();
    assert_eq!(dry["transcript_event_candidates"], json!(0));
    assert_eq!(dry["source_audio_candidates"], json!(1));
    assert_eq!(dry["job_candidates"], json!(0));
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
    assert_eq!(first["transcript_event_candidates"], json!(0));
    assert_eq!(first["source_audio_candidates"], json!(1));
    assert_eq!(first["deleted_audio"], json!(1));
    assert!(!source.exists());
    assert_eq!(
        store
            .load_events("guild", "code", None, None, Some(&kinds), None, false)
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(second["transcript_event_candidates"], json!(0));
    assert_eq!(second["source_audio_candidates"], json!(0));
}

#[tokio::test(flavor = "current_thread")]
async fn retention_sweep_respects_transcript_and_source_audio_policy() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    let start = dt(2026, 4, 1, 16, 0, 0);
    let run = store
        .create_capture_run(CaptureRunInput {
            guild_id: "guild".to_string(),
            guild_slug: "guild".to_string(),
            voice_channel_id: "code".to_string(),
            voice_channel_name: "Code Lounge".to_string(),
            voice_channel_slug: "code-lounge".to_string(),
            voice_bot_id: "clanky-vc1".to_string(),
            voice_bot_discord_user_id: "bot-user".to_string(),
            started_at: Some(start),
            retention_policy: Some(json!({
                "transcript_events": "7d",
                "source_audio": "forever",
                "job_metadata": "1d"
            })),
            ..Default::default()
        })
        .await
        .unwrap();
    let capture_run_id = string_field(&run, "capture_run_id");
    let source = store
        .capture_run_scratch_dir("guild", "code", start, &capture_run_id)
        .join("segments")
        .join("speaker-user-a")
        .join("retained-source.wav");
    std::fs::create_dir_all(source.parent().unwrap()).unwrap();
    std::fs::write(&source, b"audio").unwrap();
    store
        .append_speech_event(SpeechEventInput {
            guild_id: "guild".to_string(),
            guild_slug: "guild".to_string(),
            voice_channel_id: "code".to_string(),
            voice_channel_name: "Code Lounge".to_string(),
            voice_channel_slug: "code-lounge".to_string(),
            capture_run_id,
            voice_bot_id: "clanky-vc1".to_string(),
            voice_bot_discord_user_id: "bot-user".to_string(),
            speaker_user_id: "user-a".to_string(),
            speaker_label: "Will".to_string(),
            speaker_username: "will".to_string(),
            segment_start_time: start,
            segment_end_time: start + chrono::Duration::seconds(2),
            text_draft: "old policy words".to_string(),
            source_audio_path: source.clone(),
            audio_checksum: "sha256:test".to_string(),
            segment_index: 1,
            duration_ms: 2000,
            ..Default::default()
        })
        .await
        .unwrap();
    let mut job = Job::agent_task_for_session(
        "ags_retention",
        RuntimeScope::voice_channel("guild", "code"),
        "user-a",
        CommandRequest::agent_task("guild", "code", "user-a", "summarize"),
    );
    let started = start.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    job.created_at = started.clone();
    job.updated_at = started.clone();
    job.completed_at = Some(started);
    job.state = JobState::Complete;
    let job_id = job.id.clone();
    store.create_job(job).await.unwrap();

    let result = store
        .retention_sweep(Some(start + chrono::Duration::days(8)), false)
        .await
        .unwrap();

    assert_eq!(result["transcript_event_candidates"], json!(1));
    assert_eq!(result["forgotten_events"], json!(1));
    assert_eq!(result["source_audio_candidates"], json!(0));
    assert_eq!(result["job_candidates"], json!(1));
    assert_eq!(result["deleted_jobs"], json!(1));
    assert!(source.exists());
    let row = sqlx::query("SELECT COUNT(*) AS count FROM jobs WHERE job_id = $1")
        .bind(&job_id)
        .fetch_one(&store.pool)
        .await
        .unwrap();
    assert_eq!(sqlx::Row::try_get::<i64, _>(&row, "count").unwrap(), 0);
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
async fn retention_sweep_retires_untranscribed_wake_probe_audio_from_capture_directory() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    let start = dt(2026, 4, 1, 16, 0, 0);
    let run = store
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
    let capture_run_id = string_field(&run, "capture_run_id");
    let source = store
        .capture_run_scratch_dir("guild", "code", start, &capture_run_id)
        .join("wake-probes")
        .join("speaker-user-a")
        .join("no-wake.wav");
    std::fs::create_dir_all(source.parent().unwrap()).unwrap();
    std::fs::write(&source, b"audio").unwrap();

    let result = store
        .retention_sweep(Some(start + chrono::Duration::days(8)), false)
        .await
        .unwrap();

    assert_eq!(result["source_audio_candidates"], json!(1));
    assert_eq!(result["deleted_audio"], json!(1));
    assert!(!source.exists());
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
