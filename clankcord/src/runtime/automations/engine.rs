use serde_json::{Value, json};

use crate::Result;
use crate::runtime::automations::room_agents::RoomAgentPlacementAutomation;
use crate::runtime::{Job, JobKind, JobState, Runtime};

pub(crate) trait Automation: Send + Sync {
    fn name(&self) -> &'static str;
    fn evaluate(&self, context: &AutomationContext<'_>) -> Result<AutomationOutput>;
}

pub(crate) struct AutomationContext<'a> {
    runtime: &'a Runtime,
    active_jobs: &'a [Job],
}

impl<'a> AutomationContext<'a> {
    fn new(runtime: &'a Runtime, active_jobs: &'a [Job]) -> Self {
        Self {
            runtime,
            active_jobs,
        }
    }

    pub(crate) fn runtime(&self) -> &'a Runtime {
        self.runtime
    }

    pub(crate) fn has_active_job(
        &self,
        kind: JobKind,
        guild_id: &str,
        voice_channel_id: &str,
        matches: impl Fn(&Job) -> bool,
    ) -> bool {
        self.active_jobs.iter().any(|job| {
            job.kind == kind
                && is_active_job_state(job.state)
                && job.guild_id == guild_id
                && job.voice_channel_id == voice_channel_id
                && matches(job)
        })
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct AutomationOutput {
    jobs: Vec<Job>,
}

impl AutomationOutput {
    pub(crate) fn empty() -> Self {
        Self { jobs: Vec::new() }
    }

    pub(crate) fn emit(&mut self, job: Job) {
        self.jobs.push(job);
    }

    fn into_jobs(self) -> Vec<Job> {
        self.jobs
    }
}

#[derive(Debug, Clone)]
pub(crate) struct AutomationJob {
    pub automation: &'static str,
    pub job: Job,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct AutomationRun {
    created: Vec<AutomationJob>,
}

impl AutomationRun {
    pub(crate) fn to_json(&self) -> Value {
        json!({
            "createdJobs": self.created.iter().map(|created| {
                json!({
                    "automation": created.automation,
                    "job": created.job.to_value(),
                })
            }).collect::<Vec<_>>(),
        })
    }
}

struct AutomationRunner {
    automations: Vec<Box<dyn Automation>>,
}

impl AutomationRunner {
    fn runtime_default() -> Self {
        Self {
            automations: vec![Box::new(RoomAgentPlacementAutomation)],
        }
    }

    fn run(&self, runtime: &mut Runtime) -> Result<AutomationRun> {
        runtime.prune_expired_room_controls(true);

        let mut active_jobs = runtime.timeline_store.list_jobs(None, None)?;
        let mut created = Vec::new();
        for automation in &self.automations {
            let jobs = {
                let context = AutomationContext::new(runtime, &active_jobs);
                automation.evaluate(&context)?.into_jobs()
            };
            for job in jobs {
                let job = runtime.timeline_store.create_job(job)?;
                active_jobs.push(job.clone());
                created.push(AutomationJob {
                    automation: automation.name(),
                    job,
                });
            }
        }
        Ok(AutomationRun { created })
    }
}

impl Runtime {
    pub(crate) fn run_automations(&mut self) -> Result<AutomationRun> {
        AutomationRunner::runtime_default().run(self)
    }
}

fn is_active_job_state(state: JobState) -> bool {
    matches!(
        state,
        JobState::Queued | JobState::Running | JobState::Waiting
    )
}
