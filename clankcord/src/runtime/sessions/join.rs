use serde_json::{Value, json};

use crate::Result;
use crate::config::local_tz;
use crate::runtime::core::execution::JobDecision;
use crate::runtime::timeline::{CaptureRunInput, first_value_string, isoformat_z, utc_now};

use crate::runtime::{
    DiscordVoiceJoinOutput, DiscordVoiceJoinPayload, DiscordVoiceLeaveOutput,
    DiscordVoiceLeavePayload, DiscordVoicePlaybackCue, Job, JobKind, JobOutput, JobState,
    RoomAgentPlacementAction, RoomAgentPlacementOutput, RoomAgentPlacementPayload, RoomConfig,
    Runtime, RuntimeBotStatus, RuntimeSessionStatus,
};

impl Runtime {
    pub fn assign_room(
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

    pub fn leave_room(
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

    pub fn move_bot(
        &mut self,
        bot_id: &str,
        to_channel: &str,
        reason: Option<&str>,
    ) -> Result<Value> {
        let assigned = self.assign_room(
            Some(to_channel),
            None,
            None,
            reason.or(Some("admin_force_move")),
        )?;
        Ok(json!({
            "action": "move",
            "status": "state-recorded",
            "botId": bot_id,
            "target": to_channel,
            "assign": assigned,
        }))
    }

    pub(crate) fn prepare_join_room_jobs(
        &mut self,
        room: RoomConfig,
        requested_by_user_id: &str,
        reason: &str,
    ) -> Result<JobDecision> {
        if let Some(session) = self.active_sessions_for_room(&room).into_iter().next() {
            return Ok(JobDecision::Complete(JobOutput::RoomAgentPlacement(
                RoomAgentPlacementOutput {
                    action: RoomAgentPlacementAction::Join,
                    status: "already_assigned".to_string(),
                    room,
                    bot_id: session.bot_id.clone(),
                    capture_run_id: session.capture_run_id.clone(),
                    requested_by_user_id: requested_by_user_id.to_string(),
                    reason: reason.to_string(),
                    session: Some(session),
                    sessions: Vec::new(),
                    bots: Vec::new(),
                    message: String::new(),
                },
            )));
        }
        if let Some(bot) = self.voice_bot_currently_in_room(&room) {
            return Ok(JobDecision::Complete(JobOutput::RoomAgentPlacement(
                RoomAgentPlacementOutput {
                    action: RoomAgentPlacementAction::Join,
                    status: "already_assigned".to_string(),
                    room,
                    bot_id: bot.bot_id,
                    capture_run_id: bot.assigned_session_id,
                    requested_by_user_id: requested_by_user_id.to_string(),
                    reason: reason.to_string(),
                    session: None,
                    sessions: Vec::new(),
                    bots: Vec::new(),
                    message: "voice bot is already present in the channel".to_string(),
                },
            )));
        }
        if let Some(join) = self.active_voice_join_for_room(&room)? {
            let payload = join.discord_voice_join_payload().ok_or_else(|| {
                anyhow::anyhow!("active discord voice join {} has no join payload", join.id)
            })?;
            return Ok(JobDecision::Complete(JobOutput::RoomAgentPlacement(
                RoomAgentPlacementOutput {
                    action: RoomAgentPlacementAction::Join,
                    status: "already_joining".to_string(),
                    room,
                    bot_id: payload.bot_id.clone(),
                    capture_run_id: payload.capture_run_id.clone(),
                    requested_by_user_id: requested_by_user_id.to_string(),
                    reason: reason.to_string(),
                    session: None,
                    sessions: Vec::new(),
                    bots: Vec::new(),
                    message: "voice bot join is already in progress for the channel".to_string(),
                },
            )));
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
            return Ok(JobDecision::Complete(JobOutput::RoomAgentPlacement(
                RoomAgentPlacementOutput {
                    action: RoomAgentPlacementAction::Join,
                    status: "no_available_voice_bot".to_string(),
                    room,
                    bot_id: String::new(),
                    capture_run_id: String::new(),
                    requested_by_user_id: requested_by_user_id.to_string(),
                    reason: reason.to_string(),
                    session: None,
                    sessions: Vec::new(),
                    bots: self.bots.values().cloned().collect(),
                    message: "No configured Discord voice bot is ready and unassigned.".to_string(),
                },
            )));
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

        Ok(JobDecision::WaitFor(vec![Job::discord_voice_join(
            DiscordVoiceJoinPayload {
                room,
                bot_id: bot.bot_id,
                capture_run_id,
                assignment_id,
                started_at,
                session_dir,
                requested_by_user_id: requested_by_user_id.to_string(),
                reason: reason.to_string(),
            },
        )]))
    }

    pub(crate) fn commit_join_room_job(
        &mut self,
        request: &DiscordVoiceJoinPayload,
        result: DiscordVoiceJoinOutput,
    ) -> Result<JobOutput> {
        let room = request.room.clone();
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
            Ok(JobOutput::RoomAgentPlacement(RoomAgentPlacementOutput {
                action: RoomAgentPlacementAction::Join,
                status: result.status,
                room,
                bot_id: request.bot_id.clone(),
                capture_run_id: request.capture_run_id.clone(),
                requested_by_user_id: request.requested_by_user_id.clone(),
                reason: request.reason.clone(),
                session: Some(session),
                sessions: Vec::new(),
                bots: Vec::new(),
                message: String::new(),
            }))
        } else {
            self.persist_status_snapshot()?;
            Ok(JobOutput::RoomAgentPlacement(RoomAgentPlacementOutput {
                action: RoomAgentPlacementAction::Join,
                status: result.status,
                room,
                bot_id: request.bot_id.clone(),
                capture_run_id: request.capture_run_id.clone(),
                requested_by_user_id: request.requested_by_user_id.clone(),
                reason: request.reason.clone(),
                session: None,
                sessions: Vec::new(),
                bots: Vec::new(),
                message: result.message,
            }))
        }
    }

    pub(crate) fn fail_join_room_job(
        &mut self,
        request: &DiscordVoiceJoinPayload,
        error: &str,
    ) -> Result<()> {
        self.mark_bot_join_failed(&request.bot_id, error)?;
        let _ = self.suppress_room_auto_join(
            &request.room,
            self.manual_leave_cooldown_seconds,
            "join_failed",
            &request.requested_by_user_id,
            true,
        );
        let _ = self.timeline_store.close_capture_run(
            &request.room.guild_id,
            &request.room.channel_id,
            &request.capture_run_id,
            Some(utc_now()),
            "join_failed",
            "failed",
        );
        Ok(())
    }

    pub(crate) fn prepare_leave_room_jobs(
        &mut self,
        room_identifier: Option<&str>,
        cooldown_seconds: i64,
        requested_by_user_id: &str,
        source_job_id: &str,
    ) -> Result<JobDecision> {
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
                let Some(session) = self.sessions.get(&session_id).cloned() else {
                    anyhow::bail!("active room session {session_id} is missing from runtime state");
                };
                return Ok(JobDecision::WaitFor(vec![
                    self.voice_playback_job_for_session(
                        &session,
                        requested_by_user_id,
                        DiscordVoicePlaybackCue::Leave,
                        "manual_leave",
                        source_job_id,
                    ),
                ]));
            }
            self.persist_status_snapshot()?;
            return Ok(JobDecision::Complete(JobOutput::RoomAgentPlacement(
                RoomAgentPlacementOutput {
                    action: RoomAgentPlacementAction::Leave,
                    status: "ok".to_string(),
                    room,
                    bot_id: String::new(),
                    capture_run_id: String::new(),
                    requested_by_user_id: requested_by_user_id.to_string(),
                    reason: "manual_leave".to_string(),
                    session: None,
                    sessions: Vec::new(),
                    bots: Vec::new(),
                    message: String::new(),
                },
            )));
        }

        let requests = self
            .sessions
            .values()
            .cloned()
            .map(|session| {
                self.voice_playback_job_for_session(
                    &session,
                    requested_by_user_id,
                    DiscordVoicePlaybackCue::Leave,
                    "manual_leave_all",
                    source_job_id,
                )
            })
            .collect::<Vec<_>>();
        if requests.is_empty() {
            self.persist_status_snapshot()?;
            return Ok(JobDecision::Complete(JobOutput::RoomAgentPlacement(
                RoomAgentPlacementOutput {
                    action: RoomAgentPlacementAction::Leave,
                    status: "ok".to_string(),
                    room: RoomConfig::default(),
                    bot_id: String::new(),
                    capture_run_id: String::new(),
                    requested_by_user_id: requested_by_user_id.to_string(),
                    reason: "manual_leave_all".to_string(),
                    session: None,
                    sessions: Vec::new(),
                    bots: Vec::new(),
                    message: String::new(),
                },
            )));
        }
        Ok(JobDecision::WaitFor(requests))
    }

