use serde_json::{Value, json};

use crate::Result;
use crate::config::local_tz;
use crate::runtime::core::execution::{
    JoinRoomEffectRequest, LeaveRoomEffectRequest, RuntimeEffects,
};
use crate::runtime::timeline::{CaptureRunInput, first_value_string, isoformat_z, utc_now};

use crate::runtime::{Job, RoomConfig, Runtime, RuntimeBotStatus};

impl Runtime {
    pub async fn assign_room(
        &mut self,
        room_identifier: Option<&str>,
        guild_id: Option<&str>,
        user_id: Option<&str>,
        reason: Option<&str>,
    ) -> Result<Value> {
        let room =
            if let Some(identifier) = room_identifier.filter(|value| !value.trim().is_empty()) {
                self.room_for_identifier(Some(identifier))?
            } else if let Some(guild_id) = guild_id.filter(|value| !value.trim().is_empty()) {
                self.resolve_room_scope(guild_id, None)?
            } else {
                self.room_for_identifier(None)?
            };
        let reason = reason.unwrap_or("explicit_request");
        self.set_room_manual_hold(
            &room,
            self.manual_join_hold_seconds,
            reason,
            user_id.unwrap_or(""),
        )?;
        self.persist_status_snapshot()?;
        Ok(json!({
            "action": "join",
            "status": "state-recorded",
            "roomId": room.room_id,
            "guildId": room.guild_id,
            "channelId": room.channel_id,
            "reason": reason
        }))
    }

    pub async fn leave_room(
        &mut self,
        room_identifier: Option<&str>,
        cooldown_seconds: Option<i64>,
        requested_by_user_id: Option<&str>,
    ) -> Result<Value> {
        let cooldown_seconds = cooldown_seconds.unwrap_or(self.manual_leave_cooldown_seconds);
        let mut results = Vec::new();
        if let Some(identifier) = room_identifier.filter(|value| !value.trim().is_empty()) {
            let room = self.room_for_identifier(Some(identifier))?;
            self.suppress_room_auto_join(
                &room,
                cooldown_seconds,
                "manual_leave",
                requested_by_user_id.unwrap_or(""),
                true,
            )?;
            let session_id = self.active_session_id_for_room(&room);
            if let Some(session_id) = session_id {
                if let Some(mut session) = self.sessions.remove(&session_id) {
                    session.mark_ended(isoformat_z(None));
                    results.push(session.to_json());
                }
            }
            self.persist_status_snapshot()?;
            return Ok(
                json!({"action": "leave", "status": "ok", "roomId": room.room_id, "results": results}),
            );
        }
        for session in self.sessions.values_mut() {
            session.mark_ended(isoformat_z(None));
            results.push(session.to_json());
        }
        self.sessions.clear();
        self.persist_status_snapshot()?;
        Ok(json!({"action": "leave", "status": "ok", "results": results}))
    }

    pub async fn move_bot(
        &mut self,
        bot_id: &str,
        to_channel: &str,
        reason: Option<&str>,
    ) -> Result<Value> {
        let assigned = self
            .assign_room(
                Some(to_channel),
                None,
                None,
                reason.or(Some("admin_force_move")),
            )
            .await?;
        Ok(json!({
            "action": "move",
            "status": "state-recorded",
            "botId": bot_id,
            "target": to_channel,
            "assign": assigned,
        }))
    }

