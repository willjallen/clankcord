use std::collections::BTreeMap;

use chrono::{SecondsFormat, Utc};
use serde_json::{Value, json};

mod common;

use clankcord::runtime::domain::voice_capture::wake_activations::{
    execute, schedule_from_wake_event,
};
use clankcord::runtime::timeline::{SpeechEventInput, TimelineStore};
use clankcord::runtime::{
    AgentSessionRecord, AudioSegmentPayload, DiscordVoicePlaybackCue, Job, JobKind, JobPayload,
    JobState, Runtime, SessionCaptureStats, SessionSpeakerCaptureStats, VoiceCaptureSessionStatus,
};

use common::{dt, test_store};

const EXPECTED_WAKE_ACTIVATION_MAX_WINDOW_SECONDS: i64 = 86_400;

fn string_field(value: &Value, key: &str) -> String {
    match value.get(key) {
        Some(Value::String(text)) => text.trim().to_string(),
        Some(Value::Number(number)) => number.to_string(),
        Some(Value::Bool(boolean)) => boolean.to_string(),
        _ => String::new(),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn wake_activation_uses_long_default_max_window() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    let runtime = test_runtime(store);
    let start = dt(2026, 5, 12, 16, 0, 0);
    let wake = append_event(
        &runtime.timeline_store,
        start,
        start + chrono::Duration::seconds(1),
        "Will",
        "user-a",
        "Hey Clanky",
        json!({"wake": true, "score": 0.88}),
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
    let payload = activation_job
        .wake_activation_payload()
        .expect("missing wake activation payload");

    assert_eq!(
        payload.max_window_seconds,
        EXPECTED_WAKE_ACTIVATION_MAX_WINDOW_SECONDS
    );
}

#[tokio::test(flavor = "current_thread")]
async fn wake_activation_builds_labeled_bundle_before_dispatch() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    let mut runtime = test_runtime(store);
    insert_agent_session(&runtime.timeline_store).await;
    let start = dt(2026, 5, 12, 16, 0, 0);
    let prior = append_event(
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
    let post_other = append_event(
        &runtime.timeline_store,
        start + chrono::Duration::seconds(4),
        start + chrono::Duration::seconds(5),
        "Vince",
        "user-b",
        "the float issue was about decimal formatting",
        json!({}),
        4,
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
    assert_eq!(command.kind, JobKind::AgentTask);
    let command_value = command.command_value().unwrap();
    assert_eq!(command_value["command_kind"], json!("agent_task"));
    assert_eq!(
        command_value["arguments"]["request"],
        json!("summarize what Vince said about floats")
    );
    assert!(command_value["arguments"]["activation"]["prior_to_activation"].is_null());
    assert!(command_value["arguments"]["activation"]["post_activation_turn"].is_null());
    assert!(
        !command_value["arguments"]["activation"]["source_event_ids"]
            .as_array()
            .unwrap()
            .contains(&prior["event_id"])
    );
    assert!(
        command_value["arguments"]["activation"]["source_event_ids"]
            .as_array()
            .unwrap()
            .contains(&wake["event_id"])
    );
    assert!(
        command_value["arguments"]["activation"]["source_event_ids"]
            .as_array()
            .unwrap()
            .contains(&post["event_id"])
    );
    assert!(
        command_value["arguments"]["activation"]["source_event_ids"]
            .as_array()
            .unwrap()
            .contains(&post_other["event_id"])
    );
}

#[tokio::test(flavor = "current_thread")]
async fn wake_activation_uses_speech_segment_that_overlaps_probe_event() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    let mut runtime = test_runtime(store);
    insert_agent_session(&runtime.timeline_store).await;
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
async fn wake_activation_completes_without_agent_task_for_bare_wake_word() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    let mut runtime = test_runtime(store);
    insert_agent_session(&runtime.timeline_store).await;
    let start = dt(2026, 5, 12, 16, 0, 0);
    let wake = append_event(
        &runtime.timeline_store,
        start,
        start + chrono::Duration::seconds(1),
        "Will",
        "user-a",
        "Hey Clanky",
        json!({"wake": true, "score": 0.88}),
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

    assert_eq!(result["status"], json!("no_request_captured"));
    assert_eq!(result["reason"], json!("empty_request_text"));
    let jobs = runtime
        .timeline_store
        .list_jobs(Some("guild"), None)
        .await
        .unwrap();
    assert!(!jobs.iter().any(|job| job.kind == JobKind::AgentTask));
}

#[tokio::test(flavor = "current_thread")]
async fn wake_activation_dispatches_agent_task_for_long_captured_request() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    let mut runtime = test_runtime(store);
    insert_agent_session(&runtime.timeline_store).await;
    let start = dt(2026, 5, 12, 16, 0, 0);
    let wake = append_event(
        &runtime.timeline_store,
        start,
        start + chrono::Duration::seconds(1),
        "Will",
        "user-a",
        "Hey Clanky",
        json!({"wake": true, "score": 0.88}),
        1,
    )
    .await;
    let first = append_event(
        &runtime.timeline_store,
        start + chrono::Duration::seconds(35),
        start + chrono::Duration::seconds(36),
        "Will",
        "user-a",
        "can you check the Codex usage",
        json!({}),
        2,
    )
    .await;
    let last = append_event(
        &runtime.timeline_store,
        start + chrono::Duration::seconds(58),
        start + chrono::Duration::seconds(59),
        "Will",
        "user-a",
        "inside the Docker environment but I would like you to try",
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
    let payload = activation_job.wake_activation_payload().cloned().unwrap();
    let result = execute(&mut runtime, &activation_job, &payload)
        .await
        .unwrap();

    assert_eq!(result["status"], json!("dispatched"));
    let agent_job_id = string_field(&result["created"]["job"], "job_id");
    let agent_job = runtime.timeline_store.get_job(&agent_job_id).await.unwrap();
    assert_eq!(agent_job.kind, JobKind::AgentTask);
    let command_value = agent_job.command_value().unwrap();
    assert_eq!(
        command_value["arguments"]["request"],
        json!(
            "can you check the Codex usage inside the Docker environment but I would like you to try"
        )
    );
    assert!(
        command_value["arguments"]["activation"]["source_event_ids"]
            .as_array()
            .unwrap()
            .contains(&first["event_id"])
    );
    assert!(
        command_value["arguments"]["activation"]["source_event_ids"]
            .as_array()
            .unwrap()
            .contains(&last["event_id"])
    );
}

#[tokio::test(flavor = "current_thread")]
async fn wake_activation_treats_resume_text_as_agent_request() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    let mut runtime = test_runtime(store);
    let start = dt(2026, 5, 12, 16, 0, 0);
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
    append_event(
        &runtime.timeline_store,
        start + chrono::Duration::seconds(3),
        start + chrono::Duration::seconds(4),
        "Will",
        "user-a",
        "resume the session about banking",
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
    let payload = activation_job.wake_activation_payload().cloned().unwrap();
    let result = execute(&mut runtime, &activation_job, &payload)
        .await
        .unwrap();

    assert_eq!(result["status"], json!("dispatched"));
    let start_job_id = string_field(&result["created"]["job"], "job_id");
    let start_job = runtime.timeline_store.get_job(&start_job_id).await.unwrap();
    assert_eq!(start_job.kind, JobKind::AgentSessionStart);
    let JobPayload::AgentSessionStart(payload) = &start_job.payload else {
        panic!("expected agent-session start payload");
    };
    assert_eq!(
        payload.command.arguments.request,
        "resume the session about banking"
    );
    let jobs = runtime
        .timeline_store
        .list_jobs(Some("guild"), None)
        .await
        .unwrap();
    assert!(
        !jobs
            .iter()
            .any(|job| job.kind == JobKind::DiscordForumThreadCreate)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn wake_activation_reuses_active_session_without_thread() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    let mut runtime = test_runtime(store);
    let created_at = dt(2026, 5, 12, 15, 0, 0);
    runtime
        .timeline_store
        .create_agent_session_record(AgentSessionRecord::new_voice(
            "ags_active",
            "guild",
            "code",
            "agent-threads",
            "",
            created_at.to_rfc3339_opts(SecondsFormat::Millis, true),
            (Utc::now() + chrono::Duration::hours(8)).to_rfc3339_opts(SecondsFormat::Millis, true),
        ))
        .await
        .unwrap();
    let start = dt(2026, 5, 12, 16, 0, 0);
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
    append_event(
        &runtime.timeline_store,
        start + chrono::Duration::seconds(3),
        start + chrono::Duration::seconds(4),
        "Will",
        "user-a",
        "say hello",
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
    let payload = activation_job.wake_activation_payload().cloned().unwrap();
    let result = execute(&mut runtime, &activation_job, &payload)
        .await
        .unwrap();

    assert_eq!(result["status"], json!("dispatched"));
    let task_job_id = string_field(&result["created"]["job"], "job_id");
    let task_job = runtime.timeline_store.get_job(&task_job_id).await.unwrap();
    assert_eq!(task_job.kind, JobKind::AgentTask);
    let JobPayload::AgentTask(payload) = &task_job.payload else {
        panic!("expected agent task payload");
    };
    assert_eq!(payload.agent_session_id, "ags_active");
    let jobs = runtime
        .timeline_store
        .list_jobs(Some("guild"), None)
        .await
        .unwrap();
    assert!(
        !jobs
            .iter()
            .any(|job| job.kind == JobKind::AgentSessionStart)
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
    let runtime = test_runtime(store);
    insert_agent_session(&runtime.timeline_store).await;
    runtime
        .timeline_store
        .upsert_capture_session_status(&VoiceCaptureSessionStatus {
            session_id: "cap_test".to_string(),
            guild_id: "guild".to_string(),
            channel_id: "code".to_string(),
            voice_channel_id: "code".to_string(),
            active: true,
            ..VoiceCaptureSessionStatus::default()
        })
        .await
        .unwrap();
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
    insert_agent_session(&runtime.timeline_store).await;
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
        .upsert_capture_session_status(&VoiceCaptureSessionStatus {
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
            ..VoiceCaptureSessionStatus::default()
        })
        .await
        .unwrap();

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
    insert_agent_session(&runtime.timeline_store).await;
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
            segment_start_time: now - chrono::Duration::seconds(18),
            segment_end_time: now - chrono::Duration::seconds(16),
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
    assert_eq!(result["reason"], json!("waiting_for_request_transcription"));
}

#[tokio::test(flavor = "current_thread")]
async fn wake_activation_waits_for_retryable_failed_request_audio_segment() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    let mut runtime = test_runtime(store);
    insert_agent_session(&runtime.timeline_store).await;
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
    let mut audio = Job::audio_segment(AudioSegmentPayload {
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
        segment_start_time: now - chrono::Duration::seconds(18),
        segment_end_time: now - chrono::Duration::seconds(16),
        segment_index: 2,
        duration_ms: 2000,
        source_audio_path: raw.path().join("failed-retryable.wav"),
        audio_checksum: "sha256:failed-retryable".to_string(),
        audio_bytes: 123,
        audio_format: "wav".to_string(),
        sample_rate_hz: 48_000,
        channels: 2,
        sample_width_bits: 16,
        post_processing: "pcm_s16le_48khz_stereo_to_wav".to_string(),
    });
    audio.set_state(JobState::Failed);
    audio.metadata.error =
        "HTTP status server error (503 Service Unavailable) for url (http://127.0.0.1:8080/v1/audio/transcriptions)"
            .to_string();
    runtime.timeline_store.create_job(audio).await.unwrap();

    let result = execute(&mut runtime, &activation_job, &payload)
        .await
        .unwrap();

    assert_eq!(result["status"], json!("deferred"));
    assert_eq!(result["reason"], json!("waiting_for_request_transcription"));
}

#[tokio::test(flavor = "current_thread")]
async fn wake_activation_waits_for_pending_segment_that_overlaps_closed_window() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    let mut runtime = test_runtime(store);
    insert_agent_session(&runtime.timeline_store).await;
    let now = Utc::now();
    let wake_started_at = now - chrono::Duration::seconds(10);
    let close_at = wake_started_at + chrono::Duration::seconds(5);
    let wake = append_event(
        &runtime.timeline_store,
        wake_started_at,
        wake_started_at + chrono::Duration::milliseconds(500),
        "Will",
        "user-a",
        "Hey Clanky",
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
            segment_start_time: close_at - chrono::Duration::seconds(2),
            segment_end_time: close_at + chrono::Duration::milliseconds(200),
            segment_index: 2,
            duration_ms: 2200,
            source_audio_path: raw.path().join("pending-overlap.wav"),
            audio_checksum: "sha256:pending-overlap".to_string(),
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
    assert_eq!(result["reason"], json!("waiting_for_request_transcription"));
}

#[tokio::test(flavor = "current_thread")]
async fn wake_activation_waits_for_pending_request_audio_after_old_settlement_window() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    let mut runtime = test_runtime(store);
    insert_agent_session(&runtime.timeline_store).await;
    let now = Utc::now();
    let wake_started_at = now - chrono::Duration::seconds(300);
    let wake = append_event(
        &runtime.timeline_store,
        wake_started_at,
        wake_started_at + chrono::Duration::milliseconds(500),
        "Will",
        "user-a",
        "Hey Clanky",
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
            segment_start_time: wake_started_at + chrono::Duration::seconds(1),
            segment_end_time: wake_started_at + chrono::Duration::seconds(4),
            segment_index: 2,
            duration_ms: 3000,
            source_audio_path: raw.path().join("pending-after-old-settlement.wav"),
            audio_checksum: "sha256:pending-after-old-settlement".to_string(),
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
    assert_eq!(result["reason"], json!("waiting_for_request_transcription"));
    assert!(string_field(&result, "request_audio_closed_at").len() > 0);
}

#[tokio::test(flavor = "current_thread")]
async fn wake_activation_acks_closed_voice_window_then_waits_for_late_stt() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    let mut runtime = test_runtime(store);
    insert_agent_session(&runtime.timeline_store).await;
    runtime
        .timeline_store
        .upsert_capture_session_status(&VoiceCaptureSessionStatus {
            session_id: "cap_test".to_string(),
            guild_id: "guild".to_string(),
            channel_id: "code".to_string(),
            voice_channel_id: "code".to_string(),
            active: true,
            ..VoiceCaptureSessionStatus::default()
        })
        .await
        .unwrap();
    let now = Utc::now();
    let wake_started_at = now - chrono::Duration::seconds(10);
    let request_started_at = wake_started_at + chrono::Duration::seconds(1);
    let request_ended_at = wake_started_at + chrono::Duration::seconds(4);
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
                "startedAt": wake_started_at.to_rfc3339_opts(SecondsFormat::Millis, true),
                "endedAt": (wake_started_at + chrono::Duration::milliseconds(500)).to_rfc3339_opts(SecondsFormat::Millis, true),
                "wake": {"wake": true, "score": 0.91},
                "wake_detected": true,
            }),
        )
        .await
        .unwrap();

    let scheduled = schedule_from_wake_event(&runtime, &wake).await.unwrap();
    let activation_job_id = string_field(&scheduled["job"], "job_id");
    let activation_job = runtime
        .timeline_store
        .get_job(&activation_job_id)
        .await
        .unwrap();
    let payload = activation_job.wake_activation_payload().cloned().unwrap();
    let pending_audio = runtime
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
            segment_start_time: request_started_at,
            segment_end_time: request_ended_at,
            segment_index: 2,
            duration_ms: (request_ended_at - request_started_at).num_milliseconds(),
            source_audio_path: raw.path().join("pending-late.wav"),
            audio_checksum: "sha256:pending-late".to_string(),
            audio_bytes: 123,
            audio_format: "wav".to_string(),
            sample_rate_hz: 48_000,
            channels: 2,
            sample_width_bits: 16,
            post_processing: "pcm_s16le_48khz_stereo_to_wav".to_string(),
        }))
        .await
        .unwrap();

    let deferred = execute(&mut runtime, &activation_job, &payload)
        .await
        .unwrap();
    assert_eq!(deferred["status"], json!("deferred"));
    assert_eq!(
        deferred["reason"],
        json!("waiting_for_request_transcription")
    );
    let jobs = runtime
        .timeline_store
        .list_jobs(Some("guild"), None)
        .await
        .unwrap();
    assert!(jobs.iter().any(|job| {
        job.discord_voice_playback_payload()
            .is_some_and(|payload| payload.cue == DiscordVoicePlaybackCue::Ack)
    }));

    let speech = append_event(
        &runtime.timeline_store,
        request_started_at,
        request_ended_at,
        "Will",
        "user-a",
        "Hey Clanky send Vince the note",
        json!({}),
        3,
    )
    .await;
    let mut completed_audio = pending_audio;
    completed_audio.mark_complete();
    runtime
        .timeline_store
        .update_job(&completed_audio)
        .await
        .unwrap();

    let activation_job = runtime
        .timeline_store
        .get_job(&activation_job_id)
        .await
        .unwrap();
    let dispatched = execute(&mut runtime, &activation_job, &payload)
        .await
        .unwrap();

    assert_eq!(dispatched["status"], json!("dispatched"));
    let agent_job_id = string_field(&dispatched["created"]["job"], "job_id");
    let agent_job = runtime.timeline_store.get_job(&agent_job_id).await.unwrap();
    let command_value = agent_job.command_value().unwrap();
    assert_eq!(
        command_value["arguments"]["request"],
        json!("send Vince the note")
    );
    assert!(
        command_value["arguments"]["source_event_ids"]
            .as_array()
            .unwrap()
            .contains(&speech["event_id"])
    );
}

