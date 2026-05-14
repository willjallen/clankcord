use serde_json::{Map, Value, json};

use crate::Result;
use crate::runtime::timeline::{parse_instant, utc_now};
use crate::runtime::{BinaryPayload, Job, JobKind, JobState, Runtime};

use super::{RuntimeEffects, routes};

impl Runtime {
    pub(crate) async fn dispatch_due_jobs(
        &mut self,
        effects: Option<&dyn RuntimeEffects>,
    ) -> Result<Value> {
        let mut results = Map::new();
        for kind in [
            JobKind::RuntimeControl,
            JobKind::Command,
            JobKind::RoomAgentPlacement,
        ] {
            results.insert(
                kind.as_str().to_string(),
                self.dispatch_next_due_async_job(kind, effects).await?,
            );
        }
        Ok(Value::Object(results))
    }

    pub(crate) fn dispatch_due_blocking_jobs(&self) -> Result<Value> {
        let mut results = Map::new();
        for kind in [
            JobKind::AudioSegment,
            JobKind::RefineTranscript,
            JobKind::AgentTask,
        ] {
            let result = match kind {
                JobKind::AudioSegment => self.dispatch_audio_segment_jobs()?,
                JobKind::RefineTranscript => self.dispatch_next_refine_transcript_job()?,
                JobKind::AgentTask => self.dispatch_next_due_agent_task_job()?,
                _ => unreachable!("blocking dispatcher kind list is fixed"),
            };
            results.insert(kind.as_str().to_string(), result);
        }
        Ok(Value::Object(results))
    }

    pub(crate) fn run_blocking_maintenance(&self) -> Result<Value> {
        let timed_out = self.fail_stale_running_jobs()?;
        let resolved_waiting = self.resolve_waiting_jobs()?;
        Ok(json!({
            "timedOutJobs": timed_out,
            "resolvedWaitingJobs": resolved_waiting,
        }))
    }

    async fn dispatch_next_due_async_job(
        &mut self,
        kind: JobKind,
        effects: Option<&dyn RuntimeEffects>,
    ) -> Result<Value> {
        let Some(job) = self.next_queued_job(kind)? else {
            return Ok(json!({"dispatched": false, "reason": format!("no queued {kind} jobs")}));
        };
        let job_id = job.id.clone();
        let mut running = job.clone();
        running.mark_running();
        self.timeline_store.update_job(&running)?;

        match routes::execute_async(self, &running, effects).await {
            Ok(result) => self.complete_dispatched_job(&job_id, running, result),
            Err(error) => self.fail_dispatched_job(&job_id, running, error),
        }
    }

    fn dispatch_next_refine_transcript_job(&self) -> Result<Value> {
        let Some(job) = self.next_queued_job(JobKind::RefineTranscript)? else {
            return Ok(json!({"dispatched": false, "reason": "no queued refinement jobs"}));
        };
        routes::execute_refine_transcript(self, &job)
    }

    fn dispatch_next_audio_segment_job(&self) -> Result<Value> {
        let Some(job) = self.next_queued_audio_segment_job()? else {
            return Ok(json!({"dispatched": false, "reason": "no queued audio segment jobs"}));
        };
        let job_id = job.id.clone();
        let mut running = job.clone();
        running.mark_running();
        self.timeline_store.update_job(&running)?;
        match routes::execute_audio_segment(self, &running) {
            Ok(result) => self.complete_dispatched_job(&job_id, running, result),
            Err(error) => self.fail_dispatched_job(&job_id, running, error),
        }
    }

    fn dispatch_audio_segment_jobs(&self) -> Result<Value> {
        let limit = audio_segment_dispatch_limit();
        let mut jobs = Vec::new();
        for _ in 0..limit {
            let result = self.dispatch_next_audio_segment_job()?;
            if result.get("dispatched").and_then(Value::as_bool) != Some(true) {
                if jobs.is_empty() {
                    return Ok(result);
                }
                break;
            }
            jobs.push(result);
        }
        Ok(json!({
            "dispatched": !jobs.is_empty(),
            "count": jobs.len(),
            "jobs": jobs,
        }))
    }

    fn next_queued_audio_segment_job(&self) -> Result<Option<Job>> {
        Ok(self
            .timeline_store
            .list_jobs(None, Some(JobState::Queued))?
            .into_iter()
            .filter(|job| job.kind == JobKind::AudioSegment && !job.id.trim().is_empty())
            .min_by(|left, right| {
                left.created_at
                    .cmp(&right.created_at)
                    .then_with(|| left.id.cmp(&right.id))
            }))
    }

    pub(crate) fn next_queued_job(&self, kind: JobKind) -> Result<Option<Job>> {
        Ok(self
            .timeline_store
            .list_jobs(None, Some(JobState::Queued))?
            .into_iter()
            .find(|job| job.kind == kind && !job.id.trim().is_empty()))
    }

    fn complete_dispatched_job(
        &self,
        job_id: &str,
        fallback_job: Job,
        result: Value,
    ) -> Result<Value> {
        let mut latest = self
            .timeline_store
            .get_job(job_id)
            .unwrap_or_else(|_| fallback_job.clone());
        latest.metadata.result = BinaryPayload::from_json(&result)?;
        if latest.state != JobState::Waiting {
            latest.mark_complete();
        }
        self.timeline_store.update_job(&latest)?;
        Ok(json!({"dispatched": true, "job": latest.to_value(), "result": result}))
    }

    fn fail_dispatched_job(
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

    fn resolve_waiting_jobs(&self) -> Result<Vec<Value>> {
        let mut resolved = Vec::new();
        for mut parent in self
            .timeline_store
            .list_jobs(None, Some(JobState::Waiting))?
        {
            let children = self.timeline_store.list_child_jobs(&parent.id)?;
            if children.is_empty() || children.iter().any(|job| !job.state.is_terminal()) {
                continue;
            }
            if children.iter().all(|job| job.state == JobState::Complete) {
                parent.mark_complete();
            } else if children.iter().any(|job| job.state == JobState::Cancelled) {
                parent.mark_cancelled();
            } else {
                parent.set_state(if parent.kind == JobKind::ConfirmationRequired {
                    JobState::ApprovalFailed
                } else {
                    JobState::Failed
                });
                parent.metadata.error = children
                    .iter()
                    .filter(|job| job.state != JobState::Complete)
                    .map(|job| format!("{} {}", job.id, job.state))
                    .collect::<Vec<_>>()
                    .join("; ");
            }
            self.timeline_store.update_job(&parent)?;
            resolved.push(parent.to_value());
        }
        Ok(resolved)
    }
}

fn audio_segment_dispatch_limit() -> usize {
    std::env::var("CLAWCORD_AUDIO_SEGMENT_DISPATCH_LIMIT")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(8)
        .clamp(1, 64)
}
