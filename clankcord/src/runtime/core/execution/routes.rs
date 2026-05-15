use serde_json::json;

use crate::Result;
use crate::runtime::core::execution::JobDecision;
use crate::runtime::domain::audio_segments;
use crate::runtime::domain::responses;
use crate::runtime::domain::wake_activations;
use crate::runtime::domain::wake_probes;
use crate::runtime::refinement::run_refinement_job;
use crate::runtime::{
    Job, JobOutput, JobPayload, RoomAgentPlacementAction, RoomAgentPlacementPayload, Runtime,
    RuntimeControlAction, RuntimeControlPayload,
};

pub(crate) fn execute_runtime_async(runtime: &mut Runtime, job: &Job) -> Result<JobDecision> {
    match &job.payload {
        JobPayload::RuntimeControl(payload) => runtime_control::prepare(runtime, payload),
        JobPayload::WakeActivation(payload) => Ok(JobDecision::Complete(
            JobOutput::from_boundary_json(&wake_activations::execute(runtime, job, payload)?)?,
        )),
        JobPayload::Command(_) => commands::prepare(runtime, job),
        JobPayload::RoomAgentPlacement(payload) => room_agents::prepare(runtime, job, payload),
        JobPayload::DiscordVoicePlayback(payload) => {
            runtime.prepare_voice_playback_job(job, payload)
        }
        payload => anyhow::bail!(
            "job payload {} is not handled by async dispatcher",
            payload.kind()
        ),
    }
}

pub(crate) fn execute_response(runtime: &Runtime, job: &Job) -> Result<JobOutput> {
    match &job.payload {
        JobPayload::Response(payload) => responses::execute(runtime, job, payload),
        payload => anyhow::bail!(
            "job payload {} is not handled by response executor",
            payload.kind()
        ),
    }
}

pub(crate) fn execute_refine_transcript(runtime: &Runtime, job: &Job) -> Result<JobOutput> {
    match &job.payload {
        JobPayload::RefineTranscript(_) => refinement::execute(runtime, job),
        payload => anyhow::bail!(
            "job payload {} is not handled by refinement executor",
            payload.kind()
        ),
    }
}

pub(crate) fn execute_audio_segment(runtime: &Runtime, job: &Job) -> Result<JobOutput> {
    match &job.payload {
        JobPayload::AudioSegment(payload) => Ok(JobOutput::from_boundary_json(
            &audio_segments::execute_segment_job(runtime, job, payload)?,
        )?),
        payload => anyhow::bail!(
            "job payload {} is not handled by audio executor",
            payload.kind()
        ),
    }
}

pub(crate) fn execute_wake_probe(runtime: &Runtime, job: &Job) -> Result<JobOutput> {
    match &job.payload {
        JobPayload::WakeProbe(payload) => Ok(JobOutput::from_boundary_json(
            &wake_probes::execute_probe_job(runtime, job, payload)?,
        )?),
        payload => anyhow::bail!(
            "job payload {} is not handled by wake probe executor",
            payload.kind()
        ),
    }
}

mod runtime_control {
    use super::*;

    pub(super) fn prepare(
        runtime: &mut Runtime,
        payload: &RuntimeControlPayload,
    ) -> Result<JobDecision> {
        let output = match payload.action {
            RuntimeControlAction::RetryJob => {
                let target = runtime.retry_job_payload(&payload.target_job_id)?;
                JobOutput::from_boundary_json(
                    &json!({"kind": "runtime_control", "action": "retry_job", "target": target}),
                )?
            }
            RuntimeControlAction::ApproveConfirmation => {
                let result = runtime
                    .approve_confirmation(&payload.target_job_id, payload.actor_user_id.clone())?;
                JobOutput::from_boundary_json(
                    &json!({"kind": "runtime_control", "action": "approve_confirmation", "result": result}),
                )?
            }
            RuntimeControlAction::CancelConfirmation => {
                let result = runtime
                    .cancel_confirmation(&payload.target_job_id, payload.actor_user_id.clone())?;
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

    pub(super) fn prepare(runtime: &mut Runtime, job: &Job) -> Result<JobDecision> {
        runtime.prepare_command_job(job)
    }
}

mod room_agents {
    use super::*;

    pub(super) fn prepare(
        runtime: &mut Runtime,
        job: &Job,
        payload: &RoomAgentPlacementPayload,
    ) -> Result<JobDecision> {
        if runtime.timeline_store.has_child_jobs(&job.id)? {
            return runtime.resume_room_agent_placement_job(job, payload);
        }
        let target_room_identifier = if payload.room_id.trim().is_empty() {
            job.voice_channel_id.as_str()
        } else {
            payload.room_id.as_str()
        };
        match payload.action {
            RoomAgentPlacementAction::Join => {
                let room = if !target_room_identifier.trim().is_empty() {
                    runtime.room_for_identifier(Some(target_room_identifier))?
                } else if !job.guild_id.trim().is_empty() {
                    runtime.resolve_room_scope(&job.guild_id, None)?
                } else {
                    runtime.room_for_identifier(None)?
                };
                runtime.prepare_join_room_jobs(room, &job.requested_by_user_id, &payload.reason)
            }
            RoomAgentPlacementAction::Leave => {
                let cooldown_seconds = payload
                    .cooldown_seconds
                    .unwrap_or(runtime.manual_leave_cooldown_seconds);
                runtime.prepare_leave_room_jobs(
                    Some(target_room_identifier),
                    cooldown_seconds,
                    &job.requested_by_user_id,
                    &job.id,
                )
            }
        }
    }
}

mod refinement {
    use super::*;

    pub(super) fn execute(runtime: &Runtime, job: &Job) -> Result<JobOutput> {
        match run_refinement_job(&runtime.timeline_store, &job.id) {
            Ok(job) => Ok(JobOutput::from_boundary_json(
                &json!({"dispatched": true, "job": job}),
            )?),
            Err(error) => {
                crate::runtime::log(&format!("refinement job failed {}: {error}", job.id));
                Ok(JobOutput::from_boundary_json(
                    &json!({"dispatched": false, "jobId": job.id, "error": error.to_string()}),
                )?)
            }
        }
    }
}