    pub(crate) fn resume_room_agent_placement_job(
        &mut self,
        job: &Job,
        payload: &RoomAgentPlacementPayload,
    ) -> Result<JobDecision> {
        let children = self.timeline_store.list_child_jobs(&job.id)?;
        if children.iter().any(|child| !child.state.is_terminal()) {
            return Ok(JobDecision::Wait);
        }
        if let Some(failed) = children.iter().find(|child| {
            child.kind != JobKind::DiscordVoicePlayback && child.state != JobState::Complete
        }) {
            if let Some(join_payload) = failed.discord_voice_join_payload() {
                self.fail_join_room_job(join_payload, &failed.metadata.error)?;
            }
            return Ok(JobDecision::fail(format!(
                "room placement dependency {} ended as {}: {}",
                failed.id, failed.state, failed.metadata.error
            )));
        }
        match payload.action {
            RoomAgentPlacementAction::Join => {
                let child = single_child_of_kind(&children, JobKind::DiscordVoiceJoin)?;
                let request = child.discord_voice_join_payload().ok_or_else(|| {
                    anyhow::anyhow!("join child {} has no join payload", child.id)
                })?;
                match child.metadata.output.clone() {
                    Some(JobOutput::DiscordVoiceJoin(output)) => {
                        let placement_output = self.commit_join_room_job(request, output)?;
                        if !has_playback_child(&children, DiscordVoicePlaybackCue::Join) {
                            if let JobOutput::RoomAgentPlacement(output) = &placement_output {
                                if let Some(session) = &output.session {
                                    return Ok(JobDecision::WaitFor(vec![
                                        self.voice_playback_job_for_session(
                                            session,
                                            &request.requested_by_user_id,
                                            DiscordVoicePlaybackCue::Join,
                                            "room_join",
                                            &job.id,
                                        ),
                                    ]));
                                }
                            }
                        }
                        Ok(JobDecision::Complete(placement_output))
                    }
                    Some(other) => Ok(JobDecision::fail(format!(
                        "join child {} completed with wrong output kind: {:?}",
                        child.id, other
                    ))),
                    None => Ok(JobDecision::fail(format!(
                        "join child {} completed without output",
                        child.id
                    ))),
                }
            }
            RoomAgentPlacementAction::Leave => {
                if !children
                    .iter()
                    .any(|child| child.kind == JobKind::DiscordVoiceLeave)
                {
                    let leave_requests = children
                        .iter()
                        .filter_map(|child| {
                            child
                                .discord_voice_playback_payload()
                                .map(|payload| (child, payload))
                        })
                        .filter(|(_, payload)| payload.cue == DiscordVoicePlaybackCue::Leave)
                        .map(|(child, payload)| {
                            Job::discord_voice_leave(
                                child.guild_id.clone(),
                                child.voice_channel_id.clone(),
                                job.requested_by_user_id.clone(),
                                DiscordVoiceLeavePayload {
                                    session_id: payload.session_id.clone(),
                                    reason: payload.reason.clone(),
                                },
                            )
                        })
                        .collect::<Vec<_>>();
                    if !leave_requests.is_empty() {
                        return Ok(JobDecision::WaitFor(leave_requests));
                    }
                }
                let mut sessions = Vec::new();
                for child in children
                    .iter()
                    .filter(|child| child.kind == JobKind::DiscordVoiceLeave)
                {
                    let reason = child
                        .discord_voice_leave_payload()
                        .map(|payload| payload.reason.clone())
                        .unwrap_or_else(|| "manual_leave".to_string());
                    match child.metadata.output.clone() {
                        Some(JobOutput::DiscordVoiceLeave(output)) => {
                            if let Some(session) =
                                self.commit_finished_room_session(output, &reason)?
                            {
                                sessions.push(session);
                            }
                        }
                        Some(other) => {
                            return Ok(JobDecision::fail(format!(
                                "leave child {} completed with wrong output kind: {:?}",
                                child.id, other
                            )));
                        }
                        None => {
                            return Ok(JobDecision::fail(format!(
                                "leave child {} completed without output",
                                child.id
                            )));
                        }
                    }
                }
                self.persist_status_snapshot()?;
                Ok(JobDecision::Complete(JobOutput::RoomAgentPlacement(
                    RoomAgentPlacementOutput {
                        action: RoomAgentPlacementAction::Leave,
                        status: "ok".to_string(),
                        room: RoomConfig::default(),
                        bot_id: String::new(),
                        capture_run_id: String::new(),
                        requested_by_user_id: job.requested_by_user_id.clone(),
                        reason: payload.reason.clone(),
                        session: None,
                        sessions,
                        bots: Vec::new(),
                        message: String::new(),
                    },
                )))
            }
        }
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
        self.bots = bots
            .into_iter()
            .map(|status| (status.bot_id.clone(), status))
            .collect();
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

    fn active_voice_join_for_room(&self, room: &RoomConfig) -> Result<Option<Job>> {
        Ok(self
            .timeline_store
            .list_jobs_by_scope_kind(&room.guild_id, &room.channel_id, JobKind::DiscordVoiceJoin)?
            .into_iter()
            .find(|job| !job.state.is_terminal()))
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

    fn commit_finished_room_session(
        &mut self,
        result: DiscordVoiceLeaveOutput,
        reason: &str,
    ) -> Result<Option<RuntimeSessionStatus>> {
        for job in result.audio_jobs {
            self.timeline_store.create_job(job)?;
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
            Ok(Some(session))
        } else if let Some(mut session) = self.sessions.remove(&result.session_id) {
            session.mark_ended(isoformat_z(None));
            Ok(Some(session))
        } else {
            Ok(None)
        }
    }
}

fn single_child_of_kind(children: &[Job], kind: JobKind) -> Result<&Job> {
    let matches = children
        .iter()
        .filter(|child| child.kind == kind)
        .collect::<Vec<_>>();
    if matches.len() != 1 {
        anyhow::bail!("expected exactly one {kind} child, found {}", matches.len());
    }
    Ok(matches[0])
}

fn has_playback_child(children: &[Job], cue: DiscordVoicePlaybackCue) -> bool {
    children.iter().any(|child| {
        child
            .discord_voice_playback_payload()
            .is_some_and(|payload| payload.cue == cue)
    })
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::{Mutex as StdMutex, MutexGuard};

    use super::*;
    use crate::runtime::{AgentRuntime, ControlConfig, RuntimeSessionStatus};

    #[test]
    fn join_room_placement_creates_discord_voice_join_child_job() {
        let raw = tempfile::tempdir().unwrap();
        let _env = test_state_dir(raw.path());
        let store =
            crate::runtime::timeline::TimelineStore::new(Some(raw.path().join("voice"))).unwrap();
        let room = test_room();
        let mut runtime = test_runtime(store);
        runtime.bots.insert("clanky-vc1".to_string(), ready_bot());

        let decision = runtime
            .prepare_join_room_jobs(room.clone(), "user-a", "auto_join")
            .unwrap();

        let JobDecision::WaitFor(children) = decision else {
            panic!("expected placement to wait on adapter child job");
        };
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].kind, JobKind::DiscordVoiceJoin);
        let payload = children[0].discord_voice_join_payload().unwrap();
        assert_eq!(payload.room, room);
        assert_eq!(payload.bot_id, "clanky-vc1");
        assert_eq!(payload.requested_by_user_id, "user-a");
        assert!(!payload.capture_run_id.trim().is_empty());
        assert_eq!(
            runtime.bots.get("clanky-vc1").unwrap().joining_session_id,
            payload.capture_run_id
        );
    }

