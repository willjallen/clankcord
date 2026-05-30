use serde_json::json;

use crate::Result;
use crate::runtime::core::execution::JobDecision;
use crate::runtime::domain::external::RuntimeExternalApi;
use crate::runtime::domain::ingress::discord_slash;
use crate::runtime::domain::ingress::discord_text;
use crate::runtime::domain::voice_capture::{segments, wake_activations, wake_probes};
use crate::runtime::{
    Job, JobOutput, JobPayload, RoomAgentPlacementAction, RoomAgentPlacementPayload, Runtime,
    RuntimeControlAction, RuntimeControlPayload,
};

pub(crate) async fn execute_runtime_async(runtime: &mut Runtime, job: &Job) -> Result<JobDecision> {
    match &job.payload {
        JobPayload::RuntimeControl(payload) => runtime_control::prepare(runtime, payload).await,
        JobPayload::RuntimeMaintenance(payload) => {
            runtime.prepare_runtime_maintenance_job(job, payload).await
        }
        JobPayload::VoiceStatusSync(_) => runtime.prepare_voice_status_sync_job(job).await,
        JobPayload::AutomationEvaluation(_) => runtime.prepare_automation_evaluation_job(job).await,
        JobPayload::AgentSessionRetirement(_) => {
            runtime.prepare_agent_session_retirement_job().await
        }
        JobPayload::StaleWakeProbeSweep(payload) => {
            runtime
                .prepare_stale_wake_probe_sweep_job(payload.max_age_seconds)
                .await
        }
        JobPayload::StaleRunningJobSweep(payload) => {
            runtime
                .prepare_stale_running_job_sweep_job(payload.timeout_minutes)
                .await
        }
        JobPayload::EphemeralJobGc(payload) => {
            runtime
                .prepare_ephemeral_job_gc_job(payload.batch_limit)
                .await
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
        JobPayload::Command(_) => commands::prepare(runtime, job).await,
        JobPayload::DiscordTextMessage(payload) => {
            discord_text::prepare(runtime, job, payload).await
        }
        JobPayload::DiscordSlashCommand(payload) => {
            discord_slash::prepare(runtime, job, payload).await
        }
        JobPayload::TextDelivery(payload) => runtime.prepare_text_delivery_job(job, payload).await,
        JobPayload::ConfirmationRequired(_) => runtime.prepare_confirmation_required_job(job).await,
        JobPayload::AgentSessionStart(payload) => {
            runtime.prepare_agent_session_start_job(job, payload).await
        }
        JobPayload::AgentSessionSunset(payload) => {
            runtime.prepare_agent_session_sunset_job(payload).await
        }
        JobPayload::AgentSessionResume(payload) => {
            runtime.prepare_agent_session_resume_job(job, payload).await
        }
        JobPayload::TranscriptPublication(payload) => {
            runtime
                .prepare_transcript_publication_job(job, payload)
                .await
        }
        JobPayload::RoomAgentPlacement(payload) => {
            room_agents::prepare(runtime, job, payload).await
        }
        JobPayload::DiscordVoicePlayback(payload) => {
            runtime.prepare_voice_playback_job(job, payload).await
        }
        payload => anyhow::bail!(
            "job payload {} is not handled by async dispatcher",
            payload.kind()
        ),
    }
}

pub(crate) async fn execute_runtime_async_with_external_api<A>(
    runtime: &mut Runtime,
    job: &Job,
    external_api: &A,
) -> Result<JobDecision>
where
    A: RuntimeExternalApi,
{
    match &job.payload {
        JobPayload::DiscordTextSend(payload) => {
            runtime
                .execute_discord_text_send_job(payload, external_api)
                .await
        }
        JobPayload::DiscordForumThreadCreate(payload) => {
            runtime
                .execute_discord_forum_thread_create_job(payload, external_api)
                .await
        }
        JobPayload::DiscordForumThreadRename(payload) => {
            runtime
                .execute_discord_forum_thread_rename_job(payload, external_api)
                .await
        }
        JobPayload::DiscordTypingIndicator(payload) => {
            runtime
                .execute_discord_typing_indicator_job(job, payload, external_api)
                .await
        }
        JobPayload::DiscordVoiceJoin(payload) => {
            runtime
                .execute_discord_voice_join_job(payload, external_api)
                .await
        }
        JobPayload::DiscordVoiceLeave(payload) => {
            runtime
                .execute_discord_voice_leave_job(job, payload, external_api)
                .await
        }
        JobPayload::DiscordVoiceMute(payload) => {
            runtime
                .execute_discord_voice_mute_job(payload, external_api)
                .await
        }
        JobPayload::DiscordVoiceDeafen(payload) => {
            runtime
                .execute_discord_voice_deafen_job(payload, external_api)
                .await
        }
        JobPayload::DiscordVoicePlayAudio(payload) => {
            runtime
                .execute_discord_voice_play_audio_job(payload, external_api)
                .await
        }
        JobPayload::DiscordVoiceStatusSnapshot(_) => {
            runtime
                .execute_discord_voice_status_snapshot_job(external_api)
                .await
        }
        _ => execute_runtime_async(runtime, job).await,
    }
}

pub(crate) async fn execute_audio_segment(runtime: &Runtime, job: &Job) -> Result<JobOutput> {
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

pub(crate) async fn execute_transcription_mux(runtime: &Runtime, job: &Job) -> Result<JobOutput> {
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

pub(crate) async fn execute_wake_probe(runtime: &Runtime, job: &Job) -> Result<JobOutput> {
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
        runtime: &mut Runtime,
        payload: &RuntimeControlPayload,
    ) -> Result<JobDecision> {
        let output = match payload.action {
            RuntimeControlAction::RetryJob => {
                let target = runtime.retry_job_payload(&payload.target_job_id).await?;
                JobOutput::from_boundary_json(
                    &json!({"kind": "runtime_control", "action": "retry_job", "target": target}),
                )?
            }
            RuntimeControlAction::ApproveConfirmation => {
                let result = runtime
                    .approve_confirmation(&payload.target_job_id, payload.actor_user_id.clone())
                    .await?;
                JobOutput::from_boundary_json(
                    &json!({"kind": "runtime_control", "action": "approve_confirmation", "result": result}),
                )?
            }
            RuntimeControlAction::CancelConfirmation => {
                let result = runtime
                    .cancel_confirmation(&payload.target_job_id, payload.actor_user_id.clone())
                    .await?;
                JobOutput::from_boundary_json(
                    &json!({"kind": "runtime_control", "action": "cancel_confirmation", "result": result}),
                )?
            }
        };
        Ok(JobDecision::Complete(output))
    }
}

mod commands {
    use super::*;

    pub(super) async fn prepare(runtime: &mut Runtime, job: &Job) -> Result<JobDecision> {
        runtime.prepare_command_job(job).await
    }
}

mod room_agents {
    use super::*;

    pub(super) async fn prepare(
        runtime: &mut Runtime,
        job: &Job,
        payload: &RoomAgentPlacementPayload,
    ) -> Result<JobDecision> {
        if runtime.timeline_store.has_child_jobs(&job.id).await? {
            return runtime.resume_room_agent_placement_job(job, payload).await;
        }
        let target_room_identifier = if payload.room_id.trim().is_empty() {
            job.scope_id.as_str()
        } else {
            payload.room_id.as_str()
        };
        match payload.action {
            RoomAgentPlacementAction::Join => {
                let room = if !target_room_identifier.trim().is_empty() {
                    runtime
                        .room_for_identifier(Some(target_room_identifier))
                        .await?
                } else if !job.guild_id.trim().is_empty() {
                    runtime.resolve_room_scope(&job.guild_id, None).await?
                } else {
                    runtime.room_for_identifier(None).await?
                };
                runtime
                    .prepare_join_room_jobs(room, &job.requested_by_user_id, &payload.reason)
                    .await
            }
            RoomAgentPlacementAction::Leave => {
                let pool = runtime.timeline_store.runtime_pool_config().await?;
                let cooldown_seconds = payload
                    .cooldown_seconds
                    .unwrap_or(pool.manual_override_seconds);
                runtime
                    .prepare_leave_room_jobs(
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
