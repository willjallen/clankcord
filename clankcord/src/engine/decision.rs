use crate::model::job::{Job, JobFailure, JobOutput};

#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)] // wire/decision enums: boxing buys nothing on the encoded form
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