    #[test]
    fn join_room_placement_treats_pending_voice_join_as_channel_reservation() {
        let raw = tempfile::tempdir().unwrap();
        let _env = test_state_dir(raw.path());
        let store =
            crate::runtime::timeline::TimelineStore::new(Some(raw.path().join("voice"))).unwrap();
        let room = test_room();
        let pending = Job::discord_voice_join(DiscordVoiceJoinPayload {
            room: room.clone(),
            bot_id: "clanky-vc1".to_string(),
            capture_run_id: "cap_joining".to_string(),
            assignment_id: "assign_joining".to_string(),
            started_at: utc_now(),
            session_dir: raw.path().join("joining"),
            requested_by_user_id: "user-a".to_string(),
            reason: "explicit_request".to_string(),
        });
        store.create_job(pending).unwrap();
        let mut runtime = test_runtime(store);
        runtime.bots.insert(
            "clanky-vc2".to_string(),
            ready_bot_with("clanky-vc2", "bot-user-2"),
        );

        let decision = runtime
            .prepare_join_room_jobs(room, "user-b", "explicit_request")
            .unwrap();

        let JobDecision::Complete(JobOutput::RoomAgentPlacement(output)) = decision else {
            panic!("expected placement to complete without allocating a second bot");
        };
        assert_eq!(output.status, "already_joining");
        assert_eq!(output.bot_id, "clanky-vc1");
        assert_eq!(output.capture_run_id, "cap_joining");
        assert!(runtime.bots["clanky-vc2"].joining_session_id.is_empty());
    }

