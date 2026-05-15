use std::collections::BTreeMap;

use chrono::{SecondsFormat, Utc};
use serde_json::{Value, json};

mod common;

use clankcord::runtime::domain::wake_activations::{execute, schedule_from_wake_event};
use clankcord::runtime::timeline::{SpeechEventInput, TimelineStore, string_field};
use clankcord::runtime::{
    AgentRuntime, AudioSegmentPayload, ControlConfig, DiscordVoicePlaybackCue, Job, JobKind,
    JobState, Runtime, RuntimeSessionStatus, SessionCaptureStats, SessionSpeakerCaptureStats,
};

use common::{dt, test_store};

#[tokio::test(flavor = "current_thread")]
async fn wake_activation_builds_labeled_bundle_before_dispatch() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    let mut runtime = test_runtime(store);
    let start = dt(2026, 5, 12, 16, 0, 0);
    append_event(
        &runtime.timeline_store,
        start - chrono::Duration::seconds(20),
        start - chrono::Duration::seconds(18),
        "Vince",
        "user-b",
        "floating point rounding came up",
        json!({}),
        1,
    )
    .await;
    let wake = append_event(
        &runtime.timeline_store,
        start,
        start + chrono::Duration::seconds(1),
        "Will",
        "user-a",
        "Hey Clanky",
        json!({"wake": true, "score": 0.88}),
        2,
    )
    .await;
    let post = append_event(
        &runtime.timeline_store,
        start + chrono::Duration::seconds(3),
        start + chrono::Duration::seconds(4),
        "Will",
        "user-a",
        "summarize what Vince said about floats",
        json!({}),
        3,
    )
    .await;

    let scheduled = schedule_from_wake_event(&runtime, &wake).await.unwrap();
    let activation_job_id = string_field(&scheduled["job"], "job_id");
    let activation_job = runtime
        .timeline_store
        .get_job(&activation_job_id)
        .await
        .unwrap();
    let payload = activation_job
        .wake_activation_payload()
        .cloned()
        .unwrap_or_else(|| panic!("missing wake activation payload"));
    let result = execute(&mut runtime, &activation_job, &payload)
        .await
        .unwrap();

    assert_eq!(result["status"], json!("dispatched"));
    let command_job_id = string_field(&result["created"]["job"], "job_id");
    let command = runtime
        .timeline_store
        .get_job(&command_job_id)
        .await
        .unwrap();
    assert_eq!(command.kind, JobKind::Command);
    let command_value = command.command_value().unwrap();
    assert_eq!(command_value["command_kind"], json!("agent_task"));
    assert_eq!(
        command_value["arguments"]["request"],
        json!("summarize what Vince said about floats")
    );
    assert!(
        command_value["arguments"]["activation"]["prior_to_activation"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event
                .to_string()
                .contains("floating point rounding came up"))
    );
    assert_eq!(
        command_value["arguments"]["activation"]["post_activation_turn"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()["event_id"],
        post["event_id"]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn wake_activation_uses_speech_segment_that_overlaps_probe_event() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    let mut runtime = test_runtime(store);
    let start = dt(2026, 5, 12, 16, 0, 0);
    let wake = runtime
        .timeline_store
        .append_event(
            "guild",
            "code",
            json!({
                "event_kind": "wake_detected",
                "kind": "wake_detected",
                "capture_run_id": "cap_test",
                "speaker_user_id": "user-a",
                "speakerId": "user-a",
                "speaker_label": "Will",
                "speakerLabel": "Will",
                "startedAt": (start + chrono::Duration::seconds(1)).to_rfc3339_opts(SecondsFormat::Millis, true),
                "endedAt": (start + chrono::Duration::milliseconds(1300)).to_rfc3339_opts(SecondsFormat::Millis, true),
                "wake": {"wake": true, "score": 0.91},
                "wake_detected": true,
            }),
        ).await.unwrap();
    append_event(
        &runtime.timeline_store,
        start,
        start + chrono::Duration::seconds(3),
        "Will",
        "user-a",
        "hey clanky summarize the floating point discussion",
        json!({}),
        1,
    )
    .await;

    let scheduled = schedule_from_wake_event(&runtime, &wake).await.unwrap();
    let activation_job_id = string_field(&scheduled["job"], "job_id");
    let activation_job = runtime
        .timeline_store
        .get_job(&activation_job_id)
        .await
        .unwrap();
    let payload = activation_job.wake_activation_payload().cloned().unwrap();
    let result = execute(&mut runtime, &activation_job, &payload)
        .await
        .unwrap();

    assert_eq!(result["status"], json!("dispatched"));
    let command_job_id = string_field(&result["created"]["job"], "job_id");
    let command = runtime
        .timeline_store
        .get_job(&command_job_id)
        .await
        .unwrap();
    let command_value = command.command_value().unwrap();
    assert_eq!(
        command_value["arguments"]["request"],
        json!("summarize the floating point discussion")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn wake_followup_before_execution_amends_existing_activation() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    let runtime = test_runtime(store);
    let start = dt(2026, 5, 12, 16, 0, 0);
    let first = append_event(
        &runtime.timeline_store,
        start,
        start + chrono::Duration::seconds(1),
        "Will",
        "user-a",
        "Hey Clanky",
        json!({"wake": true}),
        1,
    )
    .await;
    let second = append_event(
        &runtime.timeline_store,
        start + chrono::Duration::seconds(20),
        start + chrono::Duration::seconds(21),
        "Will",
        "user-a",
        "Hey Clanky actually include Vince too",
        json!({"wake": true}),
        2,
    )
    .await;

    let scheduled = schedule_from_wake_event(&runtime, &first).await.unwrap();
    let activation_job_id = string_field(&scheduled["job"], "job_id");
    let amended = schedule_from_wake_event(&runtime, &second).await.unwrap();

    assert_eq!(amended["status"], json!("amended"));
    assert_eq!(string_field(&amended["job"], "job_id"), activation_job_id);
    let activation = runtime
        .timeline_store
        .get_job(&activation_job_id)
        .await
        .unwrap();
    let payload = activation.wake_activation_payload().unwrap();
    assert_eq!(
        payload.latest_wake_event_id,
        string_field(&second, "event_id")
    );
    assert!(
        payload
            .amended_wake_event_ids
            .contains(&string_field(&second, "event_id"))
    );
}

#[tokio::test(flavor = "current_thread")]
async fn wake_activation_schedules_voice_cue_jobs_for_wake_and_preempt() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    let mut runtime = test_runtime(store);
    runtime.sessions.insert(
        "cap_test".to_string(),
        RuntimeSessionStatus {
            session_id: "cap_test".to_string(),
            guild_id: "guild".to_string(),
            channel_id: "code".to_string(),
            voice_channel_id: "code".to_string(),
            active: true,
            ..RuntimeSessionStatus::default()
        },
    );
    let start = dt(2026, 5, 12, 16, 0, 0);
    let first = append_event(
        &runtime.timeline_store,
        start,
        start + chrono::Duration::seconds(1),
        "Will",
        "user-a",
        "Hey Clanky",
        json!({"wake": true}),
        1,
    )
    .await;
    let second = append_event(
        &runtime.timeline_store,
        start + chrono::Duration::seconds(8),
        start + chrono::Duration::seconds(9),
        "Will",
        "user-a",
        "Hey Clanky add one more thing",
        json!({"wake": true}),
        2,
    )
    .await;

    schedule_from_wake_event(&runtime, &first).await.unwrap();
    schedule_from_wake_event(&runtime, &second).await.unwrap();

    let cues = runtime
        .timeline_store
        .list_jobs(Some("guild"), None)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|job| {
            job.discord_voice_playback_payload()
                .map(|payload| payload.cue)
        })
        .collect::<Vec<_>>();
    assert!(cues.contains(&DiscordVoicePlaybackCue::Wake));
    assert!(cues.contains(&DiscordVoicePlaybackCue::Preempt));
}

