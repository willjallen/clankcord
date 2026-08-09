use serde_json::json;

use crate::Result;
use crate::domain::Ctx;
use crate::domain::ingress::discord_slash;
use crate::domain::ingress::discord_text;
use crate::domain::interactions::agent_sessions;
use crate::domain::interactions::confirmations;
use crate::domain::maintenance::execution;
use crate::domain::maintenance::member_sync;
use crate::domain::messaging::text_delivery;
use crate::domain::messaging::typing_indicator;
use crate::domain::rooms::catalog;
use crate::domain::transcripts::publication;
use crate::domain::voice::playback;
use crate::domain::voice::room_placement;
use crate::domain::voice_capture::{segments, wake_activations, wake_probes};
use crate::engine::JobDecision;
use crate::model::job::{
    Job, JobOutput, JobPayload, RoomAgentPlacementAction, RoomAgentPlacementPayload,
    RuntimeControlAction, RuntimeControlPayload,
};
use crate::ports::discord::DiscordApi;
use crate::views::jobs;

pub(crate) async fn execute<A>(runtime: &Ctx, job: &Job, external_api: &A) -> Result<JobDecision>
where
    A: DiscordApi,
{
    match &job.payload {
        JobPayload::DiscordTextSend(payload) => {
            crate::domain::messaging::discord_io::execute_discord_text_send_job(
                runtime,
                payload,
                external_api,
            )
            .await
        }
        JobPayload::DiscordForumThreadCreate(payload) => {
            crate::domain::messaging::discord_io::execute_discord_forum_thread_create_job(
                runtime,
                payload,
                external_api,
            )
            .await
        }
        JobPayload::DiscordForumThreadRename(payload) => {
            crate::domain::messaging::discord_io::execute_discord_forum_thread_rename_job(
                runtime,
                payload,
                external_api,
            )
            .await
        }
        JobPayload::DiscordTypingIndicator(payload) => {
            typing_indicator::execute_discord_typing_indicator_job(
                runtime,
                job,
                payload,
                external_api,
            )
            .await
        }
        JobPayload::DiscordVoiceJoin(payload) => {
            crate::domain::voice::discord_io::execute_discord_voice_join_job(
                runtime,
                payload,
                external_api,
            )
            .await
        }
        JobPayload::DiscordVoiceLeave(payload) => {
            crate::domain::voice::discord_io::execute_discord_voice_leave_job(
                runtime,
                job,
                payload,
                external_api,
            )
            .await
        }
        JobPayload::DiscordVoiceMute(payload) => {
            crate::domain::voice::discord_io::execute_discord_voice_mute_job(
                runtime,
                payload,
                external_api,
            )
            .await
        }
        JobPayload::DiscordVoiceDeafen(payload) => {
            crate::domain::voice::discord_io::execute_discord_voice_deafen_job(
                runtime,
                payload,
                external_api,
            )
            .await
        }
        JobPayload::DiscordVoicePlayAudio(payload) => {
            crate::domain::voice::discord_io::execute_discord_voice_play_audio_job(
                runtime,
                payload,
                external_api,
            )
            .await
        }
        JobPayload::MemberSync(payload) => {
            Ok(JobDecision::Complete(JobOutput::from_boundary_json(
                &member_sync::execute(runtime, payload, external_api).await?,
            )?))
        }
        JobPayload::DiscordVoiceStatusSnapshot(_) => {
            crate::domain::voice::discord_io::execute_discord_voice_status_snapshot_job(
                runtime,
                external_api,
            )
            .await
        }

        JobPayload::RuntimeControl(payload) => runtime_control::prepare(runtime, payload).await,
        JobPayload::RuntimeMaintenance(payload) => {
            execution::execute_runtime_maintenance_job(runtime, job, payload).await
        }
        JobPayload::VoiceStatusSync(_) => {
            execution::execute_voice_status_sync_job(runtime, job).await
        }
        JobPayload::AutomationEvaluation(_) => {
            execution::execute_automation_evaluation_job(runtime, job).await
        }
        JobPayload::AgentSessionRetirement(_) => {
            agent_sessions::execute_agent_session_retirement_job(runtime).await
        }
        JobPayload::StaleWakeProbeSweep(payload) => {
            execution::execute_stale_wake_probe_sweep_job(runtime, payload.max_age_seconds).await
        }
        JobPayload::EphemeralJobGc(payload) => {
            execution::execute_ephemeral_job_gc_job(runtime, payload.batch_limit).await
        }
        JobPayload::WakeActivation(payload) => {
            Ok(JobDecision::Complete(JobOutput::from_boundary_json(
                &wake_activations::execute(runtime, job, payload).await?,
            )?))
        }
        JobPayload::TranscriptionMuxPlan(payload) => {
            Ok(JobDecision::Complete(JobOutput::from_boundary_json(
                &segments::execute_transcription_mux_plan_job(runtime, job, payload).await?,
            )?))
        }
        JobPayload::Command(_) => {
            crate::domain::interactions::commands::execute_command_job(runtime, job).await
        }
        JobPayload::DiscordTextMessage(payload) => {
            discord_text::prepare(runtime, job, payload).await
        }
        JobPayload::DiscordSlashCommand(payload) => {
            discord_slash::prepare(runtime, job, payload).await
        }
        JobPayload::TextDelivery(payload) => {
            text_delivery::execute_text_delivery_job(runtime, job, payload).await
        }
        JobPayload::ConfirmationRequired(_) => {
            confirmations::execute_confirmation_required_job(runtime, job).await
        }
        JobPayload::AgentSessionStart(payload) => {
            agent_sessions::execute_agent_session_start_job(runtime, job, payload).await
        }
        JobPayload::AgentSessionSunset(payload) => {
            agent_sessions::execute_agent_session_sunset_job(runtime, payload).await
        }
        JobPayload::AgentSessionResume(payload) => {
            agent_sessions::execute_agent_session_resume_job(runtime, job, payload).await
        }
        JobPayload::TranscriptPublication(payload) => {
            publication::execute_transcript_publication_job(runtime, job, payload).await
        }
        JobPayload::RoomAgentPlacement(payload) => {
            room_agents::prepare(runtime, job, payload).await
        }
        JobPayload::DiscordVoicePlayback(payload) => {
            playback::execute_voice_playback_job(runtime, job, payload).await
        }
        payload => anyhow::bail!(
            "job payload {} is not handled by async dispatcher",
            payload.kind()
        ),
    }
}

