use serde_json::{Value, json};

use crate::Result;
use crate::config::local_tz;

use crate::runtime::timeline::format_timestamp_local;
use crate::runtime::util::first_non_empty;
use crate::runtime::{
    JobState, RoomConfig, Runtime, VoiceAssignment, VoiceBotStatus, VoiceCaptureSessionStatus,
};

impl Runtime {
    pub async fn status_for_room(&self, room: &RoomConfig) -> Result<Value> {
        let bots = self.timeline_store.list_voice_bot_states().await?;
        let sessions = self.timeline_store.list_active_capture_sessions().await?;
        let assignments = self.timeline_store.list_active_voice_assignments().await?;
        let assignment = active_assignment_for_room(&assignments, room);
        let session = match active_session_for_room(&sessions, room) {
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
            "assignmentState": assignment.as_ref().map(|value| value.state.as_str()).unwrap_or("absent"),
            "assignedVoiceBotId": assignment.as_ref().map(|value| value.voice_bot_id.as_str()).or_else(|| session.as_ref().map(|value| value.bot_id.as_str())).unwrap_or(""),
            "captureRunId": assignment.as_ref().map(|value| value.capture_run_id.as_str()).or_else(|| session.as_ref().map(|value| value.capture_run_id.as_str())).unwrap_or(""),
            "retentionPolicy": retention_policy,
            "control": self.room_control_status(room).await?,
            "occupancy": occupancy,
            "livePublications": live_publications,
            "activeJobs": active_jobs,
            "assignment": assignment.map(|value| value.to_json()),
            "session": session.map(|value| value.to_json()),
            "bots": bots.iter().map(VoiceBotStatus::to_json).collect::<Vec<_>>(),
        }))
    }

    pub async fn status_payload(&self, room_identifier: Option<&str>) -> Result<Value> {
        if let Some(identifier) = room_identifier.filter(|value| !value.trim().is_empty()) {
            return match self.room_for_identifier(Some(identifier)).await {
                Ok(room) => self.status_for_room(&room).await,
                Err(error) => Ok(json!({"ok": false, "error": error.to_string()})),
            };
        }
        let bots = self.timeline_store.list_voice_bot_states().await?;
        let active_assignments = self.timeline_store.list_active_voice_assignments().await?;
        let active_sessions = self.timeline_store.list_active_capture_sessions().await?;
        let mut sessions = Vec::new();
        for session in active_sessions.iter().cloned() {
            sessions.push(self.enrich_session_status(session).await.to_json());
        }
        let mut rooms = Vec::new();
        for room in self.known_rooms().await? {
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
                "activeSessionId": active_session_for_room(&active_sessions, &room).map(|session| session.session_id).unwrap_or_default(),
                "activeAssignmentId": active_assignment_for_room(&active_assignments, &room).map(|assignment| assignment.assignment_id).unwrap_or_default(),
                "control": self.room_control_status(&room).await?,
                "occupancy": occupancy,
            }));
        }
        Ok(json!({
            "bots": bots.iter().map(VoiceBotStatus::to_json).collect::<Vec<_>>(),
            "pool": self.capacity_payload().await,
            "sessions": sessions,
            "assignments": active_assignments.iter().map(VoiceAssignment::to_json).collect::<Vec<_>>(),
            "rooms": rooms,
            "roomControls": self.room_controls_json().await?,
        }))
    }

    pub async fn capacity_payload(&self) -> Value {
        let bots = self
            .timeline_store
            .list_voice_bot_states()
            .await
            .unwrap_or_default();
        let assignments = self
            .timeline_store
            .list_active_voice_assignments()
            .await
            .unwrap_or_default();
        let observed = bots.len();
        let active = assignments.len();
        json!({
            "observedBots": observed,
            "activeAssignments": active,
            "availableBots": observed.saturating_sub(active),
            "assignments": assignments.iter().map(VoiceAssignment::to_json).collect::<Vec<_>>(),
        })
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
}

fn active_session_for_room(
    sessions: &[VoiceCaptureSessionStatus],
    room: &RoomConfig,
) -> Option<VoiceCaptureSessionStatus> {
    let mut matches = sessions
        .iter()
        .filter(|session| {
            session.active
                && session.ended_at.trim().is_empty()
                && session.guild_id == room.guild_id
                && session.voice_channel_id == room.channel_id
        })
        .cloned()
        .collect::<Vec<_>>();
    matches.sort_by(|left, right| {
        left.started_at
            .cmp(&right.started_at)
            .then_with(|| left.session_id.cmp(&right.session_id))
    });
    matches.into_iter().next()
}

fn active_assignment_for_room(
    assignments: &[VoiceAssignment],
    room: &RoomConfig,
) -> Option<VoiceAssignment> {
    let mut matches = assignments
        .iter()
        .filter(|assignment| {
            assignment.is_active()
                && assignment.guild_id == room.guild_id
                && assignment.voice_channel_id == room.channel_id
        })
        .cloned()
        .collect::<Vec<_>>();
    matches.sort_by(|left, right| {
        left.assigned_at
            .cmp(&right.assigned_at)
            .then_with(|| left.assignment_id.cmp(&right.assignment_id))
    });
    matches.into_iter().next()
}
