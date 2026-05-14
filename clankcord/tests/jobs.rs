use chrono::TimeZone;
use serde_json::json;

use clankcord::runtime::{
    AudioSegmentPayload, BinaryPayload, CommandRequest, Job, JobKind, JobPayload, JobState,
    RefineTranscriptPayload,
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
fn job_lineage_is_bounded_to_grandchildren() {
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

    assert_eq!(child.parent_job_id.as_deref(), Some(root.id.as_str()));
    assert_eq!(child.root_job_id, root.id);
    assert_eq!(child.lineage_depth, 1);
    assert_eq!(grandchild.parent_job_id.as_deref(), Some(child.id.as_str()));
    assert_eq!(grandchild.root_job_id, child.root_job_id);
    assert_eq!(grandchild.lineage_depth, 2);
    assert!(too_deep.attach_to_parent(&grandchild).is_err());
}
