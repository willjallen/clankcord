use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::Result;
use crate::config::{
    config_path, load_control_config, non_empty, read_json, slugify, string_field, string_value,
};
use crate::runtime::timeline::{TimelineStore, utc_now};

use crate::runtime::util::parse_duration_seconds;
use crate::runtime::{AgentRuntime, ControlConfig, GuildConfig, RoomConfig, Runtime};

impl Runtime {
    pub fn new() -> Result<Self> {
        let mut runtime = Self {
            started_at: utc_now(),
            guilds: BTreeMap::new(),
            rooms: BTreeMap::new(),
            control_config: ControlConfig::default(),
            room_controls: BTreeMap::new(),
            sessions: BTreeMap::new(),
            bots: BTreeMap::new(),
            agents: AgentRuntime::default(),
            timeline_store: TimelineStore::new(None)?,
            auto_join_enabled: true,
            manual_leave_cooldown_seconds: 20 * 60,
            manual_join_hold_seconds: 60 * 60,
            pause_release_seconds: 20 * 60,
        };
        runtime.reload_config()?;
        Ok(runtime)
    }

    pub async fn start(&mut self) -> Result<()> {
        self.reload_config()
    }

    pub async fn stop(&mut self) -> Result<()> {
        Ok(())
    }

    pub fn reload_config(&mut self) -> Result<()> {
        let payload = read_json(&config_path(), json!({}));
        let pool_config = payload
            .get("pool")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let default_idle_name = string_value(pool_config.get("idleChannelName"))
            .trim()
            .to_string();
        let default_idle_name = if default_idle_name.is_empty() {
            "idle".to_string()
        } else {
            default_idle_name
        };
        if let Some(auto_join) = pool_config.get("autoJoin").and_then(Value::as_object) {
            self.auto_join_enabled = auto_join
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            self.manual_leave_cooldown_seconds = parse_duration_seconds(
                auto_join.get("manualLeaveCooldown"),
                self.manual_leave_cooldown_seconds,
            );
            self.manual_join_hold_seconds = parse_duration_seconds(
                auto_join.get("manualJoinHold"),
                self.manual_join_hold_seconds,
            );
        }

        let mut guilds = BTreeMap::new();
        for raw in payload
            .get("guilds")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            let guild_id = string_field(&raw, "guildId");
            if guild_id.is_empty() {
                continue;
            }
            guilds.insert(
                guild_id.clone(),
                GuildConfig {
                    guild_id: guild_id.clone(),
                    guild_slug: non_empty(string_field(&raw, "guildSlug"), slugify(&guild_id)),
                    idle_channel_id: string_field(&raw, "idleChannelId"),
                    idle_channel_name: non_empty(
                        string_field(&raw, "idleChannelName"),
                        default_idle_name.clone(),
                    ),
                },
            );
        }

        let rooms_payload = read_json(
            &crate::config::rooms_path(),
            json!({
                "rooms": []
            }),
        );
        let mut rooms = BTreeMap::new();
        for raw in rooms_payload
            .get("rooms")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            let guild_id = string_field(&raw, "guildId");
            let channel_id = string_field(&raw, "channelId");
            if guild_id.is_empty() || channel_id.is_empty() {
                continue;
            }
            let channel_name = non_empty(string_field(&raw, "channelName"), channel_id.clone());
            let guild_slug = non_empty(string_field(&raw, "guildSlug"), slugify(&guild_id));
            let channel_slug = non_empty(string_field(&raw, "channelSlug"), slugify(&channel_name));
            let room_id = non_empty(string_field(&raw, "id"), channel_slug.clone());
            rooms.insert(
                room_id.clone(),
                RoomConfig {
                    room_id,
                    guild_id: guild_id.clone(),
                    guild_slug: guild_slug.clone(),
                    channel_id: channel_id.clone(),
                    channel_slug,
                    channel_name,
                    auto_join: raw.get("autoJoin").and_then(Value::as_bool).unwrap_or(true),
                },
            );
            guilds
                .entry(guild_id.clone())
                .or_insert_with(|| GuildConfig {
                    guild_id: guild_id.clone(),
                    guild_slug,
                    idle_channel_id: String::new(),
                    idle_channel_name: default_idle_name.clone(),
                });
        }

        self.guilds = guilds;
        self.rooms = rooms;
        self.control_config = ControlConfig::from_json(load_control_config());
        self.load_room_controls();
        self.load_status_snapshot();
        Ok(())
    }
}
