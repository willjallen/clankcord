use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde_json::{Value, json};

use crate::Result;
use crate::config::{read_json, room_controls_path, write_json};
use crate::runtime::timeline::{isoformat_z, parse_instant, utc_now};

use crate::runtime::{RoomConfig, RoomControl, Runtime};

impl Runtime {
    pub fn pause_room(
        &mut self,
        room: &RoomConfig,
        duration_seconds: i64,
        requested_by_user_id: &str,
    ) -> Result<Value> {
        self.set_room_listening_pause(
            room,
            duration_seconds,
            "manual_pause",
            requested_by_user_id,
        )?;
        let event = self.timeline_store.append_event(
            &room.guild_id,
            &room.channel_id,
            json!({
                "event_kind": "listening_paused",
                "kind": "listening_paused",
                "duration_seconds": duration_seconds,
                "requested_by_user_id": requested_by_user_id,
            }),
        )?;
        Ok(json!({"action": "pause", "status": "paused", "roomId": room.room_id, "event": event}))
    }

    pub fn resume_room(&mut self, room: &RoomConfig, requested_by_user_id: &str) -> Result<Value> {
        self.clear_room_controls(room, &["listening_paused_until"])?;
        let event = self.timeline_store.append_event(
            &room.guild_id,
            &room.channel_id,
            json!({
                "event_kind": "listening_resumed",
                "kind": "listening_resumed",
                "requested_by_user_id": requested_by_user_id,
            }),
        )?;
        Ok(json!({"action": "resume", "status": "resumed", "roomId": room.room_id, "event": event}))
    }

    pub fn load_room_controls(&mut self) {
        let payload = read_json(&room_controls_path(), json!({"rooms": {}}));
        let rooms_payload = payload
            .get("rooms")
            .or(Some(&payload))
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        self.room_controls = rooms_payload
            .into_iter()
            .filter_map(|(key, value)| {
                let control = serde_json::from_value::<RoomControl>(value).ok()?;
                (!key.trim().is_empty() && control.has_active_marker()).then_some((key, control))
            })
            .collect();
        self.prune_expired_room_controls(false);
    }

    pub fn save_room_controls(&self) -> Result<()> {
        write_json(
            &room_controls_path(),
            &json!({"updated_at": isoformat_z(None), "rooms": self.room_controls_json()}),
        )
    }

    pub fn room_controls_json(&self) -> BTreeMap<String, Value> {
        self.room_controls
            .iter()
            .map(|(key, control)| (key.clone(), control.to_json()))
            .collect()
    }

    pub fn prune_expired_room_controls(&mut self, save: bool) {
        let now = utc_now();
        let keys = [
            "auto_join_suppressed_until",
            "manual_hold_until",
            "listening_paused_until",
        ];
        let mut changed = false;
        for control in self.room_controls.values_mut() {
            for key in keys {
                if room_control_datetime(control, key).is_some_and(|value| value <= now) {
                    control.clear_key(key);
                    changed = true;
                }
            }
        }
        self.room_controls
            .retain(|_, value| value.has_active_marker());
        if changed && save {
            let _ = self.save_room_controls();
        }
    }

    pub fn room_control_datetime_active(&self, channel_id: &str, key: &str) -> bool {
        self.room_controls
            .get(channel_id)
            .and_then(|value| room_control_datetime(value, key))
            .is_some_and(|value| value > utc_now())
    }

    pub fn room_control_status(&self, room: &RoomConfig) -> Value {
        let control = self
            .room_controls
            .get(&room.channel_id)
            .map(RoomControl::to_json)
            .unwrap_or_else(|| json!({}));
        json!({
            "autoJoinSuppressed": self.room_control_datetime_active(&room.channel_id, "auto_join_suppressed_until"),
            "manualHoldActive": self.room_control_datetime_active(&room.channel_id, "manual_hold_until"),
            "listeningPaused": self.room_control_datetime_active(&room.channel_id, "listening_paused_until"),
            "control": control,
            "cooldownActive": false,
        })
    }

