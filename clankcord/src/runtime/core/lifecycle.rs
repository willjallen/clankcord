use std::collections::BTreeMap;

use crate::Result;
use crate::config;
use crate::runtime::timeline::{TimelineStore, utc_now};

use crate::runtime::{AgentRuntime, ControlConfig, Runtime};

impl Runtime {
    pub fn new() -> Result<Self> {
        Self::from_store(TimelineStore::new(None)?)
    }

    pub fn from_store(timeline_store: TimelineStore) -> Result<Self> {
        let pool = config::runtime_pool_config();
        let mut runtime = Self {
            started_at: utc_now(),
            guilds: BTreeMap::new(),
            rooms: BTreeMap::new(),
            control_config: ControlConfig::default(),
            agents: AgentRuntime::default(),
            automations: BTreeMap::new(),
            timeline_store,
            auto_join_enabled: pool.auto_join_enabled,
            manual_leave_cooldown_seconds: pool.manual_leave_cooldown_seconds,
            manual_join_hold_seconds: pool.manual_join_hold_seconds,
            pause_release_seconds: pool.pause_release_seconds,
        };
        runtime.reload_config()?;
        Ok(runtime)
    }

    pub async fn start(&mut self) -> Result<()> {
        self.reload_config()?;
        self.load_automation_registry().await
    }

    pub async fn stop(&mut self) -> Result<()> {
        Ok(())
    }

    pub fn reload_config(&mut self) -> Result<()> {
        let pool = config::runtime_pool_config();
        self.auto_join_enabled = pool.auto_join_enabled;
        self.manual_leave_cooldown_seconds = pool.manual_leave_cooldown_seconds;
        self.manual_join_hold_seconds = pool.manual_join_hold_seconds;
        self.pause_release_seconds = pool.pause_release_seconds;
        self.guilds = config::guild_configs()
            .into_iter()
            .map(|guild| (guild.guild_id.clone(), guild))
            .collect();
        self.rooms = config::room_configs()
            .into_iter()
            .map(|room| (room.room_id.clone(), room))
            .collect();
        self.control_config = config::control_config();
        Ok(())
    }
}
