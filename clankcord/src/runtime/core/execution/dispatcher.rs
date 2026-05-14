use serde_json::{Value, json};

use crate::Result;
use crate::runtime::core::execution::JobDecision;
use crate::runtime::timeline::{parse_instant, utc_now};
use crate::runtime::{Job, JobKind, JobOutput, JobState, Runtime};

use super::routes;

impl Runtime {
    pub(crate) fn run_blocking_maintenance(&self) -> Result<Value> {
        let stale_wake_probes = self
            .timeline_store
            .cancel_stale_wake_probe_jobs(wake_probe_max_queue_age_seconds())?;
        let timed_out = self.fail_stale_running_jobs()?;
        let resolved_waiting = self.resolve_waiting_jobs()?;
        Ok(json!({
            "staleWakeProbes": stale_wake_probes,
            "timedOutJobs": timed_out,
            "resolvedWaitingJobs": resolved_waiting,
        }))
    }

    pub(crate) fn dispatch_claimed_runtime_job(&mut self, running: Job) -> Result<Value> {
        let job_id = running.id.clone();
        match routes::execute_runtime_async(self, &running) {
            Ok(decision) => self.apply_job_decision(&job_id, running, decision),
            Err(error) => self.fail_dispatched_job(&job_id, running, error),
        }
    }

    pub(crate) fn dispatch_claimed_blocking_job(&self, running: Job) -> Result<Value> {
        let job_id = running.id.clone();
        match running.kind {
            JobKind::WakeProbe => match routes::execute_wake_probe(self, &running) {
                Ok(result) => self.complete_dispatched_job(&job_id, running, result),
                Err(error) => self.fail_dispatched_job(&job_id, running, error),
            },
            JobKind::AudioSegment => match routes::execute_audio_segment(self, &running) {
                Ok(result) => self.complete_dispatched_job(&job_id, running, result),
                Err(error) => self.fail_dispatched_job(&job_id, running, error),
            },
            JobKind::RefineTranscript => match routes::execute_refine_transcript(self, &running) {
                Ok(result) => self.complete_dispatched_job(&job_id, running, result),
                Err(error) => self.fail_dispatched_job(&job_id, running, error),
            },
            JobKind::AgentTask => self.dispatch_claimed_agent_task_job(running),
            JobKind::Response => match routes::execute_response(self, &running) {
                Ok(result) => self.complete_dispatched_job(&job_id, running, result),
                Err(error) => self.fail_dispatched_job(&job_id, running, error),
            },
            kind => anyhow::bail!("job kind {kind} is not handled by blocking dispatcher"),
        }
    }

    pub(crate) fn apply_job_decision(
        &self,
        job_id: &str,
        fallback_job: Job,
        decision: JobDecision,
    ) -> Result<Value> {
        match decision {
            JobDecision::Complete(output) => {
                self.complete_dispatched_job(job_id, fallback_job, output)
            }
            JobDecision::Fail(failure) => {
                self.fail_dispatched_job(job_id, fallback_job, anyhow::anyhow!(failure.message))
            }
            JobDecision::Wait => self.wait_dispatched_job(job_id, fallback_job, Vec::new()),
            JobDecision::WaitFor(children) => {
                self.wait_dispatched_job(job_id, fallback_job, children)
            }
        }
    }

    pub(crate) fn complete_dispatched_job(
        &self,
        job_id: &str,
        fallback_job: Job,
        output: JobOutput,
    ) -> Result<Value> {
        let mut latest = self
            .timeline_store
            .get_job(job_id)
            .unwrap_or_else(|_| fallback_job.clone());
        latest.metadata.output = Some(output.clone());
        if latest.state != JobState::Waiting && latest.state != JobState::Queued {
            latest.mark_complete();
        }
        self.timeline_store.update_job(&latest)?;
        Ok(json!({"dispatched": true, "job": latest.to_value(), "result": output.to_json()}))
    }

    pub(crate) fn wait_dispatched_job(
        &self,
        job_id: &str,
        fallback_job: Job,
        children: Vec<Job>,
    ) -> Result<Value> {
        let latest = self
            .timeline_store
            .get_job(job_id)
            .unwrap_or_else(|_| fallback_job.clone());
        let mut child_ids = Vec::new();
        if children.is_empty() {
            let mut waiting = latest.clone();
            if !waiting.state.is_terminal() {
                waiting.mark_waiting();
                self.timeline_store.update_job(&waiting)?;
            }
        } else {
            for child in children {
                let child = self.timeline_store.create_child_job(&latest, child)?;
                child_ids.push(child.id);
            }
        }
        Ok(
            json!({"dispatched": true, "waiting": true, "job_id": job_id, "child_job_ids": child_ids}),
        )
    }

    pub(crate) fn fail_dispatched_job(
        &self,
        job_id: &str,
        fallback_job: Job,
        error: anyhow::Error,
    ) -> Result<Value> {
        let error_text = error.to_string();
        let mut latest = self
            .timeline_store
            .get_job(job_id)
            .unwrap_or_else(|_| fallback_job.clone());
        latest.set_state(JobState::Failed);
        latest.metadata.error = error_text.clone();
        self.timeline_store.update_job(&latest)?;
        crate::runtime::log(&format!("job dispatch failed {job_id}: {error_text}"));
        Ok(json!({"dispatched": false, "job": latest.to_value(), "error": error_text}))
    }

    fn fail_stale_running_jobs(&self) -> Result<Vec<Value>> {
        let now = utc_now();
        let mut timed_out = Vec::new();
        for mut job in self
            .timeline_store
            .list_jobs(None, Some(JobState::Running))?
        {
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
            self.timeline_store.update_job(&job)?;
            timed_out.push(job.to_value());
        }
        Ok(timed_out)
    }

    pub(crate) fn resolve_waiting_jobs(&self) -> Result<Vec<Value>> {
        self.timeline_store.resolve_waiting_jobs()
    }
}

fn wake_probe_max_queue_age_seconds() -> i64 {
    std::env::var("CLANKCORD_WAKE_PROBE_MAX_QUEUE_AGE_SECONDS")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(5)
        .clamp(1, 60)
}
