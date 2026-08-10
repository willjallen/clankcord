//! Dashboard fixtures shared across category binaries.

use chrono::{Duration, Utc};
use clankcord::store::SpeechEventInput;

#[allow(clippy::too_many_arguments)] // parameter-struct cleanup tracked in WORKING_PLAN
pub async fn append_dashboard_speech(
    store: &clankcord::store::TimelineStore,
    raw_root: &std::path::Path,
    voice_channel_id: &str,
    voice_channel_name: &str,
    voice_channel_slug: &str,
    start: chrono::DateTime<Utc>,
    text: &str,
    segment_index: i64,
) {
    store
        .append_speech_event(SpeechEventInput {
            guild_id: "guild".to_string(),
            guild_slug: "guild".to_string(),
            voice_channel_id: voice_channel_id.to_string(),
            voice_channel_name: voice_channel_name.to_string(),
            voice_channel_slug: voice_channel_slug.to_string(),
            capture_run_id: format!("cap_{voice_channel_id}"),
            voice_bot_id: "clanky-vc1".to_string(),
            voice_bot_discord_user_id: "bot-user".to_string(),
            speaker_user_id: "user-a".to_string(),
            speaker_label: "Will".to_string(),
            speaker_username: "will".to_string(),
            segment_start_time: start,
            segment_end_time: start + Duration::seconds(1),
            text_draft: text.to_string(),
            source_audio_path: raw_root.join(format!("dashboard-{segment_index}.wav")),
            audio_checksum: "sha256:test".to_string(),
            segment_index,
            duration_ms: 1000,
            ..Default::default()
        })
        .await
        .unwrap();
}
