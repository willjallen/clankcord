use serde_json::{Value, json};

use crate::Result;
use crate::config::local_tz;

use crate::domain::Ctx;
use crate::domain::rooms::catalog;
use crate::domain::rooms::control_state;
use crate::model::job::JobState;
use crate::model::rooms::RoomConfig;
use crate::model::voice::{VoiceAssignment, VoiceBotStatus, VoiceCaptureSessionStatus};
use crate::time::format_timestamp_local;
use crate::util::first_non_empty;

pub async fn status_for_room(ctx: &Ctx, room: &RoomConfig) -> Result<Value> {
    let bots = ctx.store.list_voice_bot_states().await?;
    let sessions = ctx.store.list_active_capture_sessions().await?;
    let assignments = ctx.store.list_active_voice_assignments().await?;
    let assignment = active_assignment_for_room(&assignments, room);
    let session = match active_session_for_room(&sessions, room) {
        Some(session) => Some(enrich_session_status(ctx, session).await),
        None => None,
    };
    let occupancy = ctx
        .store
        .get_occupancy(&room.guild_id, &room.channel_id)
        .await?;
    let retention_policy = occupancy
        .get("retention_policy")
        .cloned()
        .unwrap_or_else(|| {
            json!({
                "transcript_events": "forever",
                "source_audio": "7d",
                "job_metadata": "forever"
            })
        });
    let live_publications = ctx
        .store
        .list_publications(
            Some(&room.guild_id),
            Some(&room.channel_id),
            Some("live_draft_published"),
        )
        .await?;
    let active_jobs = ctx
        .store
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
        .filter(|job| job.scope_id == room.channel_id && !job.state.is_terminal())
        .map(|job| job.public_view())
        .collect::<Vec<_>>();
    Ok(json!({
        "room": room.to_json(),
        "mode": session.as_ref().map(|value| value.mode.as_str()).unwrap_or("absent"),
        "assignmentState": assignment.as_ref().map(|value| value.state.as_str()).unwrap_or("absent"),
        "assignedVoiceBotId": assignment.as_ref().map(|value| value.voice_bot_id.as_str()).or_else(|| session.as_ref().map(|value| value.bot_id.as_str())).unwrap_or(""),
        "captureRunId": assignment.as_ref().map(|value| value.capture_run_id.as_str()).or_else(|| session.as_ref().map(|value| value.capture_run_id.as_str())).unwrap_or(""),
        "retentionPolicy": retention_policy,
        "control": control_state::room_control_status(ctx, room).await?,
        "occupancy": occupancy,
        "livePublications": live_publications,
        "activeJobs": active_jobs,
        "assignment": assignment.map(|value| value.to_json()),
        "session": session.map(|value| value.to_json()),
        "bots": bots.iter().map(VoiceBotStatus::to_json).collect::<Vec<_>>(),
    }))
}

pub async fn status_payload(ctx: &Ctx, room_identifier: Option<&str>) -> Result<Value> {
    if let Some(identifier) = room_identifier.filter(|value| !value.trim().is_empty()) {
        return match catalog::room_for_identifier(ctx, Some(identifier)).await {
            Ok(room) => status_for_room(ctx, &room).await,
            Err(error) => Ok(json!({"ok": false, "error": error.to_string()})),
        };
    }
    let bots = ctx.store.list_voice_bot_states().await?;
    let active_assignments = ctx.store.list_active_voice_assignments().await?;
    let active_sessions = ctx.store.list_active_capture_sessions().await?;
    let mut sessions = Vec::new();
    for session in active_sessions.iter().cloned() {
        sessions.push(enrich_session_status(ctx, session).await.to_json());
    }
    let mut rooms = Vec::new();
    for room in catalog::known_rooms(ctx).await? {
        let occupancy = ctx
            .store
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
                "control": control_state::room_control_status(ctx, &room).await?,
                "occupancy": occupancy,
            }));
    }
    Ok(json!({
        "bots": bots.iter().map(VoiceBotStatus::to_json).collect::<Vec<_>>(),
        "pool": capacity_payload(ctx).await,
        "sessions": sessions,
        "assignments": active_assignments.iter().map(VoiceAssignment::to_json).collect::<Vec<_>>(),
        "rooms": rooms,
        "roomControls": control_state::room_controls_json(ctx).await?,
    }))
}

pub async fn capacity_payload(ctx: &Ctx) -> Value {
    let bots = ctx.store.list_voice_bot_states().await.unwrap_or_default();
    let assignments = ctx
        .store
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
    ctx: &Ctx,
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
    let Ok((event_count, last_transcript_at)) = ctx
        .store
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
        session.capture_stats.last_transcript_at = fields.get("iso").cloned().unwrap_or_default();
        session.capture_stats.last_transcript_at_local =
            fields.get("local_iso").cloned().unwrap_or_default();
    }
    session
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