#[tokio::test(flavor = "current_thread")]
async fn wake_activation_waits_for_all_overlapping_request_audio_segments() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    let mut runtime = test_runtime(store);
    insert_agent_session(&runtime.timeline_store).await;
    let now = Utc::now();
    let wake_started_at = now - chrono::Duration::seconds(20);
    let request_one_start = wake_started_at + chrono::Duration::seconds(1);
    let request_one_end = wake_started_at + chrono::Duration::seconds(3);
    let request_two_start = wake_started_at + chrono::Duration::seconds(3);
    let request_two_end = wake_started_at + chrono::Duration::seconds(5);
    let wake = append_event(
        &runtime.timeline_store,
        wake_started_at,
        wake_started_at + chrono::Duration::milliseconds(500),
        "Will",
        "user-a",
        "Hey Clanky",
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
    let first_audio = runtime
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
            segment_start_time: request_one_start,
            segment_end_time: request_one_end,
            segment_index: 2,
            duration_ms: 2000,
            source_audio_path: raw.path().join("pending-one.wav"),
            audio_checksum: "sha256:pending-one".to_string(),
            audio_bytes: 123,
            audio_format: "wav".to_string(),
            sample_rate_hz: 48_000,
            channels: 2,
            sample_width_bits: 16,
            post_processing: "pcm_s16le_48khz_stereo_to_wav".to_string(),
        }))
        .await
        .unwrap();
    let second_audio = runtime
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
            segment_start_time: request_two_start,
            segment_end_time: request_two_end,
            segment_index: 3,
            duration_ms: 2000,
            source_audio_path: raw.path().join("pending-two.wav"),
            audio_checksum: "sha256:pending-two".to_string(),
            audio_bytes: 123,
            audio_format: "wav".to_string(),
            sample_rate_hz: 48_000,
            channels: 2,
            sample_width_bits: 16,
            post_processing: "pcm_s16le_48khz_stereo_to_wav".to_string(),
        }))
        .await
        .unwrap();

    let deferred = execute(&mut runtime, &activation_job, &payload)
        .await
        .unwrap();
    assert_eq!(deferred["status"], json!("deferred"));

    append_event(
        &runtime.timeline_store,
        request_one_start,
        request_one_end,
        "Will",
        "user-a",
        "send Vince",
        json!({}),
        4,
    )
    .await;
    let mut first_audio = first_audio;
    first_audio.mark_complete();
    runtime
        .timeline_store
        .update_job(&first_audio)
        .await
        .unwrap();

    let activation_job = runtime
        .timeline_store
        .get_job(&activation_job_id)
        .await
        .unwrap();
    let payload = activation_job.wake_activation_payload().cloned().unwrap();
    let still_deferred = execute(&mut runtime, &activation_job, &payload)
        .await
        .unwrap();
    assert_eq!(still_deferred["status"], json!("deferred"));
    assert_eq!(
        still_deferred["reason"],
        json!("waiting_for_request_transcription")
    );

    append_event(
        &runtime.timeline_store,
        request_two_start,
        request_two_end,
        "Will",
        "user-a",
        "the note",
        json!({}),
        5,
    )
    .await;
    let mut second_audio = second_audio;
    second_audio.mark_complete();
    runtime
        .timeline_store
        .update_job(&second_audio)
        .await
        .unwrap();

    let activation_job = runtime
        .timeline_store
        .get_job(&activation_job_id)
        .await
        .unwrap();
    let payload = activation_job.wake_activation_payload().cloned().unwrap();
    let dispatched = execute(&mut runtime, &activation_job, &payload)
        .await
        .unwrap();

    assert_eq!(dispatched["status"], json!("dispatched"));
}

#[tokio::test(flavor = "current_thread")]
async fn wake_followup_inside_preempt_window_replaces_spawned_activation_work() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    let mut runtime = test_runtime(store);
    insert_agent_session(&runtime.timeline_store).await;
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
    Runtime::from_store(timeline_store).unwrap()
}

async fn insert_agent_session(store: &TimelineStore) {
    let created_at = chrono::Utc::now();
    let max_active_until = created_at + chrono::Duration::hours(8);
    let record = AgentSessionRecord::new_voice(
        "ags_wake_test",
        "guild",
        "code",
        "agent-threads",
        "thread-wake-test",
        created_at.to_rfc3339_opts(SecondsFormat::Millis, true),
        max_active_until.to_rfc3339_opts(SecondsFormat::Millis, true),
    );
    store.create_agent_session_record(record).await.unwrap();
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
