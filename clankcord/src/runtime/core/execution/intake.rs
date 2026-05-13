use serde_json::{Value, json};

use crate::Result;
use crate::runtime::{Job, Runtime, RuntimeControlAction};

impl Runtime {
    pub(crate) fn intake_job(&self, job: Job) -> Result<Value> {
        let job = self.timeline_store.create_job(job)?;
        Ok(json!({"kind": "job_created", "job_ids": [job.id.clone()], "job": job.to_value()}))
    }

    pub(crate) fn runtime_control_job_for_target(
        &self,
        target_job_id: &str,
        action: RuntimeControlAction,
        actor_user_id: String,
    ) -> Result<Job> {
        let target = self.timeline_store.get_job(target_job_id)?;
        Ok(Job::runtime_control(
            target.guild_id,
            target.voice_channel_id,
            actor_user_id,
            action,
            target_job_id.to_string(),
        ))
    }
}
