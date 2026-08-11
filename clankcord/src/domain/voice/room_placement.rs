use serde_json::json;

use crate::Result;
use crate::engine::JobDecision;
use crate::time::{isoformat_z, parse_instant, utc_now};
use crate::util::{first_non_empty, single_child_of_kind};

use crate::domain::Ctx;
use crate::domain::rooms::catalog;
use crate::domain::rooms::control_state;
use crate::domain::voice::playback;
use crate::model::job::{
    DiscordVoiceJoinOutput, DiscordVoiceJoinPayload, DiscordVoiceLeaveOutput,
    DiscordVoiceLeavePayload, DiscordVoicePlaybackCue, Job, JobKind, JobOutput, JobState,
    RoomAgentPlacementAction, RoomAgentPlacementOutput, RoomAgentPlacementPayload,
};
use crate::model::rooms::RoomConfig;
use crate::model::voice::{VoiceAssignment, VoiceBotStatus, VoiceCaptureSessionStatus};

pub(crate) async fn plan_join_room_jobs(
    ctx: &Ctx,
    room: RoomConfig,
    requested_by_user_id: &str,
    reason: &str,
) -> Result<JobDecision> {
    let pool = ctx.store.runtime_pool_config().await?;
    if let Some(assignment) = active_assignments_for_room(ctx, &room)
        .await?
        .into_iter()
        .next()
    {
        let session = session_for_assignment(ctx, &assignment).await?;
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
    if let Some(bot) = voice_bot_currently_in_room_from_store(ctx, &room).await? {
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
    if let Some(join) = active_voice_join_for_room(ctx, &room).await? {
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
        control_state::set_room_manual_hold(
            ctx,
            &room,
            pool.manual_override_seconds,
            reason,
            requested_by_user_id,
        )
        .await?;
    }

    let Some(assignment) = ctx
        .store
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
                bots: ctx.store.list_voice_bot_states().await?,
                message: "No configured Discord voice bot is ready and unassigned.".to_string(),
            },
        )));
    };
    let started_at = parse_instant(&assignment.assigned_at).unwrap_or_else(utc_now);
    let session_dir = session_directory(ctx, &room, started_at, &assignment.capture_run_id);

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
    ctx: &Ctx,
    request: &DiscordVoiceJoinPayload,
    result: DiscordVoiceJoinOutput,
) -> Result<JobOutput> {
    let room = request.room.clone();
    if let Some(status) = result.bot_status {
        ctx.store.upsert_voice_bot_state(&status).await?;
    }
    if let Some(session) = result.session {
        ctx.store.upsert_capture_session_status(&session).await?;
        ctx.store
            .mark_voice_assignment_capturing(&request.assignment_id)
            .await?;
        ctx.store
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
    ctx: &Ctx,
    request: &DiscordVoiceJoinPayload,
    error: &str,
) -> Result<()> {
    let pool = ctx.store.runtime_pool_config().await?;
    control_state::suppress_room_auto_join(
        ctx,
        &request.room,
        pool.auto_rejoin_cooldown_seconds,
        "join_failed",
        &request.requested_by_user_id,
        true,
    )
    .await?;
    ctx.store
        .mark_voice_assignment_failed(&request.assignment_id, error)
        .await?;
    ctx.store
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

pub(crate) async fn plan_leave_room_jobs(
    ctx: &Ctx,
    room_identifier: Option<&str>,
    cooldown_seconds: i64,
    requested_by_user_id: &str,
    source_job_id: &str,
    reason: &str,
) -> Result<JobDecision> {
    let leave_reason = normalized_leave_reason(reason);
    if let Some(identifier) = room_identifier.filter(|value| !value.trim().is_empty()) {
        let room = catalog::room_for_identifier(ctx, Some(identifier)).await?;
        control_state::suppress_room_auto_join(
            ctx,
            &room,
            cooldown_seconds,
            leave_reason,
            requested_by_user_id,
            true,
        )
        .await?;
        if let Some(assignment) = active_assignments_for_room(ctx, &room)
            .await?
            .into_iter()
            .next()
        {
            ctx.store
                .mark_voice_assignment_leaving(&assignment.assignment_id, leave_reason)
                .await?;
            if let Some(session) = session_for_assignment(ctx, &assignment).await? {
                return Ok(JobDecision::WaitFor(vec![
                    playback::voice_playback_job_for_session(
                        ctx,
                        &session,
                        requested_by_user_id,
                        DiscordVoicePlaybackCue::Leave,
                        leave_reason,
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
                    reason: leave_reason.to_string(),
                },
            )]));
        }
        if voice_bot_currently_in_room_from_store(ctx, &room)
            .await?
            .is_some()
        {
            return Ok(JobDecision::WaitFor(vec![Job::discord_voice_leave(
                room.guild_id.clone(),
                room.channel_id.clone(),
                requested_by_user_id,
                DiscordVoiceLeavePayload {
                    session_id: String::new(),
                    reason: leave_reason.to_string(),
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
                reason: leave_reason.to_string(),
                session: None,
                sessions: Vec::new(),
                bots: Vec::new(),
                message: String::new(),
            },
        )));
    }

    let mut requests = Vec::new();
    let mut requested_sessions = std::collections::BTreeSet::new();
    for assignment in ctx.store.list_active_voice_assignments().await? {
        ctx.store
            .mark_voice_assignment_leaving(&assignment.assignment_id, "manual_leave_all")
            .await?;
        if let Some(session) = session_for_assignment(ctx, &assignment).await? {
            requested_sessions.insert(session.session_id.clone());
            requests.push(playback::voice_playback_job_for_session(
                ctx,
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
    for session in ctx.store.list_active_capture_sessions().await? {
        if requested_sessions.contains(&session.session_id) {
            continue;
        }
        requests.push(playback::voice_playback_job_for_session(
            ctx,
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
    ctx: &Ctx,
    job: &Job,
    payload: &RoomAgentPlacementPayload,
) -> Result<JobDecision> {
    let children = ctx.store.list_child_jobs(&job.id).await?;
    if children.iter().any(|child| !child.state.is_terminal()) {
        return Ok(JobDecision::Wait);
    }
    if let Some(failed) = children.iter().find(|child| {
        child.kind != JobKind::DiscordVoicePlayback && child.state != JobState::Complete
    }) {
        if let Some(join_payload) = failed.discord_voice_join_payload() {
            fail_join_room_job(ctx, join_payload, &failed.metadata.error).await?;
        }
        return Ok(JobDecision::fail(format!(
            "room placement dependency {} ended as {}: {}",
            failed.id, failed.state, failed.metadata.error
        )));
    }
    match payload.action {
        RoomAgentPlacementAction::Join => {
            let child = single_child_of_kind(&children, JobKind::DiscordVoiceJoin)?;
            let request = child
                .discord_voice_join_payload()
                .ok_or_else(|| anyhow::anyhow!("join child {} has no join payload", child.id))?;
            match child.metadata.output.clone() {
                Some(JobOutput::DiscordVoiceJoin(output)) => {
                    let placement_output = commit_join_room_job(ctx, request, output).await?;
                    if !has_playback_child(&children, DiscordVoicePlaybackCue::Join)
                        && let JobOutput::RoomAgentPlacement(output) = &placement_output
                        && let Some(session) = &output.session
                    {
                        return Ok(JobDecision::WaitFor(vec![
                            playback::voice_playback_job_for_session(
                                ctx,
                                session,
                                &request.requested_by_user_id,
                                DiscordVoicePlaybackCue::Join,
                                "room_join",
                                &job.id,
                            ),
                        ]));
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
                            child.scope_id.clone(),
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
                            commit_finished_room_session(ctx, output, &reason).await?
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

async fn active_voice_join_for_room(ctx: &Ctx, room: &RoomConfig) -> Result<Option<Job>> {
    Ok(ctx
        .store
        .list_active_jobs_by_scope_kind(&room.guild_id, &room.channel_id, JobKind::DiscordVoiceJoin)
        .await?
        .into_iter()
        .next())
}

async fn active_assignments_for_room(ctx: &Ctx, room: &RoomConfig) -> Result<Vec<VoiceAssignment>> {
    ctx.store
        .list_active_voice_assignments_for_room(&room.guild_id, &room.channel_id)
        .await
}

async fn session_for_assignment(
    ctx: &Ctx,
    assignment: &VoiceAssignment,
) -> Result<Option<VoiceCaptureSessionStatus>> {
    Ok(ctx
        .store
        .list_active_capture_sessions_for_room(&assignment.guild_id, &assignment.voice_channel_id)
        .await?
        .into_iter()
        .find(|session| {
            session.assignment_id == assignment.assignment_id
                || session.capture_run_id == assignment.capture_run_id
                || session.session_id == assignment.capture_run_id
        }))
}

async fn voice_bot_currently_in_room_from_store(
    ctx: &Ctx,
    room: &RoomConfig,
) -> Result<Option<VoiceBotStatus>> {
    Ok(ctx
        .store
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
    ctx: &Ctx,
    result: DiscordVoiceLeaveOutput,
    reason: &str,
) -> Result<Option<VoiceCaptureSessionStatus>> {
    for job in result.audio_jobs {
        ctx.store.create_job(job).await?;
    }
    let capture_run_id = first_non_empty([
        result.capture_run_id.clone(),
        ctx.store
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
        ctx.store
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
        ctx.store.upsert_voice_bot_state(&status).await?;
    }
    if let Some(mut session) = result.session {
        session.mark_ended(isoformat_z(None));
        ctx.store.upsert_capture_session_status(&session).await?;
        Ok(Some(session))
    } else if let Some(mut session) = ctx
        .store
        .get_capture_session_status(&result.session_id)
        .await?
    {
        session.mark_ended(isoformat_z(None));
        ctx.store.upsert_capture_session_status(&session).await?;
        Ok(Some(session))
    } else {
        Ok(None)
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
    runtime: &Ctx,
    room: &RoomConfig,
    started_at: chrono::DateTime<chrono::Utc>,
    session_id: &str,
) -> std::path::PathBuf {
    runtime
        .store
        .capture_run_scratch_dir(&room.guild_id, &room.channel_id, started_at, session_id)
}

fn should_record_manual_hold_for_join(reason: &str) -> bool {
    !matches!(reason, "auto_join" | "manual_hold")
}

fn normalized_leave_reason(reason: &str) -> &'static str {
    match reason {
        "auto_policy_empty" => "auto_policy_empty",
        "auto_policy_single_deafened" => "auto_policy_single_deafened",
        "duplicate_voice_bot_in_channel" => "duplicate_voice_bot_in_channel",
        "orphan_voice_bot_presence" => "orphan_voice_bot_presence",
        _ => "manual_leave",
    }
}
