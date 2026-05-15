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
        )
        .await?;
        self.persist_status_snapshot().await?;
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
            )
            .await?;
            let session_id = self.active_session_id_for_room(&room);
            if let Some(session_id) = session_id {
                if let Some(mut session) = self.sessions.remove(&session_id) {
                    session.mark_ended(isoformat_z(None));
                    results.push(session.to_json());
                }
            }
            self.persist_status_snapshot().await?;
            return Ok(
                json!({"action": "leave", "status": "ok", "roomId": room.room_id, "results": results}),
            );
        }
        for session in self.sessions.values_mut() {
            session.mark_ended(isoformat_z(None));
            results.push(session.to_json());
        }
        self.sessions.clear();
        self.persist_status_snapshot().await?;
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

    pub(crate) async fn prepare_join_room_jobs(
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
        if let Some(join) = self.active_voice_join_for_room(&room).await? {
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
            )
            .await?;
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
        let run = self
            .timeline_store
            .create_capture_run(CaptureRunInput {
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
            })
            .await?;
        let capture_run_id = first_value_string(&run, &["capture_run_id", "captureRunId"]);
        let assignment_id = first_value_string(&run, &["assignment_id", "assignmentId"]);
        self.mark_bot_joining(&bot.bot_id, &capture_run_id, "")
            .await?;
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

    pub(crate) async fn commit_join_room_job(
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
            self.timeline_store
                .set_occupancy(json!({
                    "guild_id": room.guild_id,
                    "guildId": room.guild_id,
                    "voice_channel_id": room.channel_id,
                    "channelId": room.channel_id,
                    "voice_channel_name": room.channel_name,
                    "channelName": room.channel_name,
                    "updated_at": isoformat_z(None),
                }))
                .await?;
            self.persist_status_snapshot().await?;
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
            self.persist_status_snapshot().await?;
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

    pub(crate) async fn fail_join_room_job(
        &mut self,
        request: &DiscordVoiceJoinPayload,
        error: &str,
    ) -> Result<()> {
        self.mark_bot_join_failed(&request.bot_id, error).await?;
        let _ = self
            .suppress_room_auto_join(
                &request.room,
                self.manual_leave_cooldown_seconds,
                "join_failed",
                &request.requested_by_user_id,
                true,
            )
            .await;
        let _ = self
            .timeline_store
            .close_capture_run(
                &request.room.guild_id,
                &request.room.channel_id,
                &request.capture_run_id,
                Some(utc_now()),
                "join_failed",
                "failed",
            )
            .await;
        Ok(())
    }

    pub(crate) async fn prepare_leave_room_jobs(
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
            )
            .await?;
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
            self.persist_status_snapshot().await?;
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
            self.persist_status_snapshot().await?;
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

    pub(crate) async fn resume_room_agent_placement_job(
        &mut self,
        job: &Job,
        payload: &RoomAgentPlacementPayload,
    ) -> Result<JobDecision> {
        let children = self.timeline_store.list_child_jobs(&job.id).await?;
        if children.iter().any(|child| !child.state.is_terminal()) {
            return Ok(JobDecision::Wait);
        }
        if let Some(failed) = children.iter().find(|child| {
            child.kind != JobKind::DiscordVoicePlayback && child.state != JobState::Complete
        }) {
            if let Some(join_payload) = failed.discord_voice_join_payload() {
                self.fail_join_room_job(join_payload, &failed.metadata.error)
                    .await?;
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
                        let placement_output = self.commit_join_room_job(request, output).await?;
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
                                self.commit_finished_room_session(output, &reason).await?
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
                self.persist_status_snapshot().await?;
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

    pub(crate) async fn sync_voice_adapter_status(
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
                let _ = self
                    .timeline_store
                    .close_capture_run(
                        &session.guild_id,
                        &session.voice_channel_id,
                        &capture_run_id,
                        Some(utc_now()),
                        "adapter_sync_missing",
                        "ended",
                    )
                    .await;
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
        self.persist_status_snapshot().await
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

    async fn active_voice_join_for_room(&self, room: &RoomConfig) -> Result<Option<Job>> {
        Ok(self
            .timeline_store
            .list_jobs_by_scope_kind(&room.guild_id, &room.channel_id, JobKind::DiscordVoiceJoin)
            .await?
            .into_iter()
            .find(|job| !job.state.is_terminal()))
    }

    async fn mark_bot_joining(
        &mut self,
        bot_id: &str,
        session_id: &str,
        error: &str,
    ) -> Result<()> {
        if let Some(status) = self.bots.get_mut(bot_id) {
            status.joining_session_id = session_id.to_string();
            status.last_error = error.to_string();
        }
        self.persist_status_snapshot().await
    }

    async fn mark_bot_join_failed(&mut self, bot_id: &str, error: &str) -> Result<()> {
        if let Some(status) = self.bots.get_mut(bot_id) {
            status.joining_session_id.clear();
            status.last_error = error.to_string();
        }
        self.persist_status_snapshot().await
    }

    async fn commit_finished_room_session(
        &mut self,
        result: DiscordVoiceLeaveOutput,
        reason: &str,
    ) -> Result<Option<RuntimeSessionStatus>> {
        for job in result.audio_jobs {
            self.timeline_store.create_job(job).await?;
        }
        if !result.capture_run_id.trim().is_empty() {
            self.timeline_store
                .close_capture_run(
                    &result.guild_id,
                    &result.voice_channel_id,
                    &result.capture_run_id,
                    Some(utc_now()),
                    reason,
                    "ended",
                )
                .await?;
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
