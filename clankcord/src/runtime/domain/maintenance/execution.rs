use serde_json::{Value, json};

use crate::Result;
use crate::config;
use crate::runtime::core::execution::JobDecision;
use crate::runtime::domain::maintenance::definitions::evaluate_maintenance_job_definitions;
use crate::runtime::timeline::{JobVisibility, isoformat_z, parse_instant, utc_now};
use crate::runtime::{Job, JobKind, JobOutput, JobState, Runtime, RuntimeMaintenancePayload};

impl Runtime {
    pub(crate) async fn prepare_runtime_maintenance_job(
        &self,
        job: &Job,
        payload: &RuntimeMaintenancePayload,
    ) -> Result<JobDecision> {
        let next = self.schedule_next_runtime_maintenance(payload).await?;
        let mut submitted = Vec::new();
        for (definition_name, definition_job) in evaluate_maintenance_job_definitions(job) {
            let created = self.timeline_store.create_job(definition_job).await?;
            submitted.push(json!({
                "definition": definition_name,
                "job_id": created.id,
                "job_kind": created.kind.as_str(),
            }));
        }
        for definition_job in self.agent_thread_title_refresh_jobs(job).await? {
            let created = self.timeline_store.create_job(definition_job).await?;
            submitted.push(json!({
                "definition": "agent_thread_title_refresh",
                "job_id": created.id,
                "job_kind": created.kind.as_str(),
            }));
        }
        let requeued_audio_segments = self
            .timeline_store
            .requeue_failed_audio_segment_jobs(config::failed_audio_segment_retry_batch_limit())
            .await?;
        Ok(JobDecision::Complete(JobOutput::from_boundary_json(
            &json!({
                "kind": "runtime_maintenance",
                "next_job_id": next.id,
                "next_run_at": next.next_run_at,
                "submitted_jobs": submitted,
                "requeued_audio_segments": requeued_audio_segments,
            }),
        )?))
    }

    pub(crate) async fn prepare_voice_status_sync_job(&mut self, job: &Job) -> Result<JobDecision> {
        let children = self.timeline_store.list_child_jobs(&job.id).await?;
        if children.iter().any(|child| !child.state.is_terminal()) {
            return Ok(JobDecision::Wait);
        }
        if let Some(failed) = children
            .iter()
            .find(|child| child.state != JobState::Complete)
        {
            return Ok(JobDecision::fail(format!(
                "voice status snapshot dependency {} ended as {}: {}",
                failed.id, failed.state, failed.metadata.error
            )));
        }
        if let Some(snapshot_job) = children
            .iter()
            .find(|child| child.kind == JobKind::DiscordVoiceStatusSnapshot)
        {
            let Some(JobOutput::DiscordVoiceStatusSnapshot(output)) =
                snapshot_job.metadata.output.clone()
            else {
                return Ok(JobDecision::fail(format!(
                    "voice status snapshot child {} completed without snapshot output",
                    snapshot_job.id
                )));
            };
            let bot_count = output.bots.len();
            let session_count = output.sessions.len();
            self.sync_voice_adapter_status(output.bots, output.sessions)
                .await?;
            return Ok(JobDecision::Complete(JobOutput::from_boundary_json(
                &json!({
                    "kind": "voice_status_sync",
                    "snapshot_job_id": snapshot_job.id,
                    "bot_count": bot_count,
                    "session_count": session_count,
                }),
            )?));
        }
        Ok(JobDecision::WaitFor(vec![
            Job::discord_voice_status_snapshot(job.id.clone()),
        ]))
    }

    pub(crate) async fn prepare_automation_evaluation_job(
        &mut self,
        _job: &Job,
    ) -> Result<JobDecision> {
        let run = self.run_automations().await?;
        Ok(JobDecision::Complete(JobOutput::from_boundary_json(
            &json!({
                "kind": "automation_evaluation",
                "result": run.to_json(),
            }),
        )?))
    }

    pub(crate) async fn prepare_stale_wake_probe_sweep_job(
        &self,
        max_age_seconds: i64,
    ) -> Result<JobDecision> {
        let cancelled = self
            .timeline_store
            .cancel_stale_wake_probe_jobs(max_age_seconds)
            .await?;
        Ok(JobDecision::Complete(JobOutput::from_boundary_json(
            &json!({
                "kind": "stale_wake_probe_sweep",
                "max_age_seconds": max_age_seconds,
                "jobs": cancelled,
            }),
        )?))
    }

    pub(crate) async fn prepare_stale_running_job_sweep_job(
        &self,
        timeout_minutes: i64,
    ) -> Result<JobDecision> {
        let timed_out = self.fail_stale_running_jobs(timeout_minutes).await?;
        Ok(JobDecision::Complete(JobOutput::from_boundary_json(
            &json!({
                "kind": "stale_running_job_sweep",
                "timeout_minutes": timeout_minutes,
                "jobs": timed_out,
            }),
        )?))
    }

    pub(crate) async fn prepare_ephemeral_job_gc_job(
        &self,
        batch_limit: usize,
    ) -> Result<JobDecision> {
        let result = self
            .timeline_store
            .garbage_collect_ephemeral_jobs(batch_limit)
            .await?;
        Ok(JobDecision::Complete(JobOutput::from_boundary_json(
            &json!({
                "kind": "ephemeral_job_gc",
                "result": result,
            }),
        )?))
    }

    async fn schedule_next_runtime_maintenance(
        &self,
        payload: &RuntimeMaintenancePayload,
    ) -> Result<Job> {
        let next_run_at = utc_now() + chrono::Duration::milliseconds(payload.interval_ms);
        let mut next = Job::runtime_maintenance(payload.interval_ms);
        next.next_run_at = Some(isoformat_z(Some(next_run_at)));
        self.timeline_store.create_job(next).await
    }

    async fn fail_stale_running_jobs(&self, timeout_minutes: i64) -> Result<Vec<Value>> {
        let timeout = chrono::Duration::minutes(timeout_minutes);
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
                .map(|value| now - value < timeout)
                .unwrap_or(false)
            {
                continue;
            }
            job.set_state(JobState::FailedTimeout);
            job.metadata.error = "job exceeded stale running-job timeout".to_string();
            job.metadata.timed_out_at = isoformat_z(None);
            self.timeline_store.update_job(&job).await?;
            timed_out.push(job.to_value());
        }
        Ok(timed_out)
    }
}
