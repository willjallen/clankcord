use crate::model::job::{Job, JobFailure, JobOutput};

#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)] // cold dispatch value: built and consumed once per job run; boxing buys only indirection
pub enum JobDecision {
    Complete(JobOutput),
    Fail(JobFailure),
    Wait,
    WaitFor(Vec<Job>),
}

impl JobDecision {
    pub fn fail(message: impl Into<String>) -> Self {
        Self::Fail(JobFailure::new(message))
    }
}
