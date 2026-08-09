//! Binary job records: envelope versioning, payload round-trips, decode rejections,
//! and the public jobs view projection.

use chrono::{TimeZone, Utc};
use serde_json::json;

use clankcord::domain::Ctx;
use clankcord::domain::rooms::RoomConfig;
use clankcord::model::job::{
    AgentSessionStartPayload, BinaryPayload, CommandRequest, DiscordForumThreadCreatePayload,
    DiscordForumThreadRenamePayload, DiscordTextSendPayload, DiscordTypingAction,
    DiscordTypingIndicatorOutput, DiscordTypingIndicatorPayload, DiscordVoiceDeafenOutput,
    DiscordVoiceDeafenPayload, DiscordVoiceJoinPayload, DiscordVoiceLeaveOutput,
    DiscordVoiceMuteOutput, DiscordVoiceMutePayload, DiscordVoicePlayAudioOutput,
    DiscordVoicePlayAudioPayload, DiscordVoicePlaybackCue, DiscordVoicePlaybackOutput,
    DiscordVoicePlaybackPayload, DiscordVoiceStatusSnapshotOutput, Job, JobKind, JobOutput,
    JobState, OpaqueValue, TextAttachmentPayload, TextDeliveryKind, TextDeliveryPayload,
    TextTarget, TextTargetKind, TranscriptPublicationPayload, WakeActivationPayload,
    WakeProbePayload,
};
use clankcord::model::scope::RuntimeScope;
use clankcord::views::JobsRequest;

use crate::support::jobs::raw_path;
use crate::support::{initialize_test_config, test_store};

