use std::fs;

use serde_json::{Value, json};

use crate::Result;
use crate::config::{local_tz, state_dir};

use crate::runtime::timeline::{format_timestamp_local, read_json_file, write_json_file};
use crate::runtime::util::first_non_empty;
use crate::runtime::{JobState, RoomConfig, Runtime, VoiceBotStatus, VoiceCaptureSessionStatus};

impl Runtime {
    pub async fn status_for_room(&self, room: &RoomConfig) -> Result<Value> {
        let session_id = self.active_session_id_for_room(room);
        let session = match session_id
            .as_ref()
            .and_then(|id| self.sessions.get(id))
            .cloned()
        {
            Some(session) => Some(self.enrich_session_status(session).await),
            None => None,
        };
        let occupancy = self
            .timeline_store
            .get_occupancy(&room.guild_id, &room.channel_id)
            .await?;
        let retention_policy = occupancy
            .get("retention_policy")
            .cloned()
            .unwrap_or_else(|| json!({"draftTranscriptEvents": "7d", "sourceAudio": "7d"}));
        let live_publications = self
            .timeline_store
            .list_publications(
                Some(&room.guild_id),
                Some(&room.channel_id),
                Some("live_draft_published"),
            )
            .await?;
        let active_jobs = self
            .timeline_store
            .list_jobs_by_states(
                Some(&room.guild_id),
                &[
                    JobState::Queued,
                    JobState::Running,
                    JobState::Waiting,
                    JobState::CancelRequested,
                    JobState::ConfirmationPending,
                ],
            )
            .await?
            .into_iter()
            .filter(|job| job.voice_channel_id == room.channel_id && !job.state.is_terminal())
            .map(|job| Self::public_job_view(&job))
            .collect::<Vec<_>>();
        Ok(json!({
            "room": room.to_json(),
            "mode": session.as_ref().map(|value| value.mode.as_str()).unwrap_or("absent"),
            "assignedVoiceBotId": session.as_ref().map(|value| value.bot_id.as_str()).unwrap_or(""),
            "captureRunId": session.as_ref().map(|value| value.capture_run_id.as_str()).unwrap_or(""),
            "retentionPolicy": retention_policy,
            "control": self.room_control_status(room).await?,
            "occupancy": occupancy,
            "livePublications": live_publications,
            "activeJobs": active_jobs,
            "session": session.map(|value| value.to_json()),
            "bots": self.bots.values().map(VoiceBotStatus::to_json).collect::<Vec<_>>(),
        }))
    }

    pub async fn status_payload(&self, room_identifier: Option<&str>) -> Result<Value> {
        if let Some(identifier) = room_identifier.filter(|value| !value.trim().is_empty()) {
            return match self.room_for_identifier(Some(identifier)) {
                Ok(room) => self.status_for_room(&room).await,
                Err(error) => Ok(json!({"ok": false, "error": error.to_string()})),
            };
        }
        let mut sessions = Vec::new();
        for session in self.sessions.values().cloned() {
            sessions.push(self.enrich_session_status(session).await.to_json());
        }
        let mut rooms = Vec::new();
        for room in self.known_rooms() {
            let occupancy = self
                .timeline_store
                .get_occupancy(&room.guild_id, &room.channel_id)
                .await?;
            rooms.push(json!({
                "roomId": room.room_id,
                "guildId": room.guild_id,
                "channelId": room.channel_id,
                "channelName": room.channel_name,
                "channelSlug": room.channel_slug,
                "autoJoin": room.auto_join,
                "activeSessionId": self.active_session_id_for_room(&room).unwrap_or_default(),
                "control": self.room_control_status(&room).await?,
                "occupancy": occupancy,
            }));
        }
        Ok(json!({
            "bots": self.bots.values().map(VoiceBotStatus::to_json).collect::<Vec<_>>(),
            "pool": self.capacity_payload().await,
            "sessions": sessions,
            "rooms": rooms,
            "roomControls": self.room_controls_json().await?,
        }))
    }

    pub async fn capacity_payload(&self) -> Value {
        let configured = self.bots.len();
        let active = self.sessions.len();
        let mut assignments = Vec::new();
        for session in self.sessions.values().cloned() {
            assignments.push(self.enrich_session_status(session).await.to_json());
        }
        json!({
            "configuredBots": configured,
            "activeAssignments": active,
            "availableBots": configured.saturating_sub(active),
            "assignments": assignments,
        })
    }

