use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct GuildConfig {
    #[serde(rename = "guildId")]
    pub guild_id: String,
    #[serde(rename = "guildSlug")]
    pub guild_slug: String,
    #[serde(rename = "idleChannelId")]
    pub idle_channel_id: String,
    #[serde(rename = "idleChannelName")]
    pub idle_channel_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ControlConfig {
    #[serde(default)]
    pub guild_id: String,
    #[serde(default)]
    pub guild_slug: String,
    #[serde(default)]
    pub default_voice_room_id: String,
    #[serde(default)]
    pub bots_channel_id: String,
    #[serde(default)]
    pub agent_threads_channel_id: String,
    #[serde(default)]
    pub transcripts_forum_id: String,
    #[serde(default)]
    pub thread_auto_archive_minutes: i64,
}

impl ControlConfig {
    pub fn from_json(value: Value) -> Self {
        serde_json::from_value(value).unwrap_or_default()
    }
}