#[tokio::test(flavor = "current_thread")]
async fn wake_activation_waits_for_live_activating_speaker_audio() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    let mut runtime = test_runtime(store);
    let now = Utc::now();
    let start = now - chrono::Duration::seconds(20);
    let wake = append_event(
        &runtime.timeline_store,
        start,
        start + chrono::Duration::seconds(2),
        "Will",
        "user-a",
        "Hey Clanky are you working",
        json!({"wake": true}),
        1,
    )
    .await;
    let scheduled = schedule_from_wake_event(&runtime, &wake).await.unwrap();
    let activation_job_id = string_field(&scheduled["job"], "job_id");
    let activation_job = runtime
        .timeline_store
        .get_job(&activation_job_id)
        .await
        .unwrap();
    let payload = activation_job.wake_activation_payload().cloned().unwrap();
    runtime.sessions.insert(
        "cap_test".to_string(),
        RuntimeSessionStatus {
            session_id: "cap_test".to_string(),
            guild_id: "guild".to_string(),
            channel_id: "code".to_string(),
            voice_channel_id: "code".to_string(),
            active: true,
            capture_stats: SessionCaptureStats {
                speakers: BTreeMap::from([(
                    "user-a".to_string(),
                    SessionSpeakerCaptureStats {
                        user_id: "user-a".to_string(),
                        label: "Will".to_string(),
                        username: "will".to_string(),
                        active: true,
                        buffered_audio_bytes: 4096,
                        flush_in_flight: false,
                        segment_started_at: (now - chrono::Duration::seconds(3))
                            .to_rfc3339_opts(SecondsFormat::Millis, true),
                        last_pcm_at: (now - chrono::Duration::milliseconds(250))
                            .to_rfc3339_opts(SecondsFormat::Millis, true),
                    },
                )]),
                ..SessionCaptureStats::default()
            },
            ..RuntimeSessionStatus::default()
        },
    );

    let result = execute(&mut runtime, &activation_job, &payload)
        .await
        .unwrap();

    assert_eq!(result["status"], json!("deferred"));
    assert_eq!(result["reason"], json!("waiting_for_live_speaker_audio"));
}