    pub fn active_session_id_for_room(&self, room: &RoomConfig) -> Option<String> {
        self.active_sessions_for_room(room)
            .into_iter()
            .next()
            .map(|session| session.session_id)
    }

    pub(crate) fn active_sessions_for_room(
        &self,
        room: &RoomConfig,
    ) -> Vec<VoiceCaptureSessionStatus> {
        let mut sessions = self
            .sessions
            .values()
            .filter(|session| {
                session.active
                    && session.ended_at.trim().is_empty()
                    && session.guild_id == room.guild_id
                    && session.voice_channel_id == room.channel_id
            })
            .cloned()
            .collect::<Vec<_>>();
        sessions.sort_by(|left, right| {
            left.started_at
                .cmp(&right.started_at)
                .then_with(|| left.session_id.cmp(&right.session_id))
        });
        sessions
    }

    pub fn duplicate_voice_bot_sessions_for_room(
        &self,
        room: &RoomConfig,
    ) -> Vec<VoiceCaptureSessionStatus> {
        self.active_sessions_for_room(room)
            .into_iter()
            .skip(1)
            .collect()
    }

    pub(crate) fn voice_bot_currently_in_room(&self, room: &RoomConfig) -> Option<VoiceBotStatus> {
        self.bots
            .values()
            .find(|status| {
                status.ready
                    && status.current_guild_id == room.guild_id
                    && status.current_channel_id == room.channel_id
            })
            .cloned()
    }

    pub(crate) fn room_has_voice_bot_presence(&self, room: &RoomConfig) -> bool {
        !self.active_sessions_for_room(room).is_empty()
            || self.voice_bot_currently_in_room(room).is_some()
    }

    async fn enrich_session_status(
        &self,
        mut session: VoiceCaptureSessionStatus,
    ) -> VoiceCaptureSessionStatus {
        let capture_run_id = first_non_empty([
            session.capture_run_id.clone(),
            session.session_id.clone(),
            session.assignment_id.clone(),
        ]);
        if capture_run_id.is_empty()
            || session.guild_id.is_empty()
            || session.voice_channel_id.is_empty()
        {
            return session;
        }
        let Ok((event_count, last_transcript_at)) = self
            .timeline_store
            .speech_stats_for_capture_run(
                &session.guild_id,
                &session.voice_channel_id,
                &capture_run_id,
            )
            .await
        else {
            return session;
        };
        session.capture_stats.transcript_events =
            session.capture_stats.transcript_events.max(event_count);
        if let Some(last_transcript_at) = last_transcript_at {
            let fields = format_timestamp_local(last_transcript_at, local_tz());
            session.capture_stats.last_transcript_at =
                fields.get("iso").cloned().unwrap_or_default();
            session.capture_stats.last_transcript_at_local =
                fields.get("local_iso").cloned().unwrap_or_default();
        }
        session
    }

    pub async fn persist_status_snapshot(&self) -> Result<()> {
        fs::create_dir_all(state_dir())?;
        let mut payload = self.status_payload(None).await?;
        remove_room_controls_from_status_snapshot(&mut payload);
        write_json_file(&state_dir().join("status.json"), &payload)
    }

    pub(crate) fn load_status_snapshot(&mut self) {
        let payload = read_json_file(&state_dir().join("status.json"), json!({}));
        for bot in payload
            .get("bots")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            if let Ok(bot) = serde_json::from_value::<VoiceBotStatus>(bot) {
                if !bot.bot_id.trim().is_empty() {
                    self.bots.insert(bot.bot_id.clone(), bot);
                }
            }
        }
        for session in payload
            .get("sessions")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            if let Ok(session) = serde_json::from_value::<VoiceCaptureSessionStatus>(session) {
                let session_id = first_non_empty([
                    session.session_id.clone(),
                    session.capture_run_id.clone(),
                    session.assignment_id.clone(),
                ]);
                if !session_id.is_empty() {
                    self.sessions.insert(session_id, session);
                }
            }
        }
    }
}

fn remove_room_controls_from_status_snapshot(payload: &mut Value) {
    let Some(object) = payload.as_object_mut() else {
        return;
    };
    object.remove("roomControls");
    if let Some(Value::Array(rooms)) = object.get_mut("rooms") {
        for room in rooms {
            if let Value::Object(room) = room {
                room.remove("control");
            }
        }
    }
}
