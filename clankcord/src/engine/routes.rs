use serde_json::{Value, json};

use crate::Result;
use crate::domain::Ctx;
use crate::domain::ingress::discord_slash;
use crate::domain::ingress::discord_text;
use crate::domain::interactions::agent_sessions;
use crate::domain::interactions::confirmations;
use crate::domain::interactions::thread_titles;
use crate::domain::maintenance::execution;
use crate::domain::maintenance::member_sync;
use crate::domain::messaging::text_delivery;
use crate::domain::messaging::typing_indicator;
use crate::domain::rooms::catalog;
use crate::domain::transcription::execution as transcription_execution;
use crate::domain::transcripts::publication;
use crate::domain::voice::capture::{segments, wake_activations, wake_probes};
use crate::domain::voice::playback;
use crate::domain::voice::room_placement;
use crate::engine::JobDecision;
use crate::model::job::{
    Job, JobOutput, JobPayload, JobState, RoomAgentPlacementAction, RoomAgentPlacementPayload,
    RuntimeControlAction, RuntimeControlPayload,
};
use crate::ports::discord::DiscordApi;

/// Where a claimed job's execution goes and how its result is finalized.
/// Produced by [`route`] — the one exhaustive payload match.
pub(crate) enum Routed {
    /// Decision-based execution: apply the decision, fail on error.
    Decision(Result<JobDecision>),
    /// Plain output execution: complete on success, fail on error.
    Output(Result<JobOutput>),
    /// STT-backed execution: complete on success; provider failures are
    /// classified and retryable ones requeue with backoff.
    SttOutput(Result<JobOutput>),
    /// The agent task runs its own phase machine and finalizes itself.
    AgentTask,
}

fn completed(value: Value) -> Result<JobDecision> {
    Ok(JobDecision::Complete(JobOutput::from_boundary_json(
        &value,
    )?))
}

