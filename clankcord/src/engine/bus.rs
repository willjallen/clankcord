use serde_json::{Value, json};

use crate::Result;
use crate::model::job::{Job, RuntimeControlAction};
use crate::runtime::timeline::TimelineStore;
use crate::runtime::util::log;

/// Narrow job-submission capability handed to adapters and ingress surfaces.
///
/// Submission is a durable insert: `TimelineStore::create_job` commits the row
/// and wakes the dispatcher in the same call, so a submitted job is always
/// picked up promptly regardless of where it was created.
#[derive(Debug, Clone)]
pub struct JobBus {
    store: TimelineStore,
}

impl JobBus {
    pub fn new(store: TimelineStore) -> Self {
        Self { store }
    }

    pub async fn submit(&self, job: Job) -> Result<Value> {
        let created = self.store.create_job(job).await?;
        Ok(job_created_payload(created))
    }

    /// Fire-and-forget submission for latency-sensitive callers (e.g. the
    /// voice flush path). Failures are logged, never surfaced.
    pub fn submit_detached(&self, job: Job) {
        let bus = self.clone();
        tokio::spawn(async move {
            let job_id = job.id.clone();
            if let Err(error) = bus.submit(job).await {
                log(&format!("detached job submission failed {job_id}: {error}"));
            }
        });
    }

    pub async fn submit_runtime_control_for_target(
        &self,
        target_job_id: &str,
        action: RuntimeControlAction,
        actor_user_id: String,
    ) -> Result<Value> {
        let target = self.store.get_job(target_job_id).await?;
        let job = Job::runtime_control(
            target.scope(),
            actor_user_id,
            action,
            target_job_id.to_string(),
        );
        self.submit(job).await
    }
}

pub(crate) fn job_created_payload(job: Job) -> Value {
    json!({"kind": "job_created", "job_ids": [job.id.clone()], "job": job.to_value()})
}