#[tokio::test(flavor = "current_thread")]
async fn wake_activation_waits_for_pending_speaker_audio_segment_transcription() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    let mut runtime = test_runtime(store);
    let now = Utc::now();
    let start = now - chrono::Duration::seconds(20);
    let wake = append_event(
        &runtime.timeline_store,
        start,
        start + chrono::Duration::seconds(2),
        "Will",
        "user-a",
        "Hey Clanky are you working",
        json!({"wake": true}),
        1,
    )
    .await;
    let scheduled = schedule_from_wake_event(&runtime, &wake).await.unwrap();
    let activation_job_id = string_field(&scheduled["job"], "job_id");
    let activation_job = runtime
        .timeline_store
        .get_job(&activation_job_id)
        .await
        .unwrap();
    let payload = activation_job.wake_activation_payload().cloned().unwrap();
    runtime
        .timeline_store
        .create_job(Job::audio_segment(AudioSegmentPayload {
            guild_id: "guild".to_string(),
            guild_slug: "guild".to_string(),
            voice_channel_id: "code".to_string(),
            voice_channel_name: "Code Lounge".to_string(),
            voice_channel_slug: "code-lounge".to_string(),
            capture_run_id: "cap_test".to_string(),
            voice_bot_id: "clanky-vc1".to_string(),
            voice_bot_discord_user_id: "bot-user".to_string(),
            speaker_user_id: "user-a".to_string(),
            speaker_label: "Will".to_string(),
            speaker_username: "will".to_string(),
            segment_start_time: now - chrono::Duration::seconds(6),
            segment_end_time: now - chrono::Duration::seconds(1),
            segment_index: 2,
            duration_ms: 5000,
            source_audio_path: raw.path().join("pending.wav"),
            audio_checksum: "sha256:pending".to_string(),
            audio_bytes: 123,
            audio_format: "wav".to_string(),
            sample_rate_hz: 48_000,
            channels: 2,
            sample_width_bits: 16,
            post_processing: "pcm_s16le_48khz_stereo_to_wav".to_string(),
        }))
        .await
        .unwrap();

    let result = execute(&mut runtime, &activation_job, &payload)
        .await
        .unwrap();

    assert_eq!(result["status"], json!("deferred"));
    assert_eq!(
        result["reason"],
        json!("waiting_for_audio_segment_transcription")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn wake_followup_inside_preempt_window_replaces_spawned_activation_work() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    let mut runtime = test_runtime(store);
    let start = dt(2026, 5, 12, 16, 0, 0);
    let first = append_event(
        &runtime.timeline_store,
        start,
        start + chrono::Duration::seconds(1),
        "Will",
        "user-a",
        "Hey Clanky",
        json!({"wake": true}),
        1,
    )
    .await;
    append_event(
        &runtime.timeline_store,
        start + chrono::Duration::seconds(2),
        start + chrono::Duration::seconds(3),
        "Will",
        "user-a",
        "summarize the last thing",
        json!({}),
        2,
    )
    .await;
    let scheduled = schedule_from_wake_event(&runtime, &first).await.unwrap();
    let original_activation_id = string_field(&scheduled["job"], "job_id");
    let original_activation = runtime
        .timeline_store
        .get_job(&original_activation_id)
        .await
        .unwrap();
    let payload = original_activation
        .wake_activation_payload()
        .cloned()
        .unwrap();
    let dispatched = execute(&mut runtime, &original_activation, &payload)
        .await
        .unwrap();
    let command_job_id = string_field(&dispatched["created"]["job"], "job_id");
    let command_job = runtime
        .timeline_store
        .get_job(&command_job_id)
        .await
        .unwrap();
    assert_eq!(
        command_job.parent_job_id.as_deref(),
        Some(original_activation_id.as_str())
    );

    let second = append_event(
        &runtime.timeline_store,
        start + chrono::Duration::seconds(8),
        start + chrono::Duration::seconds(9),
        "Will",
        "user-a",
        "Hey Clanky actually include Vince too",
        json!({"wake": true}),
        3,
    )
    .await;
    let replaced = schedule_from_wake_event(&runtime, &second).await.unwrap();

    assert_eq!(replaced["status"], json!("replaced"));
    let replacement_id = string_field(&replaced["job"], "job_id");
    assert_ne!(replacement_id, original_activation_id);
    let original = runtime
        .timeline_store
        .get_job(&original_activation_id)
        .await
        .unwrap();
    let command = runtime
        .timeline_store
        .get_job(&command_job_id)
        .await
        .unwrap();
    let replacement = runtime
        .timeline_store
        .get_job(&replacement_id)
        .await
        .unwrap();
    assert_eq!(original.state, JobState::Cancelled);
    assert_eq!(command.state, JobState::Cancelled);
    let payload = replacement.wake_activation_payload().unwrap();
    assert_eq!(
        payload.latest_wake_event_id,
        string_field(&second, "event_id")
    );
    assert!(
        payload
            .replacement_of_job_ids
            .contains(&original_activation_id)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn wake_followup_after_independent_threshold_schedules_separate_activation() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    let runtime = test_runtime(store);
    let start = dt(2026, 5, 12, 16, 0, 0);
    let first = append_event(
        &runtime.timeline_store,
        start,
        start + chrono::Duration::seconds(1),
        "Will",
        "user-a",
        "Hey Clanky",
        json!({"wake": true}),
        1,
    )
    .await;
    let second = append_event(
        &runtime.timeline_store,
        start + chrono::Duration::seconds(50),
        start + chrono::Duration::seconds(51),
        "Will",
        "user-a",
        "Hey Clanky new request",
        json!({"wake": true}),
        2,
    )
    .await;

    let first_scheduled = schedule_from_wake_event(&runtime, &first).await.unwrap();
    let second_scheduled = schedule_from_wake_event(&runtime, &second).await.unwrap();

    assert_eq!(second_scheduled["status"], json!("scheduled"));
    assert_ne!(
        string_field(&first_scheduled["job"], "job_id"),
        string_field(&second_scheduled["job"], "job_id")
    );
}

fn test_runtime(timeline_store: TimelineStore) -> Runtime {
    Runtime {
        started_at: dt(2026, 5, 12, 15, 0, 0),
        guilds: BTreeMap::new(),
        rooms: BTreeMap::new(),
        control_config: ControlConfig::default(),
        room_controls: BTreeMap::new(),
        sessions: BTreeMap::new(),
        bots: BTreeMap::new(),
        agents: AgentRuntime::default(),
        automations: BTreeMap::new(),
        timeline_store,
        auto_join_enabled: true,
        manual_leave_cooldown_seconds: 20 * 60,
        manual_join_hold_seconds: 60 * 60,
        pause_release_seconds: 20 * 60,
    }
}

async fn append_event(
    store: &TimelineStore,
    start: chrono::DateTime<chrono::Utc>,
    end: chrono::DateTime<chrono::Utc>,
    label: &str,
    user_id: &str,
    text: &str,
    wake_metadata: Value,
    segment_index: i64,
) -> Value {
    store
        .append_speech_event(SpeechEventInput {
            guild_id: "guild".to_string(),
            guild_slug: "guild".to_string(),
            voice_channel_id: "code".to_string(),
            voice_channel_name: "Code Lounge".to_string(),
            voice_channel_slug: "code-lounge".to_string(),
            capture_run_id: "cap_test".to_string(),
            voice_bot_id: "clanky-vc1".to_string(),
            voice_bot_discord_user_id: "bot-user".to_string(),
            speaker_user_id: user_id.to_string(),
            speaker_label: label.to_string(),
            speaker_username: label.to_ascii_lowercase(),
            segment_start_time: start,
            segment_end_time: end,
            text_draft: text.to_string(),
            source_audio_path: std::path::PathBuf::from(format!(
                "/tmp/clankcord-test-{}.wav",
                start.to_rfc3339_opts(SecondsFormat::Secs, true)
            )),
            audio_checksum: "sha256:test".to_string(),
            segment_index,
            duration_ms: (end - start).num_milliseconds(),
            wake_metadata,
            ..Default::default()
        })
        .await
        .unwrap()
}
