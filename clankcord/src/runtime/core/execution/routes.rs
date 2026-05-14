use serde_json::{Value, json};

use crate::Result;
use crate::runtime::core::execution::RuntimeEffects;
use crate::runtime::domain::audio_segments;
use crate::runtime::domain::responses;
use crate::runtime::domain::wake_activations;
use crate::runtime::refinement::run_refinement_job;
use crate::runtime::{
    Job, JobPayload, RoomAgentPlacementAction, RoomAgentPlacementPayload, Runtime,
    RuntimeControlAction, RuntimeControlPayload,
};

pub(crate) async fn execute_async(
    runtime: &mut Runtime,
    job: &Job,
    effects: Option<&dyn RuntimeEffects>,
) -> Result<Value> {
    match &job.payload {
        JobPayload::RuntimeControl(payload) => runtime_control::execute(runtime, payload).await,
        JobPayload::WakeActivation(payload) => wake_activations::execute(runtime, job, payload),
        JobPayload::Command(_) => commands::execute(runtime, job, effects).await,
        JobPayload::RoomAgentPlacement(payload) => {
            room_agents::execute(runtime, job, payload, effects).await
        }
        payload => anyhow::bail!(
            "job payload {} is not handled by async dispatcher",
            payload.kind()
        ),
    }
}

pub(crate) fn execute_response(runtime: &Runtime, job: &Job) -> Result<Value> {
    match &job.payload {
        JobPayload::Response(payload) => responses::execute(runtime, job, payload),
        payload => anyhow::bail!(
            "job payload {} is not handled by response executor",
            payload.kind()
        ),
    }
}

pub(crate) fn execute_refine_transcript(runtime: &Runtime, job: &Job) -> Result<Value> {
    match &job.payload {
        JobPayload::RefineTranscript(_) => refinement::execute(runtime, job),
        payload => anyhow::bail!(
            "job payload {} is not handled by refinement executor",
            payload.kind()
        ),
    }
}

pub(crate) fn execute_audio_segment(runtime: &Runtime, job: &Job) -> Result<Value> {
    match &job.payload {
        JobPayload::AudioSegment(payload) => {
            audio_segments::execute_segment_job(runtime, job, payload)
        }
        payload => anyhow::bail!(
            "job payload {} is not handled by audio executor",
            payload.kind()
        ),
    }
}

mod runtime_control {
    use super::*;

    pub(super) async fn execute(
        runtime: &mut Runtime,
        payload: &RuntimeControlPayload,
    ) -> Result<Value> {
        match payload.action {
            RuntimeControlAction::RetryJob => {
                let target = runtime.retry_job_payload(&payload.target_job_id)?;
                Ok(json!({"kind": "runtime_control", "action": "retry_job", "target": target}))
            }
            RuntimeControlAction::ApproveConfirmation => {
                let result = runtime
                    .approve_confirmation(&payload.target_job_id, payload.actor_user_id.clone())
                    .await?;
                Ok(
                    json!({"kind": "runtime_control", "action": "approve_confirmation", "result": result}),
                )
            }
            RuntimeControlAction::CancelConfirmation => {
                let result = runtime
                    .cancel_confirmation(&payload.target_job_id, payload.actor_user_id.clone())?;
                Ok(
                    json!({"kind": "runtime_control", "action": "cancel_confirmation", "result": result}),
                )
            }
        }
    }
}

mod commands {
    use super::*;

    pub(super) async fn execute(
        runtime: &mut Runtime,
        job: &Job,
        effects: Option<&dyn RuntimeEffects>,
    ) -> Result<Value> {
        runtime.execute_command_job(job, effects).await
    }
}

mod room_agents {
    use super::*;

    pub(super) async fn execute(
        runtime: &mut Runtime,
        job: &Job,
        payload: &RoomAgentPlacementPayload,
        effects: Option<&dyn RuntimeEffects>,
    ) -> Result<Value> {
        let target_room_identifier = if payload.room_id.trim().is_empty() {
            job.voice_channel_id.as_str()
        } else {
            payload.room_id.as_str()
        };
        match payload.action {
            RoomAgentPlacementAction::Join => {
                let result = if let Some(effects) = effects {
                    let room = if !target_room_identifier.trim().is_empty() {
                        runtime.room_for_identifier(Some(target_room_identifier))?
                    } else if !job.guild_id.trim().is_empty() {
                        runtime.resolve_room_scope(&job.guild_id, None)?
                    } else {
                        runtime.room_for_identifier(None)?
                    };
                    runtime
                        .assign_room_with_effect(
                            room,
                            &job.requested_by_user_id,
                            &payload.reason,
                            effects,
                        )
                        .await?
                } else {
                    runtime
                        .assign_room(
                            Some(target_room_identifier),
                            Some(&job.guild_id),
                            Some(&job.requested_by_user_id),
                            Some(&payload.reason),
                        )
                        .await?
                };
                Ok(json!({"kind": "room_agent_placement", "action": "join", "result": result}))
            }
            RoomAgentPlacementAction::Leave => {
                let cooldown_seconds = payload
                    .cooldown_seconds
                    .unwrap_or(runtime.manual_leave_cooldown_seconds);
                let result = if let Some(effects) = effects {
                    runtime
                        .leave_room_with_effect(
                            Some(target_room_identifier),
                            cooldown_seconds,
                            &job.requested_by_user_id,
                            Some(job),
                            effects,
                        )
                        .await?
                } else {
                    runtime
                        .leave_room(
                            Some(target_room_identifier),
                            Some(cooldown_seconds),
                            Some(&job.requested_by_user_id),
                        )
                        .await?
                };
                Ok(json!({"kind": "room_agent_placement", "action": "leave", "result": result}))
            }
        }
    }
}

mod refinement {
    use super::*;

    pub(super) fn execute(runtime: &Runtime, job: &Job) -> Result<Value> {
        match run_refinement_job(&runtime.timeline_store, &job.id) {
            Ok(job) => Ok(json!({"dispatched": true, "job": job})),
            Err(error) => {
                crate::runtime::log(&format!("refinement job failed {}: {error}", job.id));
                Ok(json!({"dispatched": false, "jobId": job.id, "error": error.to_string()}))
            }
        }
    }
}