    #[test]
    fn duplicate_voice_bot_sessions_for_room_returns_all_but_oldest_session() {
        let raw = tempfile::tempdir().unwrap();
        let _env = test_state_dir(raw.path());
        let store =
            crate::runtime::timeline::TimelineStore::new(Some(raw.path().join("voice"))).unwrap();
        let room = test_room();
        let mut runtime = test_runtime(store);
        runtime.sessions.insert(
            "cap_newer".to_string(),
            RuntimeSessionStatus {
                session_id: "cap_newer".to_string(),
                guild_id: room.guild_id.clone(),
                voice_channel_id: room.channel_id.clone(),
                bot_id: "clanky-vc2".to_string(),
                started_at: "2026-05-15T00:00:02.000Z".to_string(),
                active: true,
                ..RuntimeSessionStatus::default()
            },
        );
        runtime.sessions.insert(
            "cap_older".to_string(),
            RuntimeSessionStatus {
                session_id: "cap_older".to_string(),
                guild_id: room.guild_id.clone(),
                voice_channel_id: room.channel_id.clone(),
                bot_id: "clanky-vc1".to_string(),
                started_at: "2026-05-15T00:00:01.000Z".to_string(),
                active: true,
                ..RuntimeSessionStatus::default()
            },
        );

        let duplicates = runtime.duplicate_voice_bot_sessions_for_room(&room);

        assert_eq!(duplicates.len(), 1);
        assert_eq!(duplicates[0].session_id, "cap_newer");
        assert_eq!(
            runtime.active_session_id_for_room(&room).unwrap(),
            "cap_older"
        );
    }

