use std::collections::BTreeMap;

use chrono::{DateTime, Utc};

use crate::runtime::automations::AutomationRecord;
use crate::runtime::timeline::TimelineStore;
use crate::runtime::{
    AgentRuntime, ControlConfig, GuildConfig, RoomConfig, VoiceAssignment, VoiceBotStatus,
    VoiceCaptureSessionStatus,
};

#[derive(Debug, Clone)]
pub struct Runtime {
    pub started_at: DateTime<Utc>,
    pub guilds: BTreeMap<String, GuildConfig>,
    pub rooms: BTreeMap<String, RoomConfig>,
    pub control_config: ControlConfig,
    pub sessions: BTreeMap<String, VoiceCaptureSessionStatus>,
    pub bots: BTreeMap<String, VoiceBotStatus>,
    pub assignments: BTreeMap<String, VoiceAssignment>,
    pub agents: AgentRuntime,
    pub automations: BTreeMap<String, AutomationRecord>,
    pub timeline_store: TimelineStore,
    pub auto_join_enabled: bool,
    pub manual_leave_cooldown_seconds: i64,
    pub manual_join_hold_seconds: i64,
    pub pause_release_seconds: i64,
}
