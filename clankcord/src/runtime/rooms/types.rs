use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct RoomConfig {
    #[serde(rename = "roomId")]
    pub room_id: String,
    #[serde(rename = "guildId")]
    pub guild_id: String,
    #[serde(rename = "guildSlug")]
    pub guild_slug: String,
    #[serde(rename = "channelId")]
    pub channel_id: String,
    #[serde(rename = "channelSlug")]
    pub channel_slug: String,
    #[serde(rename = "channelName")]
    pub channel_name: String,
    #[serde(default, rename = "autoJoin")]
    pub auto_join: bool,
}

impl RoomConfig {
    pub fn to_json(&self) -> Value {
        json!({
            "roomId": self.room_id,
            "id": self.room_id,
            "guildId": self.guild_id,
            "guild_id": self.guild_id,
            "guildSlug": self.guild_slug,
            "guild_slug": self.guild_slug,
            "channelId": self.channel_id,
            "voice_channel_id": self.channel_id,
            "channelSlug": self.channel_slug,
            "voice_channel_slug": self.channel_slug,
            "channelName": self.channel_name,
            "voice_channel_name": self.channel_name,
            "autoJoin": self.auto_join,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct RoomControl {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub guild_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub voice_channel_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub voice_channel_name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub updated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_join_suppressed_until: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_join_suppression_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_join_suppressed_by_user_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manual_hold_until: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manual_hold_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manual_hold_by_user_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listening_paused_until: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listening_pause_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listening_paused_by_user_id: Option<String>,
}

impl RoomControl {
    pub fn clear_key(&mut self, key: &str) {
        match key {
            "auto_join_suppressed_until" => {
                self.auto_join_suppressed_until = None;
                self.auto_join_suppression_reason = None;
                self.auto_join_suppressed_by_user_id = None;
            }
            "auto_join_suppression_reason" => self.auto_join_suppression_reason = None,
            "auto_join_suppressed_by_user_id" => self.auto_join_suppressed_by_user_id = None,
            "manual_hold_until" => {
                self.manual_hold_until = None;
                self.manual_hold_reason = None;
                self.manual_hold_by_user_id = None;
            }
            "manual_hold_reason" => self.manual_hold_reason = None,
            "manual_hold_by_user_id" => self.manual_hold_by_user_id = None,
            "listening_paused_until" => {
                self.listening_paused_until = None;
                self.listening_pause_reason = None;
                self.listening_paused_by_user_id = None;
            }
            "listening_pause_reason" => self.listening_pause_reason = None,
            "listening_paused_by_user_id" => self.listening_paused_by_user_id = None,
            _ => unreachable!("unknown room control key: {key}"),
        }
    }

    pub fn has_active_marker(&self) -> bool {
        self.auto_join_suppressed_until.is_some()
            || self.manual_hold_until.is_some()
            || self.listening_paused_until.is_some()
    }

    pub fn to_json(&self) -> Value {
        serde_json::to_value(self).unwrap_or_else(|_| json!({}))
    }
}
