//! Wake-activation fixtures shared across category binaries.

use chrono::SecondsFormat;
use clankcord::domain::Ctx;
use clankcord::domain::agents::AgentSessionRecord;
use clankcord::store::SpeechEventInput;
use clankcord::store::TimelineStore;
use serde_json::Value;

pub fn test_runtime(timeline_store: TimelineStore) -> Ctx {
    Ctx::new(timeline_store)
}

pub async fn insert_agent_session(store: &TimelineStore) {
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

pub fn string_field(value: &Value, key: &str) -> String {
    match value.get(key) {
        Some(Value::String(text)) => text.trim().to_string(),
        Some(Value::Number(number)) => number.to_string(),
        Some(Value::Bool(boolean)) => boolean.to_string(),
        _ => String::new(),
    }
}

#[allow(clippy::too_many_arguments)] // parameter-struct cleanup tracked in WORKING_PLAN
pub async fn append_event(
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
