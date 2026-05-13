#![allow(dead_code)]

use std::path::{Path, PathBuf};

use chrono::{TimeZone, Utc};
use serde_json::Value;

use clankcord::adapters::discord::voice::types::VoiceSession;
use clankcord::runtime::timeline::{SpeechEventInput, TimelineStore};
use clankcord::runtime::{RoomConfig, RuntimeSessionStatus};

pub(crate) fn dt(
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
) -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(year, month, day, hour, minute, second)
        .unwrap()
}

pub(crate) fn append_speech(
    store: &TimelineStore,
    raw_root: &Path,
    start: chrono::DateTime<Utc>,
    end: chrono::DateTime<Utc>,
    text: &str,
    segment_index: i64,
    source_audio_path: Option<PathBuf>,
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
            speaker_user_id: "user-a".to_string(),
            speaker_label: "Will".to_string(),
            speaker_username: "will".to_string(),
            segment_start_time: start,
            segment_end_time: end,
            text_draft: text.to_string(),
            source_audio_path: source_audio_path
                .unwrap_or_else(|| raw_root.join(format!("source-{segment_index}.wav"))),
            audio_checksum: "sha256:test".to_string(),
            segment_index,
            duration_ms: (end - start).num_milliseconds(),
            ..Default::default()
        })
        .unwrap()
}

pub(crate) fn test_voice_session(raw_root: &Path) -> VoiceSession {
    VoiceSession {
        session_id: "cap_test".to_string(),
        room: RoomConfig {
            room_id: "code-lounge".to_string(),
            guild_id: "guild".to_string(),
            guild_slug: "guild".to_string(),
            channel_id: "code".to_string(),
            channel_slug: "code-lounge".to_string(),
            channel_name: "Code Lounge".to_string(),
            auto_join: true,
        },
        bot_id: "clanky-vc1".to_string(),
        bot_user_id: "bot-user".to_string(),
        thread_id: String::new(),
        thread_name: String::new(),
        started_at: dt(2026, 5, 12, 16, 0, 0),
        session_dir: raw_root.join("session"),
        minute_message_ids: Default::default(),
        participants: Default::default(),
        buffers: Default::default(),
        packet_debug: clankcord::adapters::discord::voice::diagnostics::default_packet_debug(),
        debug_notes: Default::default(),
        segment_counter: 0,
        audio_segments: Vec::new(),
        transcription_task_ids: Default::default(),
        finalizing: false,
        ended_at: None,
        voice_channel_id: "code".to_string(),
        transcript_event_count: 0,
        last_pcm_at: None,
        last_transcript_at: None,
        last_pcm_monotonic: 0.0,
        last_transcript_monotonic: 0.0,
        last_stall_log_monotonic: 0.0,
        voice_client_debug: Default::default(),
        capture_run_id: "cap_test".to_string(),
        assignment_id: String::new(),
        mode: "local_buffering".to_string(),
    }
}

pub(crate) fn merge_json(base: &Value, extra: Value) -> Value {
    let mut map = base.as_object().unwrap().clone();
    map.extend(extra.as_object().unwrap().clone());
    Value::Object(map)
}

#[allow(dead_code)]
pub(crate) fn ended(session: &mut RuntimeSessionStatus) {
    session.mark_ended(clankcord::runtime::timeline::isoformat_z(None));
}
