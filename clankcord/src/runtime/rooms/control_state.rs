use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde_json::{Value, json};

use crate::Result;
use crate::runtime::timeline::{isoformat_z, parse_instant, utc_now};

use crate::runtime::{RoomConfig, RoomControl, Runtime};

impl Runtime {
    pub async fn pause_room(
        &mut self,
        room: &RoomConfig,
        duration_seconds: i64,
        requested_by_user_id: &str,
    ) -> Result<Value> {
        self.set_room_listening_pause(room, duration_seconds, "manual_pause", requested_by_user_id)
            .await?;
        let event = self
            .timeline_store
            .append_event(
                &room.guild_id,
                &room.channel_id,
                json!({
                    "event_kind": "listening_paused",
                    "kind": "listening_paused",
                    "duration_seconds": duration_seconds,
                    "requested_by_user_id": requested_by_user_id,
                }),
            )
            .await?;
        Ok(json!({"action": "pause", "status": "paused", "roomId": room.room_id, "event": event}))
    }

    pub async fn resume_room(
        &mut self,
        room: &RoomConfig,
        requested_by_user_id: &str,
    ) -> Result<Value> {
        self.clear_room_controls(room, &["listening_paused_until"])
            .await?;
        let event = self
            .timeline_store
            .append_event(
                &room.guild_id,
                &room.channel_id,
                json!({
                    "event_kind": "listening_resumed",
                    "kind": "listening_resumed",
                    "requested_by_user_id": requested_by_user_id,
                }),
            )
            .await?;
        Ok(json!({"action": "resume", "status": "resumed", "roomId": room.room_id, "event": event}))
    }

    pub async fn room_controls_json(&self) -> Result<BTreeMap<String, Value>> {
        let mut rendered = BTreeMap::new();
        for (key, mut control) in self.timeline_store.list_room_controls().await? {
            clear_expired_room_control_markers(&mut control);
            if control.has_active_marker() {
                rendered.insert(key, control.to_json());
            }
        }
        Ok(rendered)
    }

    pub async fn prune_expired_room_controls(&self) -> Result<usize> {
        self.timeline_store.prune_expired_room_controls().await
    }

    pub async fn room_control_datetime_active(&self, room: &RoomConfig, key: &str) -> Result<bool> {
        Ok(self
            .timeline_store
            .get_room_control(&room.guild_id, &room.channel_id)
            .await?
            .and_then(|control| room_control_datetime(&control, key))
            .is_some_and(|value| value > utc_now()))
    }

    pub async fn room_control_status(&self, room: &RoomConfig) -> Result<Value> {
        let mut control = self
            .timeline_store
            .get_room_control(&room.guild_id, &room.channel_id)
            .await?;
        if let Some(control) = control.as_mut() {
            clear_expired_room_control_markers(control);
        }
        let control = control.filter(RoomControl::has_active_marker);
        let control_json = control
            .as_ref()
            .map(RoomControl::to_json)
            .unwrap_or_else(|| json!({}));
        Ok(json!({
            "autoJoinSuppressed": control.as_ref().and_then(|control| room_control_datetime(control, "auto_join_suppressed_until")).is_some_and(|value| value > utc_now()),
            "manualHoldActive": control.as_ref().and_then(|control| room_control_datetime(control, "manual_hold_until")).is_some_and(|value| value > utc_now()),
            "listeningPaused": control.as_ref().and_then(|control| room_control_datetime(control, "listening_paused_until")).is_some_and(|value| value > utc_now()),
            "control": control_json,
            "cooldownActive": false,
        }))
    }

    pub async fn update_room_control(
        &mut self,
        room: &RoomConfig,
        clear_keys: &[&str],
        apply: impl FnOnce(&mut RoomControl),
    ) -> Result<RoomControl> {
        let mut control = self
            .timeline_store
            .get_room_control(&room.guild_id, &room.channel_id)
            .await?
            .unwrap_or_default();
        for key in clear_keys {
            control.clear_key(key);
        }
        apply(&mut control);
        control.guild_id = room.guild_id.clone();
        control.voice_channel_id = room.channel_id.clone();
        control.voice_channel_name = room.channel_name.clone();
        control.updated_at = isoformat_z(None);
        clear_expired_room_control_markers(&mut control);
        if control.has_active_marker() {
            self.timeline_store.upsert_room_control(&control).await?;
        } else {
            self.timeline_store
                .delete_room_control(&room.guild_id, &room.channel_id)
                .await?;
        }
        Ok(control)
    }

    pub async fn suppress_room_auto_join(
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
        let control = self
            .update_room_control(room, &clear, |control| {
                control.auto_join_suppressed_until = Some(isoformat_z(Some(until)));
                control.auto_join_suppression_reason = Some(reason.to_string());
                control.auto_join_suppressed_by_user_id = Some(requested_by_user_id.to_string());
            })
            .await?;
        let _ = self
            .timeline_store
            .append_event(
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
            )
            .await;
        Ok(control)
    }

    pub async fn set_room_manual_hold(
        &mut self,
        room: &RoomConfig,
        duration_seconds: i64,
        reason: &str,
        requested_by_user_id: &str,
    ) -> Result<RoomControl> {
        let until = utc_now() + chrono::Duration::seconds(duration_seconds.max(0));
        let control = self
            .update_room_control(
                room,
                &["auto_join_suppressed_until", "listening_paused_until"],
                |control| {
                    control.manual_hold_until = Some(isoformat_z(Some(until)));
                    control.manual_hold_reason = Some(reason.to_string());
                    control.manual_hold_by_user_id = Some(requested_by_user_id.to_string());
                },
            )
            .await?;
        let _ = self
            .timeline_store
            .append_event(
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
            )
            .await;
        Ok(control)
    }

    pub async fn set_room_listening_pause(
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
        .await
    }

    pub async fn clear_room_controls(&mut self, room: &RoomConfig, keys: &[&str]) -> Result<()> {
        let Some(mut control) = self
            .timeline_store
            .get_room_control(&room.guild_id, &room.channel_id)
            .await?
        else {
            return Ok(());
        };
        for key in keys {
            control.clear_key(key);
        }
        clear_expired_room_control_markers(&mut control);
        if control.has_active_marker() {
            control.updated_at = isoformat_z(None);
            self.timeline_store.upsert_room_control(&control).await
        } else {
            self.timeline_store
                .delete_room_control(&room.guild_id, &room.channel_id)
                .await
        }
    }
}

pub(crate) fn room_control_datetime_active_from_map(
    controls: &BTreeMap<String, RoomControl>,
    channel_id: &str,
    key: &str,
) -> bool {
    controls
        .get(channel_id)
        .and_then(|control| room_control_datetime(control, key))
        .is_some_and(|value| value > utc_now())
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

fn clear_expired_room_control_markers(control: &mut RoomControl) {
    let now = utc_now();
    for key in [
        "auto_join_suppressed_until",
        "manual_hold_until",
        "listening_paused_until",
    ] {
        if room_control_datetime(control, key).is_some_and(|value| value <= now) {
            control.clear_key(key);
        }
    }
}
