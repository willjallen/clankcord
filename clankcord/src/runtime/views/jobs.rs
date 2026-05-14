use std::str::FromStr;

use serde_json::{Value, json};

use crate::Result;
use crate::config::{non_empty, string_field};
use crate::runtime::{Job, JobState};

use crate::runtime::Runtime;
use crate::runtime::util::{first_non_empty, preview};

#[derive(Debug, Clone, Default)]
pub struct JobsRequest {
    pub guild_id: String,
    pub state: String,
}

impl Runtime {
    pub fn jobs(&self, request: JobsRequest) -> Result<Value> {
        let state_filter = if request.state.is_empty() {
            None
        } else {
            Some(JobState::from_str(&request.state)?)
        };
        let jobs = self.timeline_store.list_jobs(
            if request.guild_id.is_empty() {
                None
            } else {
                Some(&request.guild_id)
            },
            state_filter,
        )?;
        let payloads = jobs.iter().map(Job::to_value).collect::<Vec<_>>();
        Ok(json!({"jobs": payloads, "count": jobs.len()}))
    }

    pub fn get_job_payload(&self, job_id: &str) -> Result<Value> {
        Ok(self.timeline_store.get_job(job_id)?.to_value())
    }

    pub fn public_job_view(job: &Job) -> Value {
        let state = job.state.as_str().to_string();
        json!({
            "job_id": job.id.clone(),
            "kind": job.kind.as_str(),
            "state": state.clone(),
            "guild_id": job.guild_id.clone(),
            "voice_channel_id": job.voice_channel_id.clone(),
            "requested_by_user_id": job.requested_by_user_id.clone(),
            "command_kind": job.command_kind(),
            "created_at": job.created_at.clone(),
            "updated_at": job.updated_at.clone(),
            "started_at": job.started_at.clone().unwrap_or_default(),
            "completed_at": job.completed_at.clone().unwrap_or_default(),
            "parent_job_id": job.parent_job_id.clone().unwrap_or_default(),
            "root_job_id": job.root_job_id.clone(),
            "lineage_depth": job.lineage_depth,
            "cancellable": job.state.is_cancellable(),
            "cancel_requested": job.cancel_requested(),
        })
    }

    pub fn public_interaction_job_context(job: &Job) -> Value {
        let state = job.state.as_str().to_string();
        let request = job
            .command()
            .map(|command| command.arguments.request_text())
            .unwrap_or_default();
        let response_preview = first_non_empty([
            job.string_field("agent_task_dispatch_stdout_preview"),
            job.string_field("response_text"),
        ]);
        json!({
            "job_id": job.id.clone(),
            "kind": job.kind.as_str(),
            "state": state.clone(),
            "guild_id": job.guild_id.clone(),
            "voice_channel_id": job.voice_channel_id.clone(),
            "requested_by_user_id": job.requested_by_user_id.clone(),
            "command_kind": job.command_kind(),
            "request": preview(&request, 1000),
            "response_preview": preview(&response_preview, 1200),
            "created_at": job.created_at.clone(),
            "updated_at": job.updated_at.clone(),
            "started_at": job.started_at.clone().unwrap_or_default(),
            "completed_at": job.completed_at.clone().unwrap_or_default(),
            "parent_job_id": job.parent_job_id.clone().unwrap_or_default(),
            "root_job_id": job.root_job_id.clone(),
            "lineage_depth": job.lineage_depth,
            "cancellable": job.state.is_cancellable(),
            "cancel_requested": job.cancel_requested(),
        })
    }

    pub fn cancellable_jobs_for_channel(
        &self,
        guild_id: &str,
        channel_id: &str,
        limit: usize,
    ) -> Result<Vec<Value>> {
        let mut jobs = self
            .timeline_store
            .list_jobs(Some(guild_id), None)?
            .into_iter()
            .filter(|job| job.voice_channel_id == channel_id && job.state.is_cancellable())
            .collect::<Vec<_>>();
        jobs.sort_by(|left, right| {
            let left_time = first_non_empty([left.updated_at.clone(), left.created_at.clone()]);
            let right_time = first_non_empty([right.updated_at.clone(), right.created_at.clone()]);
            right_time.cmp(&left_time)
        });
        Ok(jobs
            .into_iter()
            .take(limit)
            .map(|job| Self::public_job_view(&job))
            .collect())
    }