pub(crate) async fn route<A>(runtime: &Ctx, job: &Job, external_api: &A) -> Routed
where
    A: DiscordApi,
{
    match &job.payload {
        JobPayload::DiscordTextSend(payload) => Routed::Decision(
            crate::domain::messaging::discord_io::execute_discord_text_send_job(
                runtime,
                payload,
                external_api,
            )
            .await,
        ),
        JobPayload::DiscordForumThreadCreate(payload) => Routed::Decision(
            crate::domain::messaging::discord_io::execute_discord_forum_thread_create_job(
                runtime,
                payload,
                external_api,
            )
            .await,
        ),
        JobPayload::DiscordForumThreadRename(payload) => Routed::Decision(
            crate::domain::messaging::discord_io::execute_discord_forum_thread_rename_job(
                runtime,
                payload,
                external_api,
            )
            .await,
        ),
        JobPayload::DiscordTypingIndicator(payload) => Routed::Decision(
            typing_indicator::execute_discord_typing_indicator_job(
                runtime,
                job,
                payload,
                external_api,
            )
            .await,
        ),
        JobPayload::DiscordVoiceJoin(payload) => Routed::Decision(
            crate::domain::voice::discord_io::execute_discord_voice_join_job(
                runtime,
                payload,
                external_api,
            )
            .await,
        ),
        JobPayload::DiscordVoiceLeave(payload) => Routed::Decision(
            crate::domain::voice::discord_io::execute_discord_voice_leave_job(
                runtime,
                job,
                payload,
                external_api,
            )
            .await,
        ),
        JobPayload::DiscordVoiceMute(payload) => Routed::Decision(
            crate::domain::voice::discord_io::execute_discord_voice_mute_job(
                runtime,
                payload,
                external_api,
            )
            .await,
        ),
        JobPayload::DiscordVoiceDeafen(payload) => Routed::Decision(
            crate::domain::voice::discord_io::execute_discord_voice_deafen_job(
                runtime,
                payload,
                external_api,
            )
            .await,
        ),
        JobPayload::DiscordVoicePlayAudio(payload) => Routed::Decision(
            crate::domain::voice::discord_io::execute_discord_voice_play_audio_job(
                runtime,
                payload,
                external_api,
            )
            .await,
        ),
        JobPayload::MemberSync(payload) => Routed::Decision(
            member_sync::execute(runtime, payload, external_api)
                .await
                .and_then(completed),
        ),
        JobPayload::DiscordVoiceStatusSnapshot(_) => Routed::Decision(
            crate::domain::voice::discord_io::execute_discord_voice_status_snapshot_job(
                runtime,
                external_api,
            )
            .await,
        ),

        JobPayload::RuntimeControl(payload) => {
            Routed::Decision(runtime_control::prepare(runtime, payload).await)
        }
        JobPayload::RuntimeMaintenance(payload) => Routed::Decision(
            execution::execute_runtime_maintenance_job(runtime, job, payload).await,
        ),
        JobPayload::VoiceStatusSync(_) => {
            Routed::Decision(execution::execute_voice_status_sync_job(runtime, job).await)
        }
        JobPayload::AutomationEvaluation(_) => {
            Routed::Decision(execution::execute_automation_evaluation_job(runtime, job).await)
        }
        JobPayload::AgentSessionRetirement(_) => {
            Routed::Decision(agent_sessions::execute_agent_session_retirement_job(runtime).await)
        }
        JobPayload::StaleWakeProbeSweep(payload) => Routed::Decision(
            execution::execute_stale_wake_probe_sweep_job(runtime, payload.max_age_seconds).await,
        ),
        JobPayload::EphemeralJobGc(payload) => Routed::Decision(
            execution::execute_ephemeral_job_gc_job(runtime, payload.batch_limit).await,
        ),
        JobPayload::WakeActivation(payload) => Routed::Decision(
            wake_activations::execute(runtime, job, payload)
                .await
                .and_then(completed),
        ),
        JobPayload::TranscriptionMuxPlan(payload) => Routed::Decision(
            transcription_execution::execute_transcription_mux_plan_job(runtime, job, payload)
                .await
                .and_then(completed),
        ),
        JobPayload::Command(_) => Routed::Decision(
            crate::domain::interactions::commands::execute_command_job(runtime, job).await,
        ),
        JobPayload::DiscordTextMessage(payload) => {
            Routed::Decision(discord_text::prepare(runtime, job, payload).await)
        }
        JobPayload::DiscordSlashCommand(payload) => {
            Routed::Decision(discord_slash::prepare(runtime, job, payload).await)
        }
        JobPayload::TextDelivery(payload) => {
            Routed::Decision(text_delivery::execute_text_delivery_job(runtime, job, payload).await)
        }
        JobPayload::ConfirmationRequired(_) => {
            Routed::Decision(confirmations::execute_confirmation_required_job(runtime, job).await)
        }
        JobPayload::AgentSessionStart(payload) => Routed::Decision(
            agent_sessions::execute_agent_session_start_job(runtime, job, payload).await,
        ),
        JobPayload::AgentSessionSunset(payload) => Routed::Decision(
            agent_sessions::execute_agent_session_sunset_job(runtime, payload).await,
        ),
        JobPayload::AgentSessionResume(payload) => Routed::Decision(
            agent_sessions::execute_agent_session_resume_job(runtime, job, payload).await,
        ),
        JobPayload::TranscriptPublication(payload) => Routed::Decision(
            publication::execute_transcript_publication_job(runtime, job, payload).await,
        ),
        JobPayload::RoomAgentPlacement(payload) => {
            Routed::Decision(room_agents::prepare(runtime, job, payload).await)
        }
        JobPayload::DiscordVoicePlayback(payload) => {
            Routed::Decision(playback::execute_voice_playback_job(runtime, job, payload).await)
        }
        JobPayload::AudioSegment(payload) => Routed::SttOutput(
            match segments::execute_segment_job(runtime, job, payload).await {
                Ok(value) => JobOutput::from_boundary_json(&value),
                Err(error) => Err(error),
            },
        ),
        JobPayload::TranscriptionMux(payload) => Routed::SttOutput(
            match transcription_execution::execute_transcription_mux_job(runtime, job, payload)
                .await
            {
                Ok(value) => JobOutput::from_boundary_json(&value),
                Err(error) => Err(error),
            },
        ),
        JobPayload::WakeProbe(payload) => Routed::Output(
            match wake_probes::execute_probe_job(runtime, job, payload).await {
                Ok(value) => JobOutput::from_boundary_json(&value),
                Err(error) => Err(error),
            },
        ),
        JobPayload::AgentTask(_) => Routed::AgentTask,
        JobPayload::AgentThreadTitleRefresh(payload) => Routed::Decision(
            thread_titles::execute_agent_thread_title_refresh_job(runtime, job, payload).await,
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
                let target = retry_job(runtime, &payload.target_job_id).await?;
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

/// Requeues a terminal job for another run: state returns to queued, the
/// recorded error clears, and agent-task retry accounting resets.
async fn retry_job(runtime: &Ctx, job_id: &str) -> Result<Value> {
    let mut job = runtime.store.get_job(job_id).await?;
    job.set_state(JobState::Queued);
    job.metadata.error.clear();
    job.metadata.reset_agent_task_retry();
    runtime.store.update_job(&job).await?;
    Ok(job.to_value())
}