    pub(crate) async fn assign_room_with_effect(
        &mut self,
        room: RoomConfig,
        requested_by_user_id: &str,
        reason: &str,
        effects: &dyn RuntimeEffects,
    ) -> Result<Value> {
        if let Some(session_id) = self.active_session_id_for_room(&room) {
            let session = self
                .sessions
                .get(&session_id)
                .map(|value| value.to_json())
                .unwrap_or_else(|| json!({"sessionId": session_id}));
            return Ok(json!({
                "action": "join",
                "status": "already_assigned",
                "roomId": room.room_id,
                "guildId": room.guild_id,
                "channelId": room.channel_id,
                "session": session,
            }));
        }

        if should_record_manual_hold_for_join(reason) {
            self.set_room_manual_hold(
                &room,
                self.manual_join_hold_seconds,
                reason,
                requested_by_user_id,
            )?;
        }

        let Some(bot) = self.available_voice_bot() else {
            return Ok(json!({
                "action": "join",
                "status": "no_available_voice_bot",
                "roomId": room.room_id,
                "guildId": room.guild_id,
                "channelId": room.channel_id,
                "bots": self.bots.values().map(RuntimeBotStatus::to_json).collect::<Vec<_>>(),
                "message": "No configured Discord voice bot is ready and unassigned."
            }));
        };

        let started_at = utc_now();
        let run = self.timeline_store.create_capture_run(CaptureRunInput {
            guild_id: room.guild_id.clone(),
            guild_slug: room.guild_slug.clone(),
            voice_channel_id: room.channel_id.clone(),
            voice_channel_name: room.channel_name.clone(),
            voice_channel_slug: room.channel_slug.clone(),
            voice_bot_id: bot.bot_id.clone(),
            voice_bot_discord_user_id: bot.user_id.clone(),
            started_at: Some(started_at),
            mode: "local_buffering".to_string(),
            reason: reason.to_string(),
            retention_policy: None,
        })?;
        let capture_run_id = first_value_string(&run, &["capture_run_id", "captureRunId"]);
        let assignment_id = first_value_string(&run, &["assignment_id", "assignmentId"]);
        self.mark_bot_joining(&bot.bot_id, &capture_run_id, "")?;
        let session_dir = session_directory(self, &room, started_at, &capture_run_id);

        let request = JoinRoomEffectRequest {
            room: room.clone(),
            bot_id: bot.bot_id.clone(),
            capture_run_id: capture_run_id.clone(),
            assignment_id,
            started_at,
            session_dir,
            requested_by_user_id: requested_by_user_id.to_string(),
            reason: reason.to_string(),
        };
        match effects.join_room(request).await {
            Ok(result) => {
                if let Some(status) = result.bot_status {
                    self.bots.insert(status.bot_id.clone(), status);
                }
                if let Some(session) = result.session {
                    self.sessions
                        .insert(session.session_id.clone(), session.clone());
                    self.timeline_store.set_occupancy(json!({
                        "guild_id": room.guild_id,
                        "guildId": room.guild_id,
                        "voice_channel_id": room.channel_id,
                        "channelId": room.channel_id,
                        "voice_channel_name": room.channel_name,
                        "channelName": room.channel_name,
                        "updated_at": isoformat_z(None),
                    }))?;
                    self.persist_status_snapshot()?;
                    Ok(json!({
                        "action": "join",
                        "status": result.status,
                        "roomId": room.room_id,
                        "guildId": room.guild_id,
                        "channelId": room.channel_id,
                        "botId": bot.bot_id,
                        "captureRunId": capture_run_id,
                        "requestedUserId": requested_by_user_id,
                        "reason": reason,
                        "session": session.to_json(),
                    }))
                } else {
                    self.persist_status_snapshot()?;
                    Ok(json!({
                        "action": "join",
                        "status": result.status,
                        "roomId": room.room_id,
                        "guildId": room.guild_id,
                        "channelId": room.channel_id,
                        "botId": bot.bot_id,
                        "captureRunId": capture_run_id,
                        "message": result.message,
                    }))
                }
            }
            Err(error) => {
                self.mark_bot_join_failed(&bot.bot_id, &error.to_string())?;
                let _ = self.suppress_room_auto_join(
                    &room,
                    self.manual_leave_cooldown_seconds,
                    "join_failed",
                    requested_by_user_id,
                    true,
                );
                let _ = self.timeline_store.close_capture_run(
                    &room.guild_id,
                    &room.channel_id,
                    &capture_run_id,
                    Some(utc_now()),
                    "join_failed",
                    "failed",
                );
                Err(error)
            }
        }
    }

    pub(crate) async fn leave_room_with_effect(
        &mut self,
        room_identifier: Option<&str>,
        cooldown_seconds: i64,
        requested_by_user_id: &str,
        parent_job: Option<&Job>,
        effects: &dyn RuntimeEffects,
    ) -> Result<Value> {
        let mut results = Vec::new();
        if let Some(identifier) = room_identifier.filter(|value| !value.trim().is_empty()) {
            let room = self.room_for_identifier(Some(identifier))?;
            self.suppress_room_auto_join(
                &room,
                cooldown_seconds,
                "manual_leave",
                requested_by_user_id,
                true,
            )?;
            if let Some(session_id) = self.active_session_id_for_room(&room) {
                results.push(
                    self.finish_room_session_with_effect(
                        &session_id,
                        "manual_leave",
                        parent_job,
                        effects,
                    )
                    .await?,
                );
            }
            self.persist_status_snapshot()?;
            return Ok(json!({
                "action": "leave",
                "status": "ok",
                "roomId": room.room_id,
                "results": results,
            }));
        }

        let session_ids = self.sessions.keys().cloned().collect::<Vec<_>>();
        for session_id in session_ids {
            results.push(
                self.finish_room_session_with_effect(
                    &session_id,
                    "manual_leave_all",
                    parent_job,
                    effects,
                )
                .await?,
            );
        }
        self.persist_status_snapshot()?;
        Ok(json!({"action": "leave", "status": "ok", "results": results}))
    }

