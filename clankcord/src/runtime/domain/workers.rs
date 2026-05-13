use serde_json::{Value, json};

use crate::Result;
use crate::adapters::discord::api::send_message;
use crate::config::{MESSAGE_CHUNK_LIMIT, non_empty, split_message_chunks, string_field};
use crate::runtime::agents::{AgentInfrastructureError, WorkerAgentRequest};
use crate::runtime::jobs::{DiscordPostMetadata, DiscordPostedMessageMetadata, WorkerJobMetadata};
use crate::runtime::timeline::isoformat_z;
use crate::runtime::util::{job_cancel_requested, log};
use crate::runtime::{Job, JobKind, JobState, Runtime};

impl Runtime {
    pub fn dispatch_next_due_worker_job(&self) -> Result<Value> {
        let Some(job) = self.next_queued_job(JobKind::VoiceAgentTask)? else {
            return Ok(json!({"dispatched": false, "reason": "no queued worker jobs"}));
        };
        let job_id = job.id.clone();
        let attempts = job
            .metadata
            .worker()
            .map(|worker| worker.dispatch_attempts)
            .unwrap_or(0);
        if attempts >= 3 {
            let mut failed = job.clone();
            failed.set_state(JobState::WorkerDispatchFailed);
            self.timeline_store.update_job(&failed)?;
            return Ok(
                json!({"dispatched": false, "job": failed.to_value(), "reason": "worker dispatch attempts exhausted"}),
            );
        }

        let mut running = job.clone();
        running.mark_running();
        self.timeline_store.update_job(&running)?;

        match self.dispatch_worker_agent_job(&running) {
            Ok(dispatch_result) => {
                match self.complete_worker_job(job_id.clone(), running.clone(), dispatch_result) {
                    Ok(value) => Ok(value),
                    Err(error) => self.fail_worker_job(job_id, running, attempts, error),
                }
            }
            Err(error) => self.fail_worker_job(job_id, running, attempts, error),
        }
    }

    fn dispatch_worker_agent_job(&self, job: &Job) -> Result<WorkerJobMetadata> {
        let latest = self.timeline_store.get_job(&job.id)?;
        let job_dir = self
            .timeline_store
            .channel_dir(&latest.guild_id, &latest.voice_channel_id)
            .join("jobs");
        self.agents.dispatch_worker_job(WorkerAgentRequest {
            job: latest,
            job_dir,
        })
    }

    fn complete_worker_job(
        &self,
        job_id: String,
        fallback_job: Job,
        dispatch_result: WorkerJobMetadata,
    ) -> Result<Value> {
        let mut latest = self
            .timeline_store
            .get_job(&job_id)
            .unwrap_or_else(|_| fallback_job.clone());
        latest.metadata.set_worker(dispatch_result);
        if job_cancel_requested(&latest) {
            let cancelled_at = non_empty(
                latest.cancelled_at.clone().unwrap_or_default(),
                isoformat_z(None),
            );
            latest.mark_cancelled();
            latest.cancelled_at = Some(cancelled_at);
            latest.completed_at = Some(isoformat_z(None));
            latest.metadata.worker_mut().result_suppressed = true;
            self.timeline_store.update_job(&latest)?;
            self.timeline_store.append_event(
                &latest.guild_id,
                &latest.voice_channel_id,
                json!({
                    "event_kind": "worker_result_suppressed",
                    "kind": "worker_result_suppressed",
                    "job_id": job_id,
                    "job_kind": latest.kind.as_str(),
                    "reason": "job was cancelled before the worker result was posted",
                }),
            )?;
            return Ok(json!({"dispatched": true, "job": latest.to_value(), "cancelled": true}));
        }
        let response_text = latest
            .metadata
            .worker()
            .map(|worker| worker.response_text.clone())
            .unwrap_or_default();
        if !response_text.trim().is_empty() {
            let post_result = self.post_worker_job_result(&latest, &response_text)?;
            latest.metadata.worker_mut().discord_post = Some(post_result);
        }
        latest.mark_complete();
        self.timeline_store.update_job(&latest)?;
        Ok(json!({"dispatched": true, "job": latest.to_value()}))
    }

    fn fail_worker_job(
        &self,
        job_id: String,
        fallback_job: Job,
        attempts: i64,
        error: anyhow::Error,
    ) -> Result<Value> {
        let infrastructure_error = error.downcast_ref::<AgentInfrastructureError>();
        let is_infrastructure_error = infrastructure_error.is_some();
        let error_text = error.to_string();
        let mut latest = self
            .timeline_store
            .get_job(&job_id)
            .unwrap_or_else(|_| fallback_job.clone());
        if job_cancel_requested(&latest) {
            let cancelled_at = non_empty(
                latest.cancelled_at.clone().unwrap_or_default(),
                isoformat_z(None),
            );
            latest.mark_cancelled();
            latest.cancelled_at = Some(cancelled_at);
            latest.metadata.worker_mut().dispatch_error_after_cancel = error_text;
            self.timeline_store.update_job(&latest)?;
            return Ok(json!({"dispatched": false, "job": latest.to_value(), "cancelled": true}));
        }
        if let Some(preflight) = infrastructure_error.and_then(AgentInfrastructureError::preflight)
        {
            latest.metadata.worker_mut().preflight = Some(preflight.clone());
        }
        let next_attempts = attempts + 1;
        latest.metadata.worker_mut().dispatch_attempts = if is_infrastructure_error {
            next_attempts.max(3)
        } else {
            next_attempts
        };
        latest.metadata.worker_mut().dispatch_error = error_text.clone();
        latest.set_state(if is_infrastructure_error || next_attempts >= 3 {
            JobState::WorkerDispatchFailed
        } else {
            JobState::Queued
        });
        self.timeline_store.update_job(&latest)?;
        log(&format!(
            "worker dispatch failed for {job_id}: {error_text}"
        ));
        Ok(json!({"dispatched": false, "job": latest.to_value(), "error": error_text}))
    }

    pub(crate) fn post_worker_job_result(
        &self,
        job: &Job,
        response_text: &str,
    ) -> Result<DiscordPostMetadata> {
        let channel_id = self.control_config.bots_channel_id.clone();
        if channel_id.is_empty() {
            anyhow::bail!("botsChannelId is not configured");
        }
        let requested_by = job.requested_by_user_id.clone();
        let content = if requested_by.is_empty() {
            response_text.trim().to_string()
        } else {
            format!("<@{requested_by}> {}", response_text.trim())
        };
        let mut posted = Vec::new();
        for chunk in split_message_chunks(&content, MESSAGE_CHUNK_LIMIT) {
            let payload = send_message(&channel_id, &chunk)?;
            posted.push(DiscordPostedMessageMetadata {
                channel_id: channel_id.clone(),
                message_id: string_field(&payload, "id"),
            });
        }
        Ok(DiscordPostMetadata {
            channel_id,
            messages: posted,
        })
    }
}
