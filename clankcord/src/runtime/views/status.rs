use std::fs;

use serde_json::{Value, json};

use crate::Result;
use crate::config::{format_timestamp_local, local_tz, read_json, state_dir, write_json};

use crate::runtime::util::first_non_empty;
use crate::runtime::{JobState, RoomConfig, Runtime, RuntimeBotStatus, RuntimeSessionStatus};

impl Runtime {
    pub fn status_for_room(&self, room: &RoomConfig) -> Value {
        let session_id = self.active_session_id_for_room(room);
        let session = session_id
            .as_ref()
            .and_then(|id| self.sessions.get(id))
            .cloned()
            .map(|session| self.enrich_session_status(session));
        json!({
            "room": room.to_json(),
            "mode": session.as_ref().map(|value| value.mode.as_str()).unwrap_or("absent"),
            "assignedVoiceBotId": session.as_ref().map(|value| value.bot_id.as_str()).unwrap_or(""),
            "captureRunId": session.as_ref().map(|value| value.capture_run_id.as_str()).unwrap_or(""),
            "retentionPolicy": self.timeline_store.get_occupancy(&room.guild_id, &room.channel_id).ok().and_then(|value| value.get("retention_policy").cloned()).unwrap_or_else(|| json!({"draftTranscriptEvents": "7d", "sourceAudio": "7d"})),
            "control": self.room_control_status(room),
            "occupancy": self.timeline_store.get_occupancy(&room.guild_id, &room.channel_id).unwrap_or_else(|_| json!({})),
            "livePublications": self.timeline_store.list_publications(Some(&room.guild_id), Some(&room.channel_id), Some("live_draft_published")).unwrap_or_default(),
            "activeJobs": self.timeline_store.list_jobs_by_states(Some(&room.guild_id), &[JobState::Queued, JobState::Running, JobState::Waiting, JobState::CancelRequested, JobState::ConfirmationPending]).unwrap_or_default().into_iter().filter(|job| {
                job.voice_channel_id == room.channel_id && !job.state.is_terminal()
            }).map(|job| Self::public_job_view(&job)).collect::<Vec<_>>(),
            "session": session.map(|value| value.to_json()),
            "bots": self.bots.values().map(RuntimeBotStatus::to_json).collect::<Vec<_>>(),
        })
    }

    pub fn status_payload(&self, room_identifier: Option<&str>) -> Value {
        if let Some(identifier) = room_identifier.filter(|value| !value.trim().is_empty()) {
            return match self.room_for_identifier(Some(identifier)) {
                Ok(room) => self.status_for_room(&room),
                Err(error) => json!({"ok": false, "error": error.to_string()}),
            };
        }
        json!({
            "bots": self.bots.values().map(RuntimeBotStatus::to_json).collect::<Vec<_>>(),
            "pool": self.capacity_payload(),
            "sessions": self.sessions.values().cloned().map(|session| self.enrich_session_status(session).to_json()).collect::<Vec<_>>(),
            "rooms": self.known_rooms().into_iter().map(|room| {
                json!({
                    "roomId": room.room_id,
                    "guildId": room.guild_id,
                    "channelId": room.channel_id,
                    "channelName": room.channel_name,
                    "channelSlug": room.channel_slug,
                    "autoJoin": room.auto_join,
                    "activeSessionId": self.active_session_id_for_room(&room).unwrap_or_default(),
                    "control": self.room_control_status(&room),
                    "occupancy": self.timeline_store.get_occupancy(&room.guild_id, &room.channel_id).unwrap_or_else(|_| json!({})),
                })
            }).collect::<Vec<_>>(),
            "roomControls": self.room_controls_json(),
        })
    }

    pub fn capacity_payload(&self) -> Value {
        let configured = self.bots.len();
        let active = self.sessions.len();
        json!({
            "configuredBots": configured,
            "activeAssignments": active,
            "availableBots": configured.saturating_sub(active),
            "assignments": self.sessions.values().cloned().map(|session| self.enrich_session_status(session).to_json()).collect::<Vec<_>>(),
        })
    }

    pub fn active_session_id_for_room(&self, room: &RoomConfig) -> Option<String> {
        self.active_sessions_for_room(room)
            .into_iter()
            .next()
            .map(|session| session.session_id)
    }

    pub(crate) fn active_sessions_for_room(&self, room: &RoomConfig) -> Vec<RuntimeSessionStatus> {
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

    pub(crate) fn duplicate_voice_bot_sessions_for_room(
        &self,
        room: &RoomConfig,
    ) -> Vec<RuntimeSessionStatus> {
        self.active_sessions_for_room(room)
            .into_iter()
            .skip(1)
            .collect()
    }

    pub(crate) fn voice_bot_currently_in_room(
        &self,
        room: &RoomConfig,
    ) -> Option<RuntimeBotStatus> {
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

    fn enrich_session_status(&self, mut session: RuntimeSessionStatus) -> RuntimeSessionStatus {
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
        let Ok((event_count, last_transcript_at)) =
            self.timeline_store.speech_stats_for_capture_run(
                &session.guild_id,
                &session.voice_channel_id,
                &capture_run_id,
            )
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

    pub fn persist_status_snapshot(&self) -> Result<()> {
        fs::create_dir_all(state_dir())?;
        write_json(&state_dir().join("status.json"), &self.status_payload(None))
    }

    pub(crate) fn load_status_snapshot(&mut self) {
        let payload = read_json(&state_dir().join("status.json"), json!({}));
        for bot in payload
            .get("bots")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            if let Ok(bot) = serde_json::from_value::<RuntimeBotStatus>(bot) {
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
            if let Ok(session) = serde_json::from_value::<RuntimeSessionStatus>(session) {
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
