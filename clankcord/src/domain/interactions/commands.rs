use serde_json::{Value, json};

use crate::Result;
use crate::domain::Ctx;
use crate::domain::interactions::agent_sessions;
use crate::domain::interactions::confirmations;
use crate::domain::interactions::requires_confirmation;
use crate::domain::rooms::catalog;
use crate::domain::rooms::control_state;
use crate::domain::transcripts;
use crate::domain::transcripts::{ForgetRequest, MaterializeTranscriptRequest};
use crate::domain::voice::playback;
use crate::engine::JobDecision;
use crate::model::job::{
    CommandKind, CommandRequest, DiscordVoiceMutePayload, DiscordVoicePlayAudioPayload,
    DiscordVoicePlaybackCue, Job, JobKind, JobOutput, RoomAgentPlacementAction,
};
use crate::model::scope::RuntimeScope;
use crate::time::{isoformat_z, utc_now};
use crate::util::string_field;

pub async fn create_command_job(
    ctx: &Ctx,
    command: CommandRequest,
    parent_job: Option<&Job>,
) -> Result<Value> {
    let mut command = command;
    let (guild_id, channel_id, _) = command_scope(ctx, &command).await?;
    command.guild_id = guild_id.clone();
    command.scope_id = channel_id.clone();
    if requires_confirmation(command.command_kind.as_str())
        && command.approved_by_user_id.trim().is_empty()
    {
        command.requires_confirmation = true;
    }
    if command.requires_confirmation {
        let confirmation_context =
            confirmations::confirmation_context_for_command(ctx, &command).await?;
        let job = Job::confirmation_required(
            RuntimeScope::voice_channel(guild_id.clone(), channel_id.clone()),
            command.requested_by_user_id.clone(),
            command,
            confirmation_context,
        );
        let job = if let Some(parent_job) = parent_job {
            ctx.store.create_child_job(parent_job, job).await?
        } else {
            ctx.store.create_job(job).await?
        };
        return Ok(json!({
            "kind": "confirmation_required",
            "job_ids": [job.id.clone()],
            "job": job.to_value()
        }));
    }

    let job = Job::command_request(
        RuntimeScope::voice_channel(guild_id.clone(), channel_id.clone()),
        command.requested_by_user_id.clone(),
        command,
    );
    let job = if let Some(parent_job) = parent_job {
        ctx.store.create_child_job(parent_job, job).await?
    } else {
        ctx.store.create_job(job).await?
    };
    Ok(json!({
        "kind": "command_created",
        "job_ids": [job.id.clone()],
        "job": job.to_value()
    }))
}

pub(crate) async fn execute_command_job(ctx: &Ctx, job: &Job) -> Result<JobDecision> {
    if job.kind != JobKind::Command {
        anyhow::bail!("job {} is not a command", job.id);
    }
    let command = job
        .command()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("command job {} has no command payload", job.id))?;
    prepare_command(ctx, command, job).await
}

