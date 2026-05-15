use serde_json::{Value, json};

use crate::Result;
use crate::runtime::core::execution::JobDecision;
use crate::runtime::timeline::JobVisibility;
use crate::runtime::timeline::{parse_instant, utc_now};
use crate::runtime::{Job, JobKind, JobOutput, JobState, Runtime};

use super::routes;

impl Runtime {
    pub(crate) async fn run_blocking_maintenance(&self) -> Result<Value> {
        let stale_wake_probes = self
            .timeline_store
            .cancel_stale_wake_probe_jobs(wake_probe_max_queue_age_seconds())
            .await?;
        let timed_out = self.fail_stale_running_jobs().await?;
        let resolved_waiting = self.resolve_waiting_jobs().await?;
        let ephemeral_gc = self
            .timeline_store
            .garbage_collect_ephemeral_jobs(ephemeral_job_gc_batch_limit())
            .await?;
        Ok(json!({
            "staleWakeProbes": stale_wake_probes,
            "timedOutJobs": timed_out,
            "resolvedWaitingJobs": resolved_waiting,
            "ephemeralJobGc": ephemeral_gc,
        }))
    }

    pub async fn dispatch_claimed_runtime_job(&mut self, running: Job) -> Result<Value> {
        let job_id = running.id.clone();
        match routes::execute_runtime_async(self, &running).await {
            Ok(decision) => self.apply_job_decision(&job_id, running, decision).await,
            Err(error) => self.fail_dispatched_job(&job_id, running, error).await,
        }
    }

    pub(crate) async fn dispatch_claimed_blocking_job(&self, running: Job) -> Result<Value> {
        let job_id = running.id.clone();
        match running.kind {
            JobKind::WakeProbe => match routes::execute_wake_probe(self, &running).await {
                Ok(result) => self.complete_dispatched_job(&job_id, running, result).await,
                Err(error) => self.fail_dispatched_job(&job_id, running, error).await,
            },
            JobKind::AudioSegment => match routes::execute_audio_segment(self, &running).await {
                Ok(result) => self.complete_dispatched_job(&job_id, running, result).await,
                Err(error) => self.fail_dispatched_job(&job_id, running, error).await,
            },
            JobKind::RefineTranscript => {
                match routes::execute_refine_transcript(self, &running).await {
                    Ok(result) => self.complete_dispatched_job(&job_id, running, result).await,
                    Err(error) => self.fail_dispatched_job(&job_id, running, error).await,
                }
            }
            JobKind::AgentTask => self.dispatch_claimed_agent_task_job(running).await,
            JobKind::Response => match routes::execute_response(self, &running).await {
                Ok(result) => self.complete_dispatched_job(&job_id, running, result).await,
                Err(error) => self.fail_dispatched_job(&job_id, running, error).await,
            },
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
        if latest.state != JobState::Waiting && latest.state != JobState::Queued {
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

    async fn fail_stale_running_jobs(&self) -> Result<Vec<Value>> {
        let now = utc_now();
        let mut timed_out = Vec::new();
        for mut job in self
            .timeline_store
            .list_jobs_with_visibility(
                None,
                Some(JobState::Running),
                JobVisibility::IncludeEphemeral,
            )
            .await?
        {
            if job.kind == JobKind::AgentTask {
                continue;
            }
            let updated_at = parse_instant(&job.updated_at);
            if updated_at
                .map(|value| (now - value).num_minutes() < 30)
                .unwrap_or(false)
            {
                continue;
            }
            job.set_state(JobState::FailedTimeout);
            job.metadata.error = "job exceeded maintainer timeout".to_string();
            job.metadata.timed_out_at = crate::runtime::timeline::isoformat_z(None);
            self.timeline_store.update_job(&job).await?;
            timed_out.push(job.to_value());
        }
        Ok(timed_out)
    }

    pub(crate) async fn resolve_waiting_jobs(&self) -> Result<Vec<Value>> {
        self.timeline_store.resolve_waiting_jobs().await
    }
}

fn wake_probe_max_queue_age_seconds() -> i64 {
    std::env::var("CLANKCORD_WAKE_PROBE_MAX_QUEUE_AGE_SECONDS")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(5)
        .clamp(1, 60)
}

fn ephemeral_job_gc_batch_limit() -> usize {
    std::env::var("CLANKCORD_EPHEMERAL_JOB_GC_BATCH_LIMIT")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(256)
        .clamp(1, 1000)
}
