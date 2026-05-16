use serde_json::{Value, json};

use crate::Result;
use crate::config;
use crate::runtime::core::execution::JobDecision;
use crate::runtime::timeline::{JobVisibility, isoformat_z, parse_instant, utc_now};
use crate::runtime::{Job, JobKind, JobOutput, JobState, Runtime, RuntimeMaintenancePayload};

const STALE_RUNNING_JOB_TIMEOUT_MINUTES: i64 = 30;

trait BackgroundRule {
    fn name(&self) -> &'static str;
    fn evaluate(&self, source_job: &Job) -> Vec<Job>;
}

struct VoiceStatusRule;
struct AutomationEvaluationRule;
struct StaleWakeProbeSweepRule;
struct StaleRunningJobSweepRule;
struct EphemeralJobGcRule;

impl BackgroundRule for VoiceStatusRule {
    fn name(&self) -> &'static str {
        "voice_status"
    }

    fn evaluate(&self, source_job: &Job) -> Vec<Job> {
        vec![Job::voice_status_sync(source_job.id.clone())]
    }
}

impl BackgroundRule for AutomationEvaluationRule {
    fn name(&self) -> &'static str {
        "automation_evaluation"
    }

    fn evaluate(&self, source_job: &Job) -> Vec<Job> {
        vec![Job::automation_evaluation(source_job.id.clone())]
    }
}

impl BackgroundRule for StaleWakeProbeSweepRule {
    fn name(&self) -> &'static str {
        "stale_wake_probe_sweep"
    }

    fn evaluate(&self, source_job: &Job) -> Vec<Job> {
        vec![Job::stale_wake_probe_sweep(
            source_job.id.clone(),
            config::wake_probe_max_queue_age_seconds(),
        )]
    }
}

impl BackgroundRule for StaleRunningJobSweepRule {
    fn name(&self) -> &'static str {
        "stale_running_job_sweep"
    }

    fn evaluate(&self, source_job: &Job) -> Vec<Job> {
        vec![Job::stale_running_job_sweep(
            source_job.id.clone(),
            STALE_RUNNING_JOB_TIMEOUT_MINUTES,
        )]
    }
}

impl BackgroundRule for EphemeralJobGcRule {
    fn name(&self) -> &'static str {
        "ephemeral_job_gc"
    }

    fn evaluate(&self, source_job: &Job) -> Vec<Job> {
        vec![Job::ephemeral_job_gc(
            source_job.id.clone(),
            config::ephemeral_job_gc_batch_limit(),
        )]
    }
}

impl Runtime {
    pub(crate) async fn prepare_runtime_maintenance_job(
        &self,
        job: &Job,
        payload: &RuntimeMaintenancePayload,
    ) -> Result<JobDecision> {
        let next = self.schedule_next_runtime_maintenance(payload).await?;
        let mut submitted = Vec::new();
        for (rule_name, rule_job) in evaluate_background_rules(job) {
            let created = self.timeline_store.create_job(rule_job).await?;
            submitted.push(json!({
                "rule": rule_name,
                "job_id": created.id,
                "job_kind": created.kind.as_str(),
            }));
        }
        Ok(JobDecision::Complete(JobOutput::from_boundary_json(
            &json!({
                "kind": "runtime_maintenance",
                "next_job_id": next.id,
                "next_run_at": next.next_run_at,
                "submitted_jobs": submitted,
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

fn evaluate_background_rules(source_job: &Job) -> Vec<(&'static str, Job)> {
    background_rules()
        .into_iter()
        .flat_map(|rule| {
            let name = rule.name();
            rule.evaluate(source_job)
                .into_iter()
                .map(move |job| (name, job))
        })
        .collect()
}

fn background_rules() -> Vec<Box<dyn BackgroundRule>> {
    vec![
        Box::new(VoiceStatusRule),
        Box::new(AutomationEvaluationRule),
        Box::new(StaleWakeProbeSweepRule),
        Box::new(StaleRunningJobSweepRule),
        Box::new(EphemeralJobGcRule),
    ]
}