async fn prepare_command(
    ctx: &Ctx,
    command: CommandRequest,
    parent_job: &Job,
) -> Result<JobDecision> {
    let pool = ctx.store.runtime_pool_config().await?;
    let command_kind = command.command_kind;
    let job_kind = command_kind.job_kind();
    let (guild_id, channel_id, target_room_identifier) = command_scope(ctx, &command).await?;
    match job_kind {
        "materialize_transcript" => {
            let (start, end) = command.window_times(None);
            let materialized = transcripts::materialize_transcript(
                ctx,
                MaterializeTranscriptRequest {
                    guild_id: guild_id.clone(),
                    channel_id: channel_id.clone(),
                    from: if command.arguments.from.trim().is_empty() {
                        isoformat_z(Some(start))
                    } else {
                        command.arguments.from.clone()
                    },
                    to: if command.arguments.to.trim().is_empty() {
                        isoformat_z(Some(end))
                    } else {
                        command.arguments.to.clone()
                    },
                    publish: if command.arguments.publish.trim().is_empty() {
                        "discord".to_string()
                    } else {
                        command.arguments.publish.clone()
                    },
                    live: command_kind == CommandKind::StartLiveTranscript,
                    created_by_user_id: command.requested_by_user_id.clone(),
                    parent_job_id: parent_job.id.clone(),
                    ..MaterializeTranscriptRequest::default()
                },
            )
            .await?;
            Ok(JobDecision::Complete(JobOutput::from_boundary_json(
                &json!({"kind": "materialize_transcript", "job_ids": [], "materialized": materialized}),
            )?))
        }
        "make_permanent" => {
            let end = utc_now();
            let start = end - chrono::Duration::minutes(30);
            let materialized = transcripts::materialize_transcript(
                ctx,
                MaterializeTranscriptRequest {
                    guild_id: guild_id.clone(),
                    channel_id: channel_id.clone(),
                    from: isoformat_z(Some(start)),
                    to: isoformat_z(Some(end)),
                    publish: "discord".to_string(),
                    created_by_user_id: command.requested_by_user_id.clone(),
                    parent_job_id: parent_job.id.clone(),
                    ..MaterializeTranscriptRequest::default()
                },
            )
            .await?;
            Ok(JobDecision::Complete(JobOutput::from_boundary_json(
                &json!({"kind": "make_permanent", "job_ids": [], "materialized": materialized}),
            )?))
        }
        "pause_listening" => {
            let room = catalog::room_for_identifier(ctx, Some(&target_room_identifier)).await?;
            control_state::pause_room(
                ctx,
                &room,
                command
                    .arguments
                    .duration_seconds
                    .unwrap_or(pool.pause_release_seconds),
                &command.requested_by_user_id,
            )
            .await?;
            Ok(JobDecision::Complete(JobOutput::from_boundary_json(
                &json!({"kind": "pause_listening", "job_ids": []}),
            )?))
        }
        "resume_listening" => {
            let room = catalog::room_for_identifier(ctx, Some(&target_room_identifier)).await?;
            control_state::resume_room(ctx, &room, &command.requested_by_user_id).await?;
            let _ = playback::create_voice_deafen_job_for_room(
                ctx,
                &room,
                &command.requested_by_user_id,
                false,
                "resume_listening",
                &parent_job.id,
            )
            .await?;
            let _ = playback::create_voice_playback_job_for_room(
                ctx,
                &room,
                &command.requested_by_user_id,
                DiscordVoicePlaybackCue::Undeafen,
                "resume_listening",
                &parent_job.id,
            )
            .await?;
            Ok(JobDecision::Complete(JobOutput::from_boundary_json(
                &json!({"kind": "resume_listening", "job_ids": []}),
            )?))
        }
        "deafen_listening" => {
            let room = catalog::room_for_identifier(ctx, Some(&target_room_identifier)).await?;
            let _ = playback::create_voice_deafen_job_for_room(
                ctx,
                &room,
                &command.requested_by_user_id,
                true,
                "deafen_listening",
                &parent_job.id,
            )
            .await?;
            let _ = playback::create_voice_playback_job_for_room(
                ctx,
                &room,
                &command.requested_by_user_id,
                DiscordVoicePlaybackCue::Deafen,
                "deafen_listening",
                &parent_job.id,
            )
            .await?;
            control_state::pause_room(
                ctx,
                &room,
                pool.pause_release_seconds,
                &command.requested_by_user_id,
            )
            .await?;
            Ok(JobDecision::Complete(JobOutput::from_boundary_json(
                &json!({"kind": "deafen_listening", "job_ids": []}),
            )?))
        }
        "set_voice_mute" => {
            let room = catalog::room_for_identifier(ctx, Some(&target_room_identifier)).await?;
            let session =
                playback::active_session_for_channel(ctx, &room.guild_id, &room.channel_id)
                    .await?
                    .ok_or_else(|| {
                        anyhow::anyhow!("room {} has no active voice session to mute", room.room_id)
                    })?;
            let muted = command.arguments.muted.unwrap_or(false);
            Ok(JobDecision::WaitFor(vec![Job::discord_voice_mute(
                room.guild_id,
                room.channel_id,
                command.requested_by_user_id.clone(),
                DiscordVoiceMutePayload {
                    session_id: session.session_id,
                    muted,
                    source_job_id: parent_job.id.clone(),
                    reason: "manual_voice_mute".to_string(),
                },
            )]))
        }
        "play_voice_cue" => {
            let room = catalog::room_for_identifier(ctx, Some(&target_room_identifier)).await?;
            let session =
                playback::active_session_for_channel(ctx, &room.guild_id, &room.channel_id)
                    .await?
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "room {} has no active voice session for playback",
                            room.room_id
                        )
                    })?;
            let cue: DiscordVoicePlaybackCue = command.arguments.cue.parse()?;
            Ok(JobDecision::WaitFor(vec![Job::discord_voice_play_audio(
                room.guild_id,
                room.channel_id,
                command.requested_by_user_id.clone(),
                DiscordVoicePlayAudioPayload {
                    session_id: session.session_id,
                    cue,
                    source_job_id: parent_job.id.clone(),
                    reason: "manual_voice_cue".to_string(),
                },
            )]))
        }
        "leave_room" => {
            let job = Job::room_agent_placement(
                guild_id,
                channel_id,
                target_room_identifier,
                RoomAgentPlacementAction::Leave,
                "explicit_request",
                format!(
                    "command:{}:{}:{}",
                    parent_job.id,
                    RoomAgentPlacementAction::Leave.as_str(),
                    parent_job.scope_id
                ),
                Some(pool.manual_override_seconds),
            );
            Ok(JobDecision::WaitFor(vec![job]))
        }
        "join_room" => {
            let room_id = if !target_room_identifier.trim().is_empty() {
                target_room_identifier
            } else {
                channel_id.clone()
            };
            let job = Job::room_agent_placement(
                guild_id,
                channel_id,
                room_id,
                RoomAgentPlacementAction::Join,
                "explicit_request",
                format!(
                    "command:{}:{}:{}",
                    parent_job.id,
                    RoomAgentPlacementAction::Join.as_str(),
                    parent_job.scope_id
                ),
                None,
            );
            Ok(JobDecision::WaitFor(vec![job]))
        }
        "forget_window" => {
            let (start, end) = command.window_times(None);
            let result = transcripts::forget(
                ctx,
                ForgetRequest {
                    window_id: command.arguments.window_id.clone(),
                    guild_id: guild_id.clone(),
                    channel_id: channel_id.clone(),
                    since: if command.arguments.from.trim().is_empty() {
                        isoformat_z(Some(start))
                    } else {
                        command.arguments.from.clone()
                    },
                    to: if command.arguments.to.trim().is_empty() {
                        isoformat_z(Some(end))
                    } else {
                        command.arguments.to.clone()
                    },
                    requested_by_user_id: command.requested_by_user_id.clone(),
                    unpublished_only: command.arguments.unpublished_only.unwrap_or(true),
                },
            )
            .await?;
            Ok(JobDecision::Complete(JobOutput::from_boundary_json(
                &json!({"kind": "forget_window", "job_ids": [], "result": result}),
            )?))
        }
        _ => {
            let job_kind: JobKind = job_kind.parse()?;
            if !job_kind.is_agent_task() {
                anyhow::bail!("unsupported queued job kind: {job_kind}");
            }
            let requested_by_user_id = command.requested_by_user_id.clone();
            let job = agent_sessions::agent_session_start_or_task_job(
                ctx,
                &guild_id,
                &channel_id,
                &requested_by_user_id,
                command,
            )
            .await?;
            Ok(JobDecision::WaitFor(vec![job]))
        }
    }
}

