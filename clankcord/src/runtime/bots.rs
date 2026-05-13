use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeBotStatus {
    pub bot_id: String,
    pub ready: bool,
    pub joining_session_id: String,
    pub assigned_session_id: String,
    pub current_guild_id: String,
    pub current_channel_id: String,
    pub last_error: String,
    pub pending_disconnect_events: i64,
    pub pending_disconnect_until: i64,
    pub user_id: String,
    pub username: String,
    pub gateway_running: bool,
    pub receive_backend: String,
}

impl RuntimeBotStatus {
    pub fn to_json(&self) -> Value {
        serde_json::to_value(self).unwrap_or_else(|_| json!({}))
    }
}
