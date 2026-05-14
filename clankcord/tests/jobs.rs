use chrono::{Duration, SecondsFormat, TimeZone, Utc};
use serde_json::json;

use clankcord::runtime::timeline::TimelineStore;
use clankcord::runtime::{
    AudioSegmentPayload, BinaryPayload, CommandRequest, DiscordVoiceJoinPayload,
    DiscordVoiceLeaveOutput, DiscordVoiceMuteOutput, DiscordVoiceMutePayload,
    DiscordVoicePlayAudioOutput, DiscordVoicePlayAudioPayload, DiscordVoicePlaybackCue,
    DiscordVoicePlaybackOutput, DiscordVoicePlaybackPayload, Job, JobKind, JobOutput, JobPayload,
    JobState, RefineTranscriptPayload, ResponseKind, ResponsePayload, ResponseSinkKind, RoomConfig,
    WakeActivationPayload,
};

#[test]
fn job_round_trips_as_binary_record() {
    let command = CommandRequest::from_json(&json!({
        "command_kind": "agent_task",
        "guild_id": "guild",
        "voice_channel_id": "channel",
        "requested_by_user_id": "requester",
        "arguments": {"question": "what happened?", "relative_start": "-20m"}
    }))
    .unwrap();
    let job = Job::agent_task("guild", "channel", "requester", command);

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
fn audio_segment_payload_references_ready_audio_artifact() {
    let start = chrono::Utc.with_ymd_and_hms(2026, 5, 13, 12, 0, 0).unwrap();
    let source_audio_path = std::path::PathBuf::from("/tmp/clankcord/segment.wav");
    let job = Job::audio_segment(AudioSegmentPayload {
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
        segment_start_time: start,
        segment_end_time: start + chrono::Duration::milliseconds(20),
        segment_index: 7,
        duration_ms: 20,
        source_audio_path: source_audio_path.clone(),
        audio_checksum: "sha256:test".to_string(),
        audio_bytes: 44,
        audio_format: "wav".to_string(),
        sample_rate_hz: 48_000,
        channels: 2,
        sample_width_bits: 16,
        post_processing: "pcm_s16le_to_wav".to_string(),
    });

    assert_eq!(job.kind, JobKind::AudioSegment);
    assert_eq!(
        job.audio_segment_payload().unwrap().source_audio_path,
        source_audio_path
    );
    let payload = job.payload_value();
    assert_eq!(
        payload["source_audio_path"],
        json!("/tmp/clankcord/segment.wav")
    );
    assert_eq!(payload["audio_bytes"], json!(44));
    assert!(payload.get("pcm").is_none());
}

#[test]
fn opaque_json_lowers_to_binary_payload() {
    let payload = BinaryPayload::from_json(&json!({"nested": ["value", 1]})).unwrap();
    assert!(!payload.as_bytes().is_empty());
    assert_eq!(payload.to_json(), json!({"nested": ["value", 1]}));
}

#[test]
fn job_lineage_allows_arbitrary_dag_depth_metadata() {
    let root = Job::new(
        "guild",
        "channel",
        "requester",
        JobState::Queued,
        JobPayload::RefineTranscript(RefineTranscriptPayload {
            window_id: "root".to_string(),
            publication_id: "pub".to_string(),
        }),
    );
    let mut child = Job::refine_transcript("guild", "channel", "requester", "child", "pub");
    child.attach_to_parent(&root).unwrap();
    let mut grandchild =
        Job::refine_transcript("guild", "channel", "requester", "grandchild", "pub");
    grandchild.attach_to_parent(&child).unwrap();
    let mut too_deep = Job::refine_transcript("guild", "channel", "requester", "deep", "pub");
    too_deep.attach_to_parent(&grandchild).unwrap();

    assert_eq!(child.parent_job_id.as_deref(), Some(root.id.as_str()));
    assert_eq!(child.root_job_id, root.id);
    assert_eq!(child.lineage_depth, 1);
    assert_eq!(grandchild.parent_job_id.as_deref(), Some(child.id.as_str()));
    assert_eq!(grandchild.root_job_id, child.root_job_id);
    assert_eq!(grandchild.lineage_depth, 2);
    assert_eq!(
        too_deep.parent_job_id.as_deref(),
        Some(grandchild.id.as_str())
    );
    assert_eq!(too_deep.root_job_id, child.root_job_id);
    assert_eq!(too_deep.lineage_depth, 3);
}

#[test]
fn response_payload_is_a_first_class_binary_job() {
    let payload = ResponsePayload::from_json(&json!({
        "response_kind": "question",
        "sink": "agent-chat",
        "source_job_id": "job_source",
        "requested_by_user_id": "user-a",
        "content": "Do you mean the last 20 minutes?",
        "extra_boundary_field": {"kept": true}
    }))
    .unwrap();
    let job = Job::response("guild", "code", "user-a", payload);
    let decoded = Job::decode(&job.encode().unwrap()).unwrap();

    assert_eq!(decoded.kind, JobKind::Response);
    let response = decoded.response_payload().unwrap();
    assert_eq!(response.response_kind, ResponseKind::Question);
    assert_eq!(response.sink.kind, ResponseSinkKind::AgentChat);
    assert_eq!(response.source_job_id, "job_source");
    assert_eq!(
        response.to_json()["extra_boundary_field"]["kept"],
        json!(true)
    );
}

#[test]
fn wake_activation_payload_is_a_first_class_binary_job() {
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

#[test]
fn timeline_claim_due_jobs_marks_running_without_claiming_future_jobs() {
    let raw = tempfile::tempdir().unwrap();
    let store = TimelineStore::new(Some(raw.path().join("voice"))).unwrap();
    let due = Job::response("guild", "code", "user-a", response_payload("due"));
    let due_id = due.id.clone();
    let mut future = Job::response("guild", "code", "user-a", response_payload("future"));
    let future_id = future.id.clone();
    future.next_run_at =
        Some((Utc::now() + Duration::minutes(5)).to_rfc3339_opts(SecondsFormat::Millis, true));

    store.create_job(future).unwrap();
    store.create_job(due).unwrap();

    let claimed = store
        .claim_due_jobs(JobKind::Response, 8, |_| false)
        .unwrap();

    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].id, due_id);
    assert_eq!(claimed[0].state, JobState::Running);
    assert_eq!(store.get_job(&due_id).unwrap().state, JobState::Running);
    assert_eq!(store.get_job(&future_id).unwrap().state, JobState::Queued);
    assert!(
        store
            .claim_due_jobs(JobKind::Response, 8, |_| false)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn timeline_claim_due_jobs_can_skip_active_agent_sessions() {
    let raw = tempfile::tempdir().unwrap();
    let store = TimelineStore::new(Some(raw.path().join("voice"))).unwrap();
    let command = CommandRequest::from_json(&json!({
        "command_kind": "agent_task",
        "guild_id": "guild",
        "voice_channel_id": "code",
        "requested_by_user_id": "user-a",
        "arguments": {"question": "summarize this"}
    }))
    .unwrap();
    let job = Job::agent_task("guild", "code", "user-a", command);
    let job_id = job.id.clone();
    store.create_job(job).unwrap();

    let skipped = store
        .claim_due_jobs(JobKind::AgentTask, 4, |job| job.voice_channel_id == "code")
        .unwrap();

    assert!(skipped.is_empty());
    assert_eq!(store.get_job(&job_id).unwrap().state, JobState::Queued);

    let claimed = store
        .claim_due_jobs(JobKind::AgentTask, 4, |_| false)
        .unwrap();
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].id, job_id);
    assert_eq!(store.get_job(&job_id).unwrap().state, JobState::Running);
}