async fn command_scope(ctx: &Ctx, command: &CommandRequest) -> Result<(String, String, String)> {
    let mut guild_id = command.guild_id.trim().to_string();
    let mut channel_id = command.scope_id.trim().to_string();
    let explicit_room_identifier = command.explicit_room_identifier();
    if command.command_kind.requires_explicit_room_target()
        && channel_id.is_empty()
        && explicit_room_identifier.is_empty()
    {
        anyhow::bail!(
            "command {} requires explicit room/channel target",
            command.command_kind.as_str()
        );
    }
    let target_room_identifier = command.target_room_identifier(&channel_id);
    if (guild_id.is_empty() || channel_id.is_empty()) && !command.arguments.window_id.is_empty() {
        let window = ctx.store.get_window(&command.arguments.window_id).await?;
        if guild_id.is_empty() {
            guild_id = string_field(&window, "guild_id");
        }
        if channel_id.is_empty() {
            channel_id = string_field(&window, "voice_channel_id");
        }
    }
    if guild_id.is_empty() || channel_id.is_empty() {
        let room =
            catalog::resolve_room_scope(ctx, &guild_id, Some(&target_room_identifier)).await?;
        if guild_id.is_empty() {
            guild_id = room.guild_id;
        }
        channel_id = room.channel_id;
    }
    if guild_id.is_empty() || channel_id.is_empty() {
        anyhow::bail!("command is missing guild or channel");
    }
    Ok((guild_id, channel_id, target_room_identifier))
}
