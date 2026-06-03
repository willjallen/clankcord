use crate::config;
use crate::runtime::Job;

pub(super) trait MaintenanceJobDefinition {
    fn name(&self) -> &'static str;
    fn evaluate(&self, source_job: &Job) -> Vec<Job>;
}

struct VoiceStatusDefinition;
struct AutomationEvaluationDefinition;
struct AgentSessionRetirementDefinition;
struct StaleWakeProbeSweepDefinition;
struct EphemeralJobGcDefinition;

impl MaintenanceJobDefinition for VoiceStatusDefinition {
    fn name(&self) -> &'static str {
        "voice_status"
    }

    fn evaluate(&self, source_job: &Job) -> Vec<Job> {
        vec![Job::voice_status_sync(source_job.id.clone())]
    }
}

impl MaintenanceJobDefinition for AutomationEvaluationDefinition {
    fn name(&self) -> &'static str {
        "automation_evaluation"
    }

    fn evaluate(&self, source_job: &Job) -> Vec<Job> {
        vec![Job::automation_evaluation(source_job.id.clone())]
    }
}

impl MaintenanceJobDefinition for AgentSessionRetirementDefinition {
    fn name(&self) -> &'static str {
        "agent_session_retirement"
    }

    fn evaluate(&self, source_job: &Job) -> Vec<Job> {
        vec![Job::agent_session_retirement(source_job.id.clone())]
    }
}

impl MaintenanceJobDefinition for StaleWakeProbeSweepDefinition {
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

impl MaintenanceJobDefinition for EphemeralJobGcDefinition {
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

pub(super) fn evaluate_maintenance_job_definitions(source_job: &Job) -> Vec<(&'static str, Job)> {
    maintenance_job_definitions()
        .into_iter()
        .flat_map(|definition| {
            let name = definition.name();
            definition
                .evaluate(source_job)
                .into_iter()
                .map(move |job| (name, job))
        })
        .collect()
}

fn maintenance_job_definitions() -> Vec<Box<dyn MaintenanceJobDefinition>> {
    vec![
        Box::new(VoiceStatusDefinition),
        Box::new(AutomationEvaluationDefinition),
        Box::new(AgentSessionRetirementDefinition),
        Box::new(StaleWakeProbeSweepDefinition),
        Box::new(EphemeralJobGcDefinition),
    ]
}
