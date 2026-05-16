use std::collections::BTreeSet;

use crate::Result;
use crate::errors::discord_tool_error;

use crate::runtime::util::{non_empty, slugify};
use crate::runtime::{RoomConfig, Runtime};

impl Runtime {
    pub async fn known_rooms(&self) -> Result<Vec<RoomConfig>> {
        let mut rooms = self.timeline_store.list_room_configs().await?;
        rooms.sort_by(|a, b| {
            (
                a.guild_slug.as_str(),
                a.channel_slug.as_str(),
                a.channel_id.as_str(),
            )
                .cmp(&(
                    b.guild_slug.as_str(),
                    b.channel_slug.as_str(),
                    b.channel_id.as_str(),
                ))
        });
        Ok(rooms)
    }

    pub fn build_room_config(
        &self,
        guild_id: &str,
        guild_slug: &str,
        channel_id: &str,
        channel_name: &str,
        room_id: &str,
    ) -> RoomConfig {
        let channel_name = non_empty(channel_name.to_string(), channel_id.to_string());
        let channel_slug = non_empty(
            slugify(&channel_name),
            non_empty(slugify(channel_id), channel_id.to_string()),
        );
        RoomConfig {
            room_id: non_empty(room_id.to_string(), channel_slug.clone()),
            guild_id: guild_id.to_string(),
            guild_slug: non_empty(guild_slug.to_string(), slugify(guild_id)),
            channel_id: channel_id.to_string(),
            channel_slug,
            channel_name,
            auto_join: true,
        }
    }

    pub fn normalize_room_identifier(identifier: Option<&str>) -> String {
        let mut raw = identifier.unwrap_or("").trim().to_string();
        if raw.starts_with("<#") && raw.ends_with('>') {
            raw = raw[2..raw.len() - 1].trim().to_string();
        }
        if raw.is_empty() {
            return raw;
        }
        raw.split_whitespace()
            .map(|word| {
                match word
                    .trim_matches(|ch: char| ".,;:!?".contains(ch))
                    .to_lowercase()
                    .as_str()
                {
                    "longue" | "launch" | "lunge" => "lounge".to_string(),
                    _ => word.to_string(),
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    pub fn room_match_keys(room: &RoomConfig) -> BTreeSet<String> {
        let mut keys = BTreeSet::new();
        for raw in [
            &room.room_id,
            &room.channel_slug,
            &room.channel_name,
            &room.channel_id,
        ] {
            let lowered = raw.trim().to_lowercase();
            if !lowered.is_empty() {
                keys.insert(lowered);
            }
            let slugged = slugify(raw);
            if !slugged.is_empty() {
                if let Some(stripped) = slugged
                    .strip_suffix("-lounge")
                    .filter(|value| !value.is_empty())
                {
                    keys.insert(stripped.trim_matches('-').to_string());
                }
                keys.insert(slugged);
            }
        }
        keys
    }

    pub fn room_matches(room: &RoomConfig, wanted: &str) -> bool {
        let lowered = wanted.trim().to_lowercase();
        let slugged = slugify(wanted);
        let keys = Self::room_match_keys(room);
        (!lowered.is_empty() && keys.contains(&lowered))
            || (!slugged.is_empty() && keys.contains(&slugged))
    }

    pub async fn room_for_identifier(&self, identifier: Option<&str>) -> Result<RoomConfig> {
        let control = self.timeline_store.control_config().await?;
        let fallback = control.default_voice_room_id;
        let wanted = Self::normalize_room_identifier(
            identifier
                .filter(|value| !value.trim().is_empty())
                .or(Some(&fallback)),
        );
        let rooms = self.known_rooms().await?;
        if wanted.is_empty() {
            return match rooms.as_slice() {
                [room] => Ok(room.clone()),
                [] => Err(discord_tool_error("room is required")),
                _ => Err(discord_tool_error("room is required")),
            };
        }
        let exact = rooms
            .iter()
            .filter(|room| Self::room_matches(room, &wanted))
            .cloned()
            .collect::<Vec<_>>();
        match exact.as_slice() {
            [room] => return Ok(room.clone()),
            [] => {}
            _ => return Err(discord_tool_error(format!("room is ambiguous: {wanted}"))),
        }
        let wanted_slug = slugify(&wanted);
        let prefix = rooms
            .iter()
            .filter(|room| {
                Self::room_match_keys(room).iter().any(|key| {
                    key.starts_with(&wanted)
                        || (!wanted_slug.is_empty() && key.starts_with(&wanted_slug))
                })
            })
            .cloned()
            .collect::<Vec<_>>();
        match prefix.as_slice() {
            [room] => Ok(room.clone()),
            [] => Err(discord_tool_error(format!("unknown room: {wanted}"))),
            _ => Err(discord_tool_error(format!("room is ambiguous: {wanted}"))),
        }
    }

    pub async fn room_for_channel_ids(
        &self,
        guild_id: &str,
        channel_id: &str,
        channel_name: Option<&str>,
    ) -> Result<RoomConfig> {
        for room in self.known_rooms().await? {
            if room.guild_id == guild_id && room.channel_id == channel_id {
                return Ok(room);
            }
        }
        let guild_slug = self
            .timeline_store
            .list_guild_configs()
            .await?
            .into_iter()
            .find(|guild| guild.guild_id == guild_id)
            .map(|guild| guild.guild_slug.clone())
            .unwrap_or_else(|| slugify(guild_id));
        Ok(self.build_room_config(
            guild_id,
            &guild_slug,
            channel_id,
            channel_name.unwrap_or(channel_id),
            "",
        ))
    }

    pub async fn resolve_room_scope(
        &self,
        guild_id: &str,
        channel: Option<&str>,
    ) -> Result<RoomConfig> {
        let raw_channel = Self::normalize_room_identifier(channel);
        if !raw_channel.is_empty() {
            if let Ok(room) = self.room_for_identifier(Some(&raw_channel)).await {
                if guild_id.is_empty() || room.guild_id == guild_id {
                    return Ok(room);
                }
            }
        }
        if !guild_id.is_empty() && !raw_channel.is_empty() {
            return self
                .room_for_channel_ids(guild_id, &raw_channel, None)
                .await;
        }
        self.room_for_identifier(if raw_channel.is_empty() {
            None
        } else {
            Some(&raw_channel)
        })
        .await
    }
}
