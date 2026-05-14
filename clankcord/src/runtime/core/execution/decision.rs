use crate::runtime::{Job, JobFailure, JobOutput};

#[derive(Debug, Clone)]
pub(crate) enum JobDecision {
    Complete(JobOutput),
    Fail(JobFailure),
    Wait,
    WaitFor(Vec<Job>),
}

impl JobDecision {
    pub(crate) fn fail(message: impl Into<String>) -> Self {
        Self::Fail(JobFailure::new(message))
    }
}