    pub fn recent_agent_task_jobs_for_channel(
        &self,
        guild_id: &str,
        channel_id: &str,
        requester_user_id: &str,
        limit: usize,
    ) -> Result<Vec<Value>> {
        let requester = requester_user_id.trim();
        let mut jobs = self
            .timeline_store
            .list_jobs(Some(guild_id), None)?
            .into_iter()
            .filter(|job| {
                job.voice_channel_id == channel_id
                    && job.kind.is_agent_task()
                    && matches!(
                        job.state,
                        JobState::Queued
                            | JobState::Running
                            | JobState::Waiting
                            | JobState::CancelRequested
                            | JobState::Complete
                            | JobState::FailedTimeout
                            | JobState::AgentDispatchFailed
                    )
            })
            .collect::<Vec<_>>();
        jobs.sort_by(|left, right| {
            let left_preferred = requester.is_empty() || left.requested_by_user_id == requester;
            let right_preferred = requester.is_empty() || right.requested_by_user_id == requester;
            let left_time = first_non_empty([left.updated_at.clone(), left.created_at.clone()]);
            let right_time = first_non_empty([right.updated_at.clone(), right.created_at.clone()]);
            right_preferred
                .cmp(&left_preferred)
                .then_with(|| right_time.cmp(&left_time))
        });
        Ok(jobs
            .into_iter()
            .take(limit)
            .map(|job| Self::public_interaction_job_context(&job))
            .collect())
    }

    pub fn command_interaction_context(
        &self,
        interaction: &Value,
        guild_id: &str,
        channel_id: &str,
    ) -> Result<Value> {
        let active_jobs = self.cancellable_jobs_for_channel(guild_id, channel_id, 10)?;
        let requester_id = string_field(interaction, "current_requester_user_id");
        let recent_jobs =
            self.recent_agent_task_jobs_for_channel(guild_id, channel_id, &requester_id, 5)?;
        let cancellable_job_ids = active_jobs
            .iter()
            .filter(|job| job.get("cancellable").and_then(Value::as_bool) == Some(true))
            .map(|job| string_field(job, "job_id"))
            .filter(|job_id| !job_id.is_empty())
            .collect::<Vec<_>>();
        let recent_job_ids = recent_jobs
            .iter()
            .map(|job| string_field(job, "job_id"))
            .filter(|job_id| !job_id.is_empty())
            .collect::<Vec<_>>();
        Ok(json!({
            "interaction_id": string_field(interaction, "interaction_id"),
            "state": string_field(interaction, "state"),
            "guild_id": non_empty(string_field(interaction, "guild_id"), guild_id.to_string()),
            "voice_channel_id": non_empty(string_field(interaction, "voice_channel_id"), channel_id.to_string()),
            "created_at": string_field(interaction, "created_at"),
            "updated_at": string_field(interaction, "updated_at"),
            "expires_at": string_field(interaction, "expires_at"),
            "current_requester_user_id": requester_id,
            "turn_history": interaction.get("turn_history").filter(|value| value.is_array()).cloned().unwrap_or_else(|| json!([])),
            "active_jobs": active_jobs,
            "cancellable_job_ids": cancellable_job_ids,
            "recent_jobs": recent_jobs,
            "recent_job_ids": recent_job_ids,
        }))
    }

    pub fn retry_job_payload(&self, job_id: &str) -> Result<Value> {
        let mut job = self.timeline_store.get_job(job_id)?;
        job.set_state(JobState::Queued);
        job.metadata.error.clear();
        job.metadata.reset_agent_task_retry();
        self.timeline_store.update_job(&job)?;
        Ok(job.to_value())
    }
}