    pub fn update_room_control(
        &mut self,
        room: &RoomConfig,
        clear_keys: &[&str],
        apply: impl FnOnce(&mut RoomControl),
    ) -> Result<RoomControl> {
        let mut control = self
            .room_controls
            .get(&room.channel_id)
            .cloned()
            .unwrap_or_default();
        for key in clear_keys {
            control.clear_key(key);
        }
        apply(&mut control);
        control.guild_id = room.guild_id.clone();
        control.voice_channel_id = room.channel_id.clone();
        control.voice_channel_name = room.channel_name.clone();
        control.updated_at = isoformat_z(None);
        self.room_controls
            .insert(room.channel_id.clone(), control.clone());
        self.prune_expired_room_controls(false);
        self.save_room_controls()?;
        Ok(control)
    }

    pub fn suppress_room_auto_join(
        &mut self,
        room: &RoomConfig,
        duration_seconds: i64,
        reason: &str,
        requested_by_user_id: &str,
        clear_manual_hold: bool,
    ) -> Result<RoomControl> {
        let until = utc_now() + chrono::Duration::seconds(duration_seconds.max(0));
        let clear = if clear_manual_hold {
            vec!["manual_hold_until"]
        } else {
            Vec::new()
        };
        let control = self.update_room_control(room, &clear, |control| {
            control.auto_join_suppressed_until = Some(isoformat_z(Some(until)));
            control.auto_join_suppression_reason = Some(reason.to_string());
            control.auto_join_suppressed_by_user_id = Some(requested_by_user_id.to_string());
        })?;
        let _ = self.timeline_store.append_event(
            &room.guild_id,
            &room.channel_id,
            json!({
                "event_kind": "room_auto_join_suppressed",
                "kind": "room_auto_join_suppressed",
                "duration_seconds": duration_seconds.max(0),
                "until": control.auto_join_suppressed_until.clone().unwrap_or_default(),
                "reason": reason,
                "requested_by_user_id": requested_by_user_id,
            }),
        );
        Ok(control)
    }

    pub fn set_room_manual_hold(
        &mut self,
        room: &RoomConfig,
        duration_seconds: i64,
        reason: &str,
        requested_by_user_id: &str,
    ) -> Result<RoomControl> {
        let until = utc_now() + chrono::Duration::seconds(duration_seconds.max(0));
        let control = self.update_room_control(
            room,
            &["auto_join_suppressed_until", "listening_paused_until"],
            |control| {
                control.manual_hold_until = Some(isoformat_z(Some(until)));
                control.manual_hold_reason = Some(reason.to_string());
                control.manual_hold_by_user_id = Some(requested_by_user_id.to_string());
            },
        )?;
        let _ = self.timeline_store.append_event(
            &room.guild_id,
            &room.channel_id,
            json!({
                "event_kind": "room_manual_hold_set",
                "kind": "room_manual_hold_set",
                "duration_seconds": duration_seconds.max(0),
                "until": control.manual_hold_until.clone().unwrap_or_default(),
                "reason": reason,
                "requested_by_user_id": requested_by_user_id,
            }),
        );
        Ok(control)
    }

    pub fn set_room_listening_pause(
        &mut self,
        room: &RoomConfig,
        duration_seconds: i64,
        reason: &str,
        requested_by_user_id: &str,
    ) -> Result<RoomControl> {
        let until = utc_now() + chrono::Duration::seconds(duration_seconds.max(0));
        self.update_room_control(room, &[], |control| {
            control.listening_paused_until = Some(isoformat_z(Some(until)));
            control.listening_pause_reason = Some(reason.to_string());
            control.listening_paused_by_user_id = Some(requested_by_user_id.to_string());
        })
    }

    pub fn clear_room_controls(&mut self, room: &RoomConfig, keys: &[&str]) -> Result<()> {
        let Some(control) = self.room_controls.get_mut(&room.channel_id) else {
            return Ok(());
        };
        for key in keys {
            control.clear_key(key);
        }
        if !control.has_active_marker() {
            self.room_controls.remove(&room.channel_id);
        }
        self.save_room_controls()
    }
}

fn room_control_datetime(control: &RoomControl, key: &str) -> Option<DateTime<Utc>> {
    let value = match key {
        "auto_join_suppressed_until" => control.auto_join_suppressed_until.as_deref(),
        "manual_hold_until" => control.manual_hold_until.as_deref(),
        "listening_paused_until" => control.listening_paused_until.as_deref(),
        _ => None,
    }?;
    parse_instant(value)
}