    #[test]
    fn room_placement_resume_commits_discord_voice_join_output() {
        let raw = tempfile::tempdir().unwrap();
        let _env = test_state_dir(raw.path());
        let store =
            crate::runtime::timeline::TimelineStore::new(Some(raw.path().join("voice"))).unwrap();
        let room = test_room();
        let mut runtime = test_runtime(store.clone());
        let parent = store
            .create_job(Job::room_agent_placement(
                &room.guild_id,
                &room.channel_id,
                &room.room_id,
                RoomAgentPlacementAction::Join,
                "auto_join",
                "test-placement",
                None,
            ))
            .unwrap();
        let join_payload = DiscordVoiceJoinPayload {
            room: room.clone(),
            bot_id: "clanky-vc1".to_string(),
            capture_run_id: "cap_1".to_string(),
            assignment_id: "assign_1".to_string(),
            started_at: utc_now(),
            session_dir: raw.path().join("session"),
            requested_by_user_id: "user-a".to_string(),
            reason: "auto_join".to_string(),
        };
        let child = store
            .create_child_job(&parent, Job::discord_voice_join(join_payload))
            .unwrap();
        let mut completed_child = store.get_job(&child.id).unwrap();
        completed_child.mark_complete();
        completed_child.metadata.output =
            Some(JobOutput::DiscordVoiceJoin(DiscordVoiceJoinOutput {
                status: "assigned".to_string(),
                session: Some(RuntimeSessionStatus {
                    session_id: "cap_1".to_string(),
                    room_id: room.room_id.clone(),
                    guild_id: room.guild_id.clone(),
                    channel_id: room.channel_id.clone(),
                    voice_channel_id: room.channel_id.clone(),
                    channel_name: room.channel_name.clone(),
                    bot_id: "clanky-vc1".to_string(),
                    capture_run_id: "cap_1".to_string(),
                    assignment_id: "assign_1".to_string(),
                    active: true,
                    ..RuntimeSessionStatus::default()
                }),
                bot_status: Some(RuntimeBotStatus {
                    bot_id: "clanky-vc1".to_string(),
                    ready: true,
                    assigned_session_id: "cap_1".to_string(),
                    ..RuntimeBotStatus::default()
                }),
                message: String::new(),
            }));
        store.update_job(&completed_child).unwrap();

        let payload = parent.room_agent_placement_payload().unwrap();
        let decision = runtime
            .resume_room_agent_placement_job(&parent, payload)
            .unwrap();

        let JobDecision::WaitFor(playback_jobs) = decision else {
            panic!("expected join placement to wait on playback");
        };
        assert_eq!(playback_jobs.len(), 1);
        assert_eq!(playback_jobs[0].kind, JobKind::DiscordVoicePlayback);
        assert_eq!(
            playback_jobs[0]
                .discord_voice_playback_payload()
                .unwrap()
                .cue,
            DiscordVoicePlaybackCue::Join
        );
        let playback_child = store
            .create_child_job(&parent, playback_jobs.into_iter().next().unwrap())
            .unwrap();
        let mut completed_playback = store.get_job(&playback_child.id).unwrap();
        completed_playback.set_state(JobState::Failed);
        completed_playback.metadata.error = "missing cue asset".to_string();
        store.update_job(&completed_playback).unwrap();

        let decision = runtime
            .resume_room_agent_placement_job(&parent, payload)
            .unwrap();
        let JobDecision::Complete(JobOutput::RoomAgentPlacement(output)) = decision else {
            panic!("expected completed room placement output after playback");
        };
        assert_eq!(output.status, "assigned");
        assert_eq!(runtime.sessions["cap_1"].channel_id, room.channel_id);
        assert_eq!(runtime.bots["clanky-vc1"].assigned_session_id, "cap_1");
    }

