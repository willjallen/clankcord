use std::fmt;

#[derive(Debug)]
pub(crate) struct AgentInfrastructureError {
    detail: String,
}

impl AgentInfrastructureError {
    pub(crate) fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }
}

impl fmt::Display for AgentInfrastructureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl std::error::Error for AgentInfrastructureError {}
