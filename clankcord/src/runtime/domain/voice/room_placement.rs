use serde_json::{Value, json};

use crate::Result;
use crate::config::local_tz;
use crate::runtime::core::execution::JobDecision;
use crate::runtime::timeline::{isoformat_z, parse_instant, utc_now};
use crate::runtime::util::{first_non_empty, single_child_of_kind};

use crate::runtime::{
    DiscordVoiceJoinOutput, DiscordVoiceJoinPayload, DiscordVoiceLeaveOutput,
    DiscordVoiceLeavePayload, DiscordVoicePlaybackCue, Job, JobKind, JobOutput, JobState,
    RoomAgentPlacementAction, RoomAgentPlacementOutput, RoomAgentPlacementPayload, RoomConfig,
    Runtime, VoiceAssignment, VoiceBotStatus, VoiceCaptureSessionStatus,
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
            for assignment in self.active_assignments_for_room(&room).await? {
                let _ = self
                    .timeline_store
                    .mark_voice_assignment_leaving(&assignment.assignment_id, "manual_leave")
                    .await?;
                results.push(assignment.to_json());
            }
            return Ok(
                json!({"action": "leave", "status": "ok", "roomId": room.room_id, "results": results}),
            );
        }
        for assignment in self.timeline_store.list_active_voice_assignments().await? {
            let _ = self
                .timeline_store
                .mark_voice_assignment_leaving(&assignment.assignment_id, "manual_leave_all")
                .await?;
            results.push(assignment.to_json());
        }
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
        if let Some(assignment) = self
            .active_assignments_for_room(&room)
            .await?
            .into_iter()
            .next()
        {
            let session = self.session_for_assignment(&assignment).await?;
            let status = if assignment.state == "joining" {
                "already_joining"
            } else {
                "already_assigned"
            };
            return Ok(JobDecision::Complete(JobOutput::RoomAgentPlacement(
                RoomAgentPlacementOutput {
                    action: RoomAgentPlacementAction::Join,
                    status: status.to_string(),
                    room,
                    bot_id: assignment.voice_bot_id.clone(),
                    capture_run_id: assignment.capture_run_id.clone(),
                    requested_by_user_id: requested_by_user_id.to_string(),
                    reason: reason.to_string(),
                    session,
                    sessions: Vec::new(),
                    bots: Vec::new(),
                    message: String::new(),
                },
            )));
        }
        if let Some(bot) = self.voice_bot_currently_in_room_from_store(&room).await? {
            return Ok(JobDecision::Complete(JobOutput::RoomAgentPlacement(
                RoomAgentPlacementOutput {
                    action: RoomAgentPlacementAction::Join,
                    status: "already_assigned".to_string(),
                    room,
                    bot_id: bot.bot_id,
                    capture_run_id: String::new(),
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

        let Some(assignment) = self
            .timeline_store
            .claim_voice_assignment_for_room(&room, reason)
            .await?
        else {
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
                    bots: self.timeline_store.list_voice_bot_states().await?,
                    message: "No configured Discord voice bot is ready and unassigned.".to_string(),
                },
            )));
        };
        let started_at = parse_instant(&assignment.assigned_at).unwrap_or_else(utc_now);
        let session_dir = session_directory(self, &room, started_at, &assignment.capture_run_id);

        Ok(JobDecision::WaitFor(vec![Job::discord_voice_join(
            DiscordVoiceJoinPayload {
                room,
                bot_id: assignment.voice_bot_id,
                capture_run_id: assignment.capture_run_id,
                assignment_id: assignment.assignment_id,
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
            self.timeline_store.upsert_voice_bot_state(&status).await?;
        }
        if let Some(session) = result.session {
            self.timeline_store
                .upsert_capture_session_status(&session)
                .await?;
            self.timeline_store
                .mark_voice_assignment_capturing(&request.assignment_id)
                .await?;
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
        self.suppress_room_auto_join(
            &request.room,
            self.manual_leave_cooldown_seconds,
            "join_failed",
            &request.requested_by_user_id,
            true,
        )
        .await?;
        self.timeline_store
            .mark_voice_assignment_failed(&request.assignment_id, error)
            .await?;
        self.timeline_store
            .close_capture_run(
                &request.room.guild_id,
                &request.room.channel_id,
                &request.capture_run_id,
                Some(utc_now()),
                "join_failed",
                "failed",
            )
            .await?;
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
            if let Some(assignment) = self
                .active_assignments_for_room(&room)
                .await?
                .into_iter()
                .next()
            {
                self.timeline_store
                    .mark_voice_assignment_leaving(&assignment.assignment_id, "manual_leave")
                    .await?;
                if let Some(session) = self.session_for_assignment(&assignment).await? {
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
                return Ok(JobDecision::WaitFor(vec![Job::discord_voice_leave(
                    room.guild_id.clone(),
                    room.channel_id.clone(),
                    requested_by_user_id,
                    DiscordVoiceLeavePayload {
                        session_id: assignment.capture_run_id.clone(),
                        reason: "manual_leave".to_string(),
                    },
                )]));
            }
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

        let mut requests = Vec::new();
        let mut requested_sessions = std::collections::BTreeSet::new();
        for assignment in self.timeline_store.list_active_voice_assignments().await? {
            self.timeline_store
                .mark_voice_assignment_leaving(&assignment.assignment_id, "manual_leave_all")
                .await?;
            if let Some(session) = self.session_for_assignment(&assignment).await? {
                requested_sessions.insert(session.session_id.clone());
                requests.push(self.voice_playback_job_for_session(
                    &session,
                    requested_by_user_id,
                    DiscordVoicePlaybackCue::Leave,
                    "manual_leave_all",
                    source_job_id,
                ));
            } else {
                requests.push(Job::discord_voice_leave(
                    assignment.guild_id.clone(),
                    assignment.voice_channel_id.clone(),
                    requested_by_user_id,
                    DiscordVoiceLeavePayload {
                        session_id: assignment.capture_run_id.clone(),
                        reason: "manual_leave_all".to_string(),
                    },
                ));
            }
        }
        for session in self.timeline_store.list_active_capture_sessions().await? {
            if requested_sessions.contains(&session.session_id) {
                continue;
            }
            requests.push(self.voice_playback_job_for_session(
                &session,
                requested_by_user_id,
                DiscordVoicePlaybackCue::Leave,
                "manual_leave_all",
                source_job_id,
            ));
        }
        if requests.is_empty() {
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

    async fn active_voice_join_for_room(&self, room: &RoomConfig) -> Result<Option<Job>> {
        Ok(self
            .timeline_store
            .list_jobs_by_scope_kind(&room.guild_id, &room.channel_id, JobKind::DiscordVoiceJoin)
            .await?
            .into_iter()
            .find(|job| !job.state.is_terminal()))
    }

    async fn active_assignments_for_room(&self, room: &RoomConfig) -> Result<Vec<VoiceAssignment>> {
        self.timeline_store
            .list_active_voice_assignments_for_room(&room.guild_id, &room.channel_id)
            .await
    }

    async fn session_for_assignment(
        &self,
        assignment: &VoiceAssignment,
    ) -> Result<Option<VoiceCaptureSessionStatus>> {
        Ok(self
            .timeline_store
            .list_active_capture_sessions_for_room(
                &assignment.guild_id,
                &assignment.voice_channel_id,
            )
            .await?
            .into_iter()
            .find(|session| {
                session.assignment_id == assignment.assignment_id
                    || session.capture_run_id == assignment.capture_run_id
                    || session.session_id == assignment.capture_run_id
            }))
    }

    async fn voice_bot_currently_in_room_from_store(
        &self,
        room: &RoomConfig,
    ) -> Result<Option<VoiceBotStatus>> {
        Ok(self
            .timeline_store
            .list_voice_bot_states()
            .await?
            .into_iter()
            .find(|status| {
                status.ready
                    && status.current_guild_id == room.guild_id
                    && status.current_channel_id == room.channel_id
            }))
    }

    async fn commit_finished_room_session(
        &mut self,
        result: DiscordVoiceLeaveOutput,
        reason: &str,
    ) -> Result<Option<VoiceCaptureSessionStatus>> {
        for job in result.audio_jobs {
            self.timeline_store.create_job(job).await?;
        }
        let capture_run_id = first_non_empty([
            result.capture_run_id.clone(),
            self.timeline_store
                .get_voice_assignment_by_capture_run(&result.session_id)
                .await?
                .map(|assignment| assignment.capture_run_id)
                .unwrap_or_default(),
            result.session_id.clone(),
        ]);
        let guild_id = first_non_empty([
            result.guild_id.clone(),
            result
                .session
                .as_ref()
                .map(|session| session.guild_id.clone())
                .unwrap_or_default(),
        ]);
        let voice_channel_id = first_non_empty([
            result.voice_channel_id.clone(),
            result
                .session
                .as_ref()
                .map(|session| session.voice_channel_id.clone())
                .unwrap_or_default(),
        ]);
        if !capture_run_id.trim().is_empty() {
            self.timeline_store
                .close_capture_run(
                    &guild_id,
                    &voice_channel_id,
                    &capture_run_id,
                    Some(utc_now()),
                    reason,
                    "ended",
                )
                .await?;
        }
        if let Some(status) = result.bot_status {
            self.timeline_store.upsert_voice_bot_state(&status).await?;
        }
        if let Some(mut session) = result.session {
            session.mark_ended(isoformat_z(None));
            self.timeline_store
                .upsert_capture_session_status(&session)
                .await?;
            Ok(Some(session))
        } else if let Some(mut session) = self
            .timeline_store
            .get_capture_session_status(&result.session_id)
            .await?
        {
            session.mark_ended(isoformat_z(None));
            self.timeline_store
                .upsert_capture_session_status(&session)
                .await?;
            Ok(Some(session))
        } else {
            Ok(None)
        }
    }
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