    fn test_runtime(timeline_store: crate::runtime::timeline::TimelineStore) -> Runtime {
        Runtime {
            started_at: utc_now(),
            guilds: BTreeMap::new(),
            rooms: BTreeMap::new(),
            control_config: ControlConfig::default(),
            room_controls: BTreeMap::new(),
            sessions: BTreeMap::new(),
            bots: BTreeMap::new(),
            agents: AgentRuntime::default(),
            automations: BTreeMap::new(),
            timeline_store,
            auto_join_enabled: true,
            manual_leave_cooldown_seconds: 20 * 60,
            manual_join_hold_seconds: 60 * 60,
            pause_release_seconds: 20 * 60,
        }
    }

    fn test_room() -> RoomConfig {
        RoomConfig {
            room_id: "code-lounge".to_string(),
            guild_id: "guild".to_string(),
            guild_slug: "guild".to_string(),
            channel_id: "code".to_string(),
            channel_slug: "code-lounge".to_string(),
            channel_name: "Code Lounge".to_string(),
            auto_join: true,
        }
    }

    fn ready_bot() -> RuntimeBotStatus {
        ready_bot_with("clanky-vc1", "bot-user")
    }

    fn ready_bot_with(bot_id: &str, user_id: &str) -> RuntimeBotStatus {
        RuntimeBotStatus {
            bot_id: bot_id.to_string(),
            ready: true,
            user_id: user_id.to_string(),
            username: bot_id.to_string(),
            ..RuntimeBotStatus::default()
        }
    }

    fn test_state_dir(root: &std::path::Path) -> MutexGuard<'static, ()> {
        static ENV_LOCK: StdMutex<()> = StdMutex::new(());
        let guard = ENV_LOCK.lock().unwrap();
        // Environment mutation is process-global; this test module serializes it.
        unsafe {
            std::env::set_var("CLANKCORD_STATE_DIR", root.join("state"));
        }
        guard
    }
}
