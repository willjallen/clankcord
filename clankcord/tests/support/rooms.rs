//! Room and voice-assignment fixtures shared across category binaries.

use clankcord::domain::Ctx;
use clankcord::model::rooms::RoomConfig;
use clankcord::model::voice::VoiceBotStatus;
pub fn room_runtime(timeline_store: clankcord::store::TimelineStore, _room: RoomConfig) -> Ctx {
    Ctx::new(timeline_store)
}
pub fn test_room() -> RoomConfig {
    RoomConfig {
        room_id: "code-lounge".to_string(),
        guild_id: "guild".to_string(),
        guild_slug: "guild".to_string(),
        channel_id: "code".to_string(),
        channel_slug: "code-lounge".to_string(),
        channel_name: "Code Lounge".to_string(),
        auto_join: true,
    }
}
pub fn ready_bot_with(bot_id: &str, user_id: &str) -> VoiceBotStatus {
    VoiceBotStatus {
        bot_id: bot_id.to_string(),
        ready: true,
        user_id: user_id.to_string(),
        username: bot_id.to_string(),
        ..VoiceBotStatus::default()
    }
}