    pub(crate) fn sync_voice_adapter_status(
        &mut self,
        bots: Vec<RuntimeBotStatus>,
        sessions: Vec<crate::runtime::RuntimeSessionStatus>,
    ) -> Result<()> {
        let active_session_ids = sessions
            .iter()
            .filter(|session| session.active)
            .map(|session| session.session_id.clone())
            .collect::<std::collections::BTreeSet<_>>();
        let ended_at = isoformat_z(None);
        let mut stale_sessions = Vec::new();
        for session in self.sessions.values_mut() {
            if !active_session_ids.contains(&session.session_id)
                && session.ended_at.trim().is_empty()
            {
                stale_sessions.push(session.clone());
                session.mark_ended(ended_at.clone());
            }
        }
        for session in stale_sessions {
            let capture_run_id = first_value_string(
                &session.to_json(),
                &["capture_run_id", "captureRunId", "session_id", "sessionId"],
            );
            if !capture_run_id.trim().is_empty() {
                let _ = self.timeline_store.close_capture_run(
                    &session.guild_id,
                    &session.voice_channel_id,
                    &capture_run_id,
                    Some(utc_now()),
                    "adapter_sync_missing",
                    "ended",
                );
            }
        }
        self.sessions.retain(|_, session| {
            session.active
                && session.ended_at.trim().is_empty()
                && active_session_ids.contains(&session.session_id)
        });
        for status in bots {
            self.bots.insert(status.bot_id.clone(), status);
        }
        for session in sessions {
            if session.active {
                self.sessions.insert(session.session_id.clone(), session);
            }
        }
        self.persist_status_snapshot()
    }

    fn available_voice_bot(&self) -> Option<RuntimeBotStatus> {
        self.bots
            .values()
            .find(|status| {
                status.ready
                    && status.joining_session_id.trim().is_empty()
                    && status.assigned_session_id.trim().is_empty()
            })
            .cloned()
    }

    fn mark_bot_joining(&mut self, bot_id: &str, session_id: &str, error: &str) -> Result<()> {
        if let Some(status) = self.bots.get_mut(bot_id) {
            status.joining_session_id = session_id.to_string();
            status.last_error = error.to_string();
        }
        self.persist_status_snapshot()
    }

    fn mark_bot_join_failed(&mut self, bot_id: &str, error: &str) -> Result<()> {
        if let Some(status) = self.bots.get_mut(bot_id) {
            status.joining_session_id.clear();
            status.last_error = error.to_string();
        }
        self.persist_status_snapshot()
    }

    async fn finish_room_session_with_effect(
        &mut self,
        session_id: &str,
        reason: &str,
        parent_job: Option<&Job>,
        effects: &dyn RuntimeEffects,
    ) -> Result<Value> {
        let result = effects
            .leave_room(LeaveRoomEffectRequest {
                session_id: session_id.to_string(),
                reason: reason.to_string(),
            })
            .await?;
        for job in result.audio_jobs {
            if let Some(parent_job) = parent_job {
                self.timeline_store.create_child_job(parent_job, job)?;
            } else {
                self.timeline_store.create_job(job)?;
            }
        }
        if !result.capture_run_id.trim().is_empty() {
            self.timeline_store.close_capture_run(
                &result.guild_id,
                &result.voice_channel_id,
                &result.capture_run_id,
                Some(utc_now()),
                reason,
                "ended",
            )?;
        }
        if let Some(status) = result.bot_status {
            self.bots.insert(status.bot_id.clone(), status);
        }
        if let Some(mut session) = result.session {
            session.mark_ended(isoformat_z(None));
            self.sessions.remove(&session.session_id);
            Ok(session.to_json())
        } else if let Some(mut session) = self.sessions.remove(session_id) {
            session.mark_ended(isoformat_z(None));
            Ok(session.to_json())
        } else {
            Ok(json!({
                "sessionId": result.session_id,
                "status": result.status,
                "endedAt": isoformat_z(None),
            }))
        }
    }
}

fn session_directory(
    runtime: &Runtime,
    room: &RoomConfig,
    started_at: chrono::DateTime<chrono::Utc>,
    session_id: &str,
) -> std::path::PathBuf {
    let local = started_at.with_timezone(&local_tz());
    let prefix = session_id.chars().take(8).collect::<String>();
    runtime
        .timeline_store
        .channel_dir(&room.guild_id, &room.channel_id)
        .join("capture-run-scratch")
        .join(local.format("%Y/%m/%d").to_string())
        .join(format!("{}-{prefix}", local.format("%H-%M-%S")))
}

fn should_record_manual_hold_for_join(reason: &str) -> bool {
    !matches!(reason, "auto_join" | "manual_hold")
}