#[test]
fn timeline_child_jobs_are_stored_as_dependency_edges() {
    let raw = tempfile::tempdir().unwrap();
    let store = TimelineStore::new(Some(raw.path().join("voice"))).unwrap();
    let parent = store
        .create_job(Job::response(
            "guild",
            "code",
            "user-a",
            response_payload("parent"),
        ))
        .unwrap();
    let child = Job::response("guild", "code", "user-a", response_payload("child"));
    let child_id = child.id.clone();

    store.create_child_job(&parent, child).unwrap();

    let parent = store.get_job(&parent.id).unwrap();
    assert_eq!(parent.state, JobState::Waiting);
    let children = store.list_child_jobs(&parent.id).unwrap();
    assert_eq!(children.len(), 1);
    assert_eq!(children[0].id, child_id);
    assert_eq!(
        children[0].parent_job_id.as_deref(),
        Some(parent.id.as_str())
    );
}

#[test]
fn discord_voice_jobs_are_first_class_binary_jobs() {
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
}

fn response_payload(content: &str) -> ResponsePayload {
    ResponsePayload::from_json(&json!({
        "response_kind": "message",
        "sink": "stdout",
        "requested_by_user_id": "user-a",
        "content": content,
    }))
    .unwrap()
}

fn raw_path(path: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(path)
}