#[tokio::test(flavor = "current_thread")]
async fn job_round_trips_as_binary_record() {
    let command = CommandRequest::from_json(&json!({
        "command_kind": "agent_task",
        "guild_id": "guild",
        "scope_id": "channel",
        "requested_by_user_id": "requester",
        "arguments": {"question": "what happened?", "relative_start": "-20m"}
    }))
    .unwrap();
    let job = Job::agent_task_for_session(
        "ags_test",
        RuntimeScope::voice_channel("guild", "channel"),
        "requester",
        command,
    );

    let encoded = job.encode().unwrap();
    let parsed = Job::decode(&encoded).unwrap();

    assert_eq!(parsed.kind, JobKind::AgentTask);
    assert_eq!(parsed.state, JobState::Queued);
    assert_eq!(parsed.command_kind(), "agent_task");
    assert_eq!(
        parsed.command().unwrap().arguments.question,
        "what happened?"
    );
}
#[test]
fn job_payload_blob_uses_current_version_envelope() {
    let job = Job::runtime_maintenance(500);
    let encoded = job.encode().unwrap();

    assert_eq!(&encoded[..8], b"CLANKJOB");
    assert_eq!(u16::from_le_bytes([encoded[8], encoded[9]]), 9);
    assert!(Job::is_current_payload_blob(&encoded));
}
#[test]
fn job_decode_rejects_pre_v0_2_0_raw_bincode_payload() {
    let job = Job::runtime_maintenance(500);
    let pre_v0_2_0 = bincode::serialize(&job).unwrap();

    let error = Job::decode(&pre_v0_2_0).unwrap_err().to_string();

    assert!(error.contains("job payload blob is not a current encoded job payload"));
    assert!(!Job::is_current_payload_blob(&pre_v0_2_0));
}
#[test]
fn job_state_rejects_agent_specific_dispatch_failure_state() {
    assert!("agent_dispatch_failed".parse::<JobState>().is_err());
    assert_eq!("failed".parse::<JobState>().unwrap(), JobState::Failed);
}
#[tokio::test(flavor = "current_thread")]
async fn jobs_public_view_uses_generic_scope_fields() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(&raw.path().join("voice")).await;
    let created = store
        .create_job(Job::agent_task_for_session(
            "ags_test",
            RuntimeScope::voice_channel("guild", "code"),
            "user-a",
            CommandRequest::agent_task("guild", "code", "user-a", "summarize"),
        ))
        .await
        .unwrap();
    let runtime = Ctx::new(store);

    let jobs = clankcord::views::jobs::jobs(
        &runtime,
        JobsRequest {
            guild_id: "guild".to_string(),
            ..JobsRequest::default()
        },
    )
    .await
    .unwrap();
    let job = jobs["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|job| job["job_id"] == created.id)
        .expect("created job appears in public jobs view");

    assert_eq!(job["scope_kind"], "voice_channel");
    assert_eq!(job["scope_id"], "code");
    assert!(job.get("voice_channel_id").is_none());

    let verbose = clankcord::views::jobs::get_job_payload(&runtime, &created.id, true)
        .await
        .unwrap();
    assert_eq!(verbose["scope_kind"], "voice_channel");
    assert_eq!(verbose["scope_id"], "code");
    assert!(verbose.get("voice_channel_id").is_none());
}
#[tokio::test(flavor = "current_thread")]
async fn wake_probe_payload_references_ready_audio_artifact() {
    let start = chrono::Utc.with_ymd_and_hms(2026, 5, 13, 12, 0, 0).unwrap();
    let source_audio_path = std::path::PathBuf::from("/tmp/clankcord/wake-probe.wav");
    let job = Job::wake_probe(WakeProbePayload {
        guild_id: "guild".to_string(),
        guild_slug: "guild".to_string(),
        voice_channel_id: "channel".to_string(),
        voice_channel_name: "Channel".to_string(),
        voice_channel_slug: "channel".to_string(),
        capture_run_id: "cap".to_string(),
        voice_bot_id: "bot".to_string(),
        voice_bot_discord_user_id: "bot-user".to_string(),
        speaker_user_id: "speaker".to_string(),
        speaker_label: "Speaker".to_string(),
        speaker_username: "speaker_name".to_string(),
        probe_start_time: start,
        probe_end_time: start + chrono::Duration::milliseconds(500),
        probe_index: 2,
        duration_ms: 500,
        source_audio_path: source_audio_path.clone(),
        audio_checksum: "sha256:test".to_string(),
        audio_bytes: 44,
        audio_format: "wav".to_string(),
        sample_rate_hz: 48_000,
        channels: 2,
        sample_width_bits: 16,
        post_processing: "pcm_s16le_to_wav".to_string(),
        stream_id: "guild:channel:speaker".to_string(),
        reset_stream: false,
    });

    assert_eq!(job.kind, JobKind::WakeProbe);
    assert_eq!(
        job.wake_probe_payload().unwrap().source_audio_path,
        source_audio_path
    );
    let payload = job.payload_value();
    assert_eq!(
        payload["source_audio_path"],
        json!("/tmp/clankcord/wake-probe.wav")
    );
    assert_eq!(payload["stream_id"], json!("guild:channel:speaker"));
    assert_eq!(payload["reset_stream"], json!(false));
}
#[tokio::test(flavor = "current_thread")]
async fn opaque_json_lowers_to_binary_payload() {
    let payload = BinaryPayload::from_json(&json!({"nested": ["value", 1]})).unwrap();
    assert!(!payload.as_bytes().is_empty());
    assert_eq!(payload.to_json(), json!({"nested": ["value", 1]}));
}
#[tokio::test(flavor = "current_thread")]
async fn text_delivery_payload_is_a_first_class_binary_job() {
    let payload = TextDeliveryPayload::from_json(&json!({
        "intent": "question",
        "target": "agent_chat",
        "source_job_id": "job_source",
        "requested_by_user_id": "user-a",
        "content": "Do you mean the last 20 minutes?",
        "extra_boundary_field": {"kept": true}
    }))
    .unwrap();
    let job = Job::text_delivery(
        RuntimeScope::voice_channel("guild", "code"),
        "user-a",
        payload,
    );
    let decoded = Job::decode(&job.encode().unwrap()).unwrap();

    assert_eq!(decoded.kind, JobKind::TextDelivery);
    let delivery = decoded.text_delivery_payload().unwrap();
    assert_eq!(delivery.intent, TextDeliveryKind::Question);
    assert_eq!(delivery.target.kind, TextTargetKind::AgentChat);
    assert_eq!(delivery.source_job_id, "job_source");
    assert_eq!(
        delivery.to_json()["extra_boundary_field"]["kept"],
        json!(true)
    );

    let without_attachments = TextDeliveryPayload::from_json(&json!({
        "intent": "message",
        "target": "agent_chat",
        "requested_by_user_id": "user-a",
        "content": "No attachment here.",
        "attachments": []
    }))
    .unwrap();
    assert!(without_attachments.attachments.is_empty());
    assert!(without_attachments.to_json()["attachments"].is_null());
}
#[tokio::test(flavor = "current_thread")]
async fn text_delivery_attachments_are_first_class_payload_fields() {
    let payload = TextDeliveryPayload::from_json(&json!({
        "intent": "message",
        "target": "channel:thread-1",
        "source_job_id": "job_source",
        "requested_by_user_id": "user-a",
        "content": "Attached a benchmark.",
        "attachments": [
            {
                "path": "/workspace/agent/artifact.zip",
                "filename": "artifact.zip",
                "size_bytes": 128,
                "sha256": "sha256:abc123"
            }
        ],
        "extra_boundary_field": {"kept": true}
    }))
    .unwrap();
    let job = Job::text_delivery(
        RuntimeScope::voice_channel("guild", "code"),
        "user-a",
        payload,
    );
    let decoded = Job::decode(&job.encode().unwrap()).unwrap();
    let delivery = decoded.text_delivery_payload().unwrap();

    assert_eq!(delivery.attachments.len(), 1);
    assert_eq!(
        delivery.attachments[0].path,
        "/workspace/agent/artifact.zip"
    );
    assert_eq!(delivery.attachments[0].filename, "artifact.zip");
    assert_eq!(delivery.attachments[0].size_bytes, 128);
    assert_eq!(delivery.attachments[0].sha256, "sha256:abc123");
    assert_eq!(
        delivery.to_json()["attachments"][0]["path"],
        json!("/workspace/agent/artifact.zip")
    );
    assert_eq!(
        delivery.to_json()["extra_boundary_field"]["kept"],
        json!(true)
    );
}
#[test]
fn discord_text_io_jobs_round_trip() {
    let text = Job::discord_text_send(
        RuntimeScope::voice_channel("guild", "code"),
        "user-a",
        DiscordTextSendPayload {
            intent: TextDeliveryKind::Message,
            target: TextTarget {
                kind: TextTargetKind::Channel,
                channel_id: "thread-1".to_string(),
                user_id: String::new(),
            },
            content: "Approve this?".to_string(),
            source_job_id: "job_source".to_string(),
            requested_by_user_id: String::new(),
            allowed_mentions: BinaryPayload::from_json(&json!({"parse": []})).unwrap(),
            components: BinaryPayload::from_json(&json!([{"type": 1}])).unwrap(),
            attachments: vec![TextAttachmentPayload {
                path: "/workspace/agent/artifact.zip".to_string(),
                filename: "artifact.zip".to_string(),
                size_bytes: 128,
                sha256: "sha256:abc123".to_string(),
            }],
        },
    );
    let decoded = Job::decode(&text.encode().unwrap()).unwrap();
    assert_eq!(decoded.kind, JobKind::DiscordTextSend);
    assert_eq!(decoded.payload.to_json()["components"][0]["type"], 1);
    assert_eq!(
        decoded.payload.to_json()["attachments"][0]["filename"],
        json!("artifact.zip")
    );

    let thread = Job::discord_forum_thread_create(
        RuntimeScope::voice_channel("guild", "code"),
        "user-a",
        DiscordForumThreadCreatePayload {
            parent_channel_id: "forum-1".to_string(),
            name: "agent code ags_1".to_string(),
            content: "# Agent Session".to_string(),
            auto_archive_minutes: 1440,
            source_job_id: "job_source".to_string(),
        },
    );
    let decoded = Job::decode(&thread.encode().unwrap()).unwrap();
    assert_eq!(decoded.kind, JobKind::DiscordForumThreadCreate);
    assert_eq!(decoded.payload.to_json()["parent_channel_id"], "forum-1");

    let rename = Job::discord_forum_thread_rename(
        RuntimeScope::voice_channel("guild", "code"),
        "runtime",
        DiscordForumThreadRenamePayload {
            thread_id: "thread-1".to_string(),
            name: "gRPC and REST".to_string(),
            source_job_id: "job_source".to_string(),
        },
    );
    let decoded = Job::decode(&rename.encode().unwrap()).unwrap();
    assert_eq!(decoded.kind, JobKind::DiscordForumThreadRename);
    assert_eq!(decoded.payload.to_json()["thread_id"], "thread-1");
    assert_eq!(decoded.payload.to_json()["name"], "gRPC and REST");
}
#[tokio::test(flavor = "current_thread")]
async fn discord_typing_indicator_job_round_trips_and_requeues_agent_parent() {
    let typing = Job::discord_typing_indicator(
        RuntimeScope::voice_channel("guild", "code"),
        "user-a",
        DiscordTypingIndicatorPayload {
            action: DiscordTypingAction::Start,
            target: TextTarget {
                kind: TextTargetKind::AgentSession,
                channel_id: String::new(),
                user_id: String::new(),
            },
            source_job_id: "job_agent".to_string(),
            requested_by_user_id: "user-a".to_string(),
            agent_task_attempt: 2,
        },
    );
    let decoded = Job::decode(&typing.encode().unwrap()).unwrap();
    assert_eq!(decoded.kind, JobKind::DiscordTypingIndicator);
    assert_eq!(decoded.payload.to_json()["action"], "start");
    assert_eq!(decoded.payload.to_json()["source_job_id"], "job_agent");
    assert_eq!(decoded.payload.to_json()["agent_task_attempt"], 2);

    let mut completed = decoded;
    completed.metadata.output = Some(JobOutput::DiscordTypingIndicator(
        DiscordTypingIndicatorOutput {
            action: DiscordTypingAction::Stop,
            target: TextTarget {
                kind: TextTargetKind::Channel,
                channel_id: "thread-1".to_string(),
                user_id: String::new(),
            },
            source_job_id: "job_agent".to_string(),
            status: "stopped".to_string(),
        },
    ));
    let completed = Job::decode(&completed.encode().unwrap()).unwrap();
    assert!(matches!(
        completed.metadata.output,
        Some(JobOutput::DiscordTypingIndicator(_))
    ));

    let raw = tempfile::tempdir().unwrap();
    let store = test_store(&raw.path().join("voice")).await;
    let parent = store
        .create_job(Job::agent_task_for_session(
            "ags_test",
            RuntimeScope::voice_channel("guild", "code"),
            "user-a",
            CommandRequest::agent_task("guild", "code", "user-a", "summarize this"),
        ))
        .await
        .unwrap();
    let child = store.create_child_job(&parent, completed).await.unwrap();
    let mut completed_child = store.get_job(&child.id).await.unwrap();
    completed_child.mark_complete();
    store.update_job(&completed_child).await.unwrap();

    let resolved = store.resolve_waiting_jobs().await.unwrap();

    assert_eq!(resolved.len(), 1);
    assert_eq!(
        store.get_job(&parent.id).await.unwrap().state,
        JobState::Queued
    );
}
#[test]
fn agent_session_start_and_publication_jobs_round_trip() {
    let command = CommandRequest::agent_task(
        "guild".to_string(),
        "code".to_string(),
        "user-a".to_string(),
        "follow up".to_string(),
    );
    let session = Job::agent_session_start(
        "guild",
        "code",
        "user-a",
        AgentSessionStartPayload {
            agent_session_id: "ags_1".to_string(),
            guild_id: "guild".to_string(),
            voice_channel_id: "code".to_string(),
            discord_parent_channel_id: "agent-threads".to_string(),
            requested_by_user_id: "user-a".to_string(),
            command,
        },
    );
    let decoded = Job::decode(&session.encode().unwrap()).unwrap();
    assert_eq!(decoded.kind, JobKind::AgentSessionStart);
    assert_eq!(decoded.payload.to_json()["agent_session_id"], "ags_1");

    let publication = Job::transcript_publication(
        "guild",
        "code",
        "user-a",
        TranscriptPublicationPayload {
            publication_id: "pub_1".to_string(),
            live: false,
        },
    );
    let decoded = Job::decode(&publication.encode().unwrap()).unwrap();
    assert_eq!(decoded.kind, JobKind::TranscriptPublication);
    assert_eq!(decoded.payload.to_json()["publication_id"], "pub_1");
}
#[tokio::test(flavor = "current_thread")]
async fn wake_activation_payload_is_a_first_class_binary_job() {
    let payload = WakeActivationPayload {
        activation_id: "act_1".to_string(),
        guild_id: "guild".to_string(),
        voice_channel_id: "code".to_string(),
        voice_channel_name: "Code Lounge".to_string(),
        speaker_user_id: "user-a".to_string(),
        speaker_label: "Will".to_string(),
        wake_event_id: "evt_wake".to_string(),
        wake_started_at: "2026-05-14T12:00:00.000Z".to_string(),
        wake_ended_at: "2026-05-14T12:00:01.000Z".to_string(),
        latest_wake_event_id: "evt_wake".to_string(),
        latest_wake_at: "2026-05-14T12:00:00.000Z".to_string(),
        lookback_seconds: 30,
        min_post_seconds: 5,
        speaker_idle_seconds: 5,
        stt_flush_grace_seconds: 2,
        max_window_seconds: 60,
        additive_preempt_seconds: 10,
        independent_after_seconds: 45,
        amended_wake_event_ids: Vec::new(),
        replacement_of_job_ids: Vec::new(),
    };
    let job = Job::wake_activation(payload);
    let decoded = Job::decode(&job.encode().unwrap()).unwrap();

    assert_eq!(decoded.kind, JobKind::WakeActivation);
    assert_eq!(
        decoded.wake_activation_payload().unwrap().wake_event_id,
        "evt_wake"
    );
}
#[tokio::test(flavor = "current_thread")]
async fn discord_voice_jobs_are_first_class_binary_jobs() {
    let room = RoomConfig {
        room_id: "code-lounge".to_string(),
        guild_id: "guild".to_string(),
        guild_slug: "guild".to_string(),
        channel_id: "code".to_string(),
        channel_slug: "code-lounge".to_string(),
        channel_name: "Code Lounge".to_string(),
        auto_join: true,
    };
    let payload = DiscordVoiceJoinPayload {
        room: room.clone(),
        bot_id: "clanky-vc1".to_string(),
        capture_run_id: "cap_1".to_string(),
        assignment_id: "assign_1".to_string(),
        started_at: Utc::now(),
        session_dir: raw_path("session"),
        requested_by_user_id: "user-a".to_string(),
        reason: "auto_join".to_string(),
    };
    let job = Job::discord_voice_join(payload);
    let decoded = Job::decode(&job.encode().unwrap()).unwrap();

    assert_eq!(decoded.kind, JobKind::DiscordVoiceJoin);
    assert_eq!(
        decoded.discord_voice_join_payload().unwrap().room.room_id,
        room.room_id
    );

    let output = JobOutput::DiscordVoiceLeave(DiscordVoiceLeaveOutput {
        session_id: "cap_1".to_string(),
        status: "ended".to_string(),
        session: None,
        bot_status: None,
        guild_id: "guild".to_string(),
        voice_channel_id: "code".to_string(),
        capture_run_id: "cap_1".to_string(),
        audio_jobs: Vec::new(),
    });
    let mut completed = decoded.clone();
    completed.metadata.output = Some(output);
    let completed = Job::decode(&completed.encode().unwrap()).unwrap();

    assert!(matches!(
        completed.metadata.output,
        Some(JobOutput::DiscordVoiceLeave(_))
    ));

    let playback = Job::discord_voice_playback(
        "guild",
        "code",
        "user-a",
        DiscordVoicePlaybackPayload {
            session_id: "cap_1".to_string(),
            cue: DiscordVoicePlaybackCue::Deafen,
            source_job_id: "job_parent".to_string(),
            reason: "deafen_listening".to_string(),
        },
    );
    let decoded = Job::decode(&playback.encode().unwrap()).unwrap();
    assert_eq!(decoded.kind, JobKind::DiscordVoicePlayback);
    let payload = decoded.discord_voice_playback_payload().unwrap();
    assert_eq!(payload.cue, DiscordVoicePlaybackCue::Deafen);
    assert_eq!(payload.cue.asset_file_name(), "clanky-deafen.wav");

    let mut completed = decoded;
    completed.metadata.output = Some(JobOutput::DiscordVoicePlayback(
        DiscordVoicePlaybackOutput {
            session_id: "cap_1".to_string(),
            cue: DiscordVoicePlaybackCue::Undeafen,
            status: "played".to_string(),
            guild_id: "guild".to_string(),
            voice_channel_id: "code".to_string(),
            audio_path: "/workspace/clankcord/res/audio/clanky-deafen.wav".to_string(),
            duration_ms: 250,
            message: String::new(),
        },
    ));
    let completed = Job::decode(&completed.encode().unwrap()).unwrap();
    assert!(matches!(
        completed.metadata.output,
        Some(JobOutput::DiscordVoicePlayback(_))
    ));

    let mute = Job::discord_voice_mute(
        "guild",
        "code",
        "user-a",
        DiscordVoiceMutePayload {
            session_id: "cap_1".to_string(),
            muted: false,
            source_job_id: "job_parent".to_string(),
            reason: "before_playback".to_string(),
        },
    );
    let decoded = Job::decode(&mute.encode().unwrap()).unwrap();
    assert_eq!(decoded.kind, JobKind::DiscordVoiceMute);
    assert!(!decoded.discord_voice_mute_payload().unwrap().muted);

    let mut completed = decoded;
    completed.metadata.output = Some(JobOutput::DiscordVoiceMute(DiscordVoiceMuteOutput {
        session_id: "cap_1".to_string(),
        muted: false,
        status: "set".to_string(),
        guild_id: "guild".to_string(),
        voice_channel_id: "code".to_string(),
        message: String::new(),
    }));
    let completed = Job::decode(&completed.encode().unwrap()).unwrap();
    assert!(matches!(
        completed.metadata.output,
        Some(JobOutput::DiscordVoiceMute(_))
    ));

    let deafen = Job::discord_voice_deafen(
        "guild",
        "code",
        "user-a",
        DiscordVoiceDeafenPayload {
            session_id: "cap_1".to_string(),
            deafened: true,
            source_job_id: "job_parent".to_string(),
            reason: "deafen_listening".to_string(),
        },
    );
    let decoded = Job::decode(&deafen.encode().unwrap()).unwrap();
    assert_eq!(decoded.kind, JobKind::DiscordVoiceDeafen);
    assert!(decoded.discord_voice_deafen_payload().unwrap().deafened);

    let mut completed = decoded;
    completed.metadata.output = Some(JobOutput::DiscordVoiceDeafen(DiscordVoiceDeafenOutput {
        session_id: "cap_1".to_string(),
        deafened: true,
        status: "set".to_string(),
        guild_id: "guild".to_string(),
        voice_channel_id: "code".to_string(),
        message: String::new(),
    }));
    let completed = Job::decode(&completed.encode().unwrap()).unwrap();
    assert!(matches!(
        completed.metadata.output,
        Some(JobOutput::DiscordVoiceDeafen(_))
    ));

    let play_audio = Job::discord_voice_play_audio(
        "guild",
        "code",
        "user-a",
        DiscordVoicePlayAudioPayload {
            session_id: "cap_1".to_string(),
            cue: DiscordVoicePlaybackCue::Wake,
            source_job_id: "job_parent".to_string(),
            reason: "wake_detected".to_string(),
        },
    );
    let decoded = Job::decode(&play_audio.encode().unwrap()).unwrap();
    assert_eq!(decoded.kind, JobKind::DiscordVoicePlayAudio);
    assert_eq!(
        decoded.discord_voice_play_audio_payload().unwrap().cue,
        DiscordVoicePlaybackCue::Wake
    );

    let mut completed = decoded;
    completed.metadata.output = Some(JobOutput::DiscordVoicePlayAudio(
        DiscordVoicePlayAudioOutput {
            session_id: "cap_1".to_string(),
            cue: DiscordVoicePlaybackCue::Wake,
            status: "played".to_string(),
            guild_id: "guild".to_string(),
            voice_channel_id: "code".to_string(),
            audio_path: "/workspace/clankcord/res/audio/clanky-wake.wav".to_string(),
            duration_ms: 250,
            message: String::new(),
        },
    ));
    let completed = Job::decode(&completed.encode().unwrap()).unwrap();
    assert!(matches!(
        completed.metadata.output,
        Some(JobOutput::DiscordVoicePlayAudio(_))
    ));

    let mut snapshot = Job::discord_voice_status_snapshot("job_parent");
    snapshot.metadata.output = Some(JobOutput::DiscordVoiceStatusSnapshot(
        DiscordVoiceStatusSnapshotOutput {
            bots: Vec::new(),
            sessions: Vec::new(),
            voice_state_guild_ids: vec!["guild".to_string()],
            voice_states: vec![OpaqueValue::from_json(&json!({
                "guild_id": "guild",
                "voice_channel_id": "code",
                "user_id": "user-a"
            }))],
        },
    ));
    let snapshot = Job::decode(&snapshot.encode().unwrap()).unwrap();
    let Some(JobOutput::DiscordVoiceStatusSnapshot(output)) = snapshot.metadata.output else {
        panic!("decoded snapshot output");
    };
    assert_eq!(output.voice_states.len(), 1);
    assert_eq!(output.voice_states[0].to_json()["voice_channel_id"], "code");
}
