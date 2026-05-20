use serde_json::{Value, json};

use crate::Result;
use crate::runtime::core::execution::JobDecision;
use crate::runtime::domain::external::RuntimeExternalApi;
use crate::runtime::domain::voice_capture::segments;
use crate::runtime::{Job, JobKind, JobOutput, JobState, Runtime};

use super::routes;

impl Runtime {
    pub async fn dispatch_claimed_runtime_job(&mut self, running: Job) -> Result<Value> {
        let job_id = running.id.clone();
        match routes::execute_runtime_async(self, &running).await {
            Ok(decision) => self.apply_job_decision(&job_id, running, decision).await,
            Err(error) => self.fail_dispatched_job(&job_id, running, error).await,
        }
    }

    pub(crate) async fn dispatch_claimed_runtime_job_with_external_api<A>(
        &mut self,
        running: Job,
        external_api: &A,
    ) -> Result<Value>
    where
        A: RuntimeExternalApi,
    {
        let job_id = running.id.clone();
        match routes::execute_runtime_async_with_external_api(self, &running, external_api).await {
            Ok(decision) => self.apply_job_decision(&job_id, running, decision).await,
            Err(error) => self.fail_dispatched_job(&job_id, running, error).await,
        }
    }

    pub async fn dispatch_claimed_blocking_job(&self, running: Job) -> Result<Value> {
        let job_id = running.id.clone();
        match running.kind {
            JobKind::WakeProbe => match routes::execute_wake_probe(self, &running).await {
                Ok(result) => self.complete_dispatched_job(&job_id, running, result).await,
                Err(error) => self.fail_dispatched_job(&job_id, running, error).await,
            },
            JobKind::AudioSegment => match routes::execute_audio_segment(self, &running).await {
                Ok(result) => self.complete_dispatched_job(&job_id, running, result).await,
                Err(error) if segments::is_retryable_audio_segment_error(&error) => {
                    let retry = segments::retry_plan(error);
                    self.requeue_dispatched_job(
                        &job_id,
                        running,
                        retry.delay_for_attempt,
                        retry.error,
                        retry.log_prefix,
                    )
                    .await
                }
                Err(error) => self.fail_dispatched_job(&job_id, running, error).await,
            },
            JobKind::RefineTranscript => {
                match routes::execute_refine_transcript(self, &running).await {
                    Ok(result) => self.complete_dispatched_job(&job_id, running, result).await,
                    Err(error) => self.fail_dispatched_job(&job_id, running, error).await,
                }
            }
            JobKind::AgentTask => self.dispatch_claimed_agent_task_job(running).await,
            JobKind::AgentThreadTitleRefresh => {
                let decision = match &running.payload {
                    crate::runtime::JobPayload::AgentThreadTitleRefresh(payload) => {
                        self.prepare_agent_thread_title_refresh_job(&running, payload)
                            .await
                    }
                    payload => anyhow::bail!(
                        "job kind {} has unexpected payload {}",
                        running.kind,
                        payload.kind()
                    ),
                };
                match decision {
                    Ok(decision) => self.apply_job_decision(&job_id, running, decision).await,
                    Err(error) => self.fail_dispatched_job(&job_id, running, error).await,
                }
            }
            kind => anyhow::bail!("job kind {kind} is not handled by blocking dispatcher"),
        }
    }

    pub(crate) async fn apply_job_decision(
        &self,
        job_id: &str,
        fallback_job: Job,
        decision: JobDecision,
    ) -> Result<Value> {
        match decision {
            JobDecision::Complete(output) => {
                self.complete_dispatched_job(job_id, fallback_job, output)
                    .await
            }
            JobDecision::Fail(failure) => {
                self.fail_dispatched_job(job_id, fallback_job, anyhow::anyhow!(failure.message))
                    .await
            }
            JobDecision::Wait => {
                self.wait_dispatched_job(job_id, fallback_job, Vec::new())
                    .await
            }
            JobDecision::WaitFor(children) => {
                self.wait_dispatched_job(job_id, fallback_job, children)
                    .await
            }
        }
    }

    pub(crate) async fn complete_dispatched_job(
        &self,
        job_id: &str,
        fallback_job: Job,
        output: JobOutput,
    ) -> Result<Value> {
        let mut latest = match self.timeline_store.get_job(job_id).await {
            Ok(job) => job,
            Err(_) => fallback_job.clone(),
        };
        latest.metadata.output = Some(output.clone());
        if latest.state != JobState::Waiting
            && latest.state != JobState::Queued
            && latest.state != JobState::ConfirmationPending
        {
            latest.mark_complete();
        }
        self.timeline_store.update_job(&latest).await?;
        Ok(json!({"dispatched": true, "job": latest.to_value(), "result": output.to_json()}))
    }

    pub(crate) async fn wait_dispatched_job(
        &self,
        job_id: &str,
        fallback_job: Job,
        children: Vec<Job>,
    ) -> Result<Value> {
        let latest = match self.timeline_store.get_job(job_id).await {
            Ok(job) => job,
            Err(_) => fallback_job.clone(),
        };
        let mut child_ids = Vec::new();
        if children.is_empty() {
            let mut waiting = latest.clone();
            if !waiting.state.is_terminal() {
                waiting.mark_waiting();
                self.timeline_store.update_job(&waiting).await?;
            }
        } else {
            for child in children {
                let child = self.timeline_store.create_child_job(&latest, child).await?;
                child_ids.push(child.id);
            }
        }
        Ok(
            json!({"dispatched": true, "waiting": true, "job_id": job_id, "child_job_ids": child_ids}),
        )
    }

    pub(crate) async fn fail_dispatched_job(
        &self,
        job_id: &str,
        fallback_job: Job,
        error: anyhow::Error,
    ) -> Result<Value> {
        let error_text = error.to_string();
        let mut latest = match self.timeline_store.get_job(job_id).await {
            Ok(job) => job,
            Err(_) => fallback_job.clone(),
        };
        latest.set_state(JobState::Failed);
        latest.metadata.error = error_text.clone();
        self.timeline_store.update_job(&latest).await?;
        crate::runtime::log(&format!("job dispatch failed {job_id}: {error_text}"));
        Ok(json!({"dispatched": false, "job": latest.to_value(), "error": error_text}))
    }

    pub(crate) async fn requeue_dispatched_job(
        &self,
        job_id: &str,
        fallback_job: Job,
        delay_for_attempt: fn(i64) -> chrono::Duration,
        error_text: String,
        log_prefix: &'static str,
    ) -> Result<Value> {
        let mut latest = match self.timeline_store.get_job(job_id).await {
            Ok(job) => job,
            Err(_) => fallback_job.clone(),
        };
        latest.attempts = latest.attempts.saturating_add(1);
        latest.set_state(JobState::Queued);
        latest.started_at = None;
        latest.completed_at = None;
        let delay = delay_for_attempt(latest.attempts);
        latest.next_run_at = Some(crate::runtime::timeline::isoformat_z(Some(
            crate::runtime::timeline::utc_now() + delay,
        )));
        latest.metadata.error = error_text.clone();
        self.timeline_store.update_job(&latest).await?;
        crate::runtime::log(&format!(
            "{log_prefix} {job_id}: attempt {} next_run_at {} error: {error_text}",
            latest.attempts,
            latest.next_run_at.clone().unwrap_or_default()
        ));
        Ok(json!({
            "dispatched": false,
            "retry_scheduled": true,
            "job": latest.to_value(),
            "error": error_text,
        }))
    }
}