pub(crate) async fn execute_audio_segment(runtime: &Ctx, job: &Job) -> Result<JobOutput> {
    match &job.payload {
        JobPayload::AudioSegment(payload) => Ok(JobOutput::from_boundary_json(
            &segments::execute_segment_job(runtime, job, payload).await?,
        )?),
        payload => anyhow::bail!(
            "job payload {} is not handled by audio executor",
            payload.kind()
        ),
    }
}

pub(crate) async fn execute_transcription_mux(runtime: &Ctx, job: &Job) -> Result<JobOutput> {
    match &job.payload {
        JobPayload::TranscriptionMux(payload) => Ok(JobOutput::from_boundary_json(
            &segments::execute_transcription_mux_job(runtime, job, payload).await?,
        )?),
        payload => anyhow::bail!(
            "job payload {} is not handled by transcription mux executor",
            payload.kind()
        ),
    }
}

pub(crate) async fn execute_wake_probe(runtime: &Ctx, job: &Job) -> Result<JobOutput> {
    match &job.payload {
        JobPayload::WakeProbe(payload) => Ok(JobOutput::from_boundary_json(
            &wake_probes::execute_probe_job(runtime, job, payload).await?,
        )?),
        payload => anyhow::bail!(
            "job payload {} is not handled by wake probe executor",
            payload.kind()
        ),
    }
}

mod runtime_control {
    use super::*;

    pub(super) async fn prepare(
        runtime: &Ctx,
        payload: &RuntimeControlPayload,
    ) -> Result<JobDecision> {
        let output = match payload.action {
            RuntimeControlAction::RetryJob => {
                let target = jobs::retry_job_payload(runtime, &payload.target_job_id).await?;
                JobOutput::from_boundary_json(
                    &json!({"kind": "runtime_control", "action": "retry_job", "target": target}),
                )?
            }
            RuntimeControlAction::ApproveConfirmation => {
                let result = confirmations::approve_confirmation(
                    runtime,
                    &payload.target_job_id,
                    payload.actor_user_id.clone(),
                )
                .await?;
                JobOutput::from_boundary_json(
                    &json!({"kind": "runtime_control", "action": "approve_confirmation", "result": result}),
                )?
            }
            RuntimeControlAction::CancelConfirmation => {
                let result = confirmations::cancel_confirmation(
                    runtime,
                    &payload.target_job_id,
                    payload.actor_user_id.clone(),
                )
                .await?;
                JobOutput::from_boundary_json(
                    &json!({"kind": "runtime_control", "action": "cancel_confirmation", "result": result}),
                )?
            }
        };
        Ok(JobDecision::Complete(output))
    }
}

mod room_agents {
    use super::*;

    pub(super) async fn prepare(
        runtime: &Ctx,
        job: &Job,
        payload: &RoomAgentPlacementPayload,
    ) -> Result<JobDecision> {
        if runtime.store.has_child_jobs(&job.id).await? {
            return room_placement::resume_room_agent_placement_job(runtime, job, payload).await;
        }
        let target_room_identifier = if payload.room_id.trim().is_empty() {
            job.scope_id.as_str()
        } else {
            payload.room_id.as_str()
        };
        match payload.action {
            RoomAgentPlacementAction::Join => {
                let room = if !target_room_identifier.trim().is_empty() {
                    catalog::room_for_identifier(runtime, Some(target_room_identifier)).await?
                } else if !job.guild_id.trim().is_empty() {
                    catalog::resolve_room_scope(runtime, &job.guild_id, None).await?
                } else {
                    catalog::room_for_identifier(runtime, None).await?
                };
                room_placement::plan_join_room_jobs(
                    runtime,
                    room,
                    &job.requested_by_user_id,
                    &payload.reason,
                )
                .await
            }
            RoomAgentPlacementAction::Leave => {
                let pool = runtime.store.runtime_pool_config().await?;
                let cooldown_seconds = payload
                    .cooldown_seconds
                    .unwrap_or(pool.manual_override_seconds);
                room_placement::plan_leave_room_jobs(
                    runtime,
                    Some(target_room_identifier),
                    cooldown_seconds,
                    &job.requested_by_user_id,
                    &job.id,
                    &payload.reason,
                )
                .await
            }
        }
    }
}
