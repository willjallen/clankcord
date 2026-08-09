//! Voice transport seam.
//!
//! The Discord voice connection layer reduces serenity/songbird events to
//! these plain facts at the boundary; the capture engine consumes them
//! without seeing provider types.

use serde_json::Value;

/// A voice bot's gateway session came online.
#[derive(Debug, Clone)]
pub struct VoiceClientReady {
    pub user_id: String,
    pub username: String,
}

/// A member profile as observed on a voice event.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VoiceMemberProfile {
    pub id: String,
    pub display_name: String,
    pub global_name: String,
    pub name: String,
}

/// One side of a Discord voice-state transition. `payload` is the canonical
/// voice_state event JSON recorded on the timeline.
#[derive(Debug, Clone)]
pub struct VoiceStateInfo {
    pub user_id: String,
    pub guild_id: String,
    pub channel_id: String,
    pub member_profile: Option<VoiceMemberProfile>,
    pub payload: Value,
}

/// A decoded voice packet for one SSRC within a tick.
#[derive(Debug, Clone)]
pub struct VoicePacket {
    pub pcm: Vec<u8>,
    pub has_packet: bool,
}
