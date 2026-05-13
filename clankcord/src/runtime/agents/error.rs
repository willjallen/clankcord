use std::fmt;

use crate::runtime::jobs::AgentPreflightMetadata;

#[derive(Debug, Clone)]
pub(crate) struct AgentInfrastructureError {
    detail: String,
    preflight: Option<AgentPreflightMetadata>,
}

impl AgentInfrastructureError {
    pub(crate) fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
            preflight: None,
        }
    }

    pub(crate) fn with_preflight(
        detail: impl Into<String>,
        preflight: AgentPreflightMetadata,
    ) -> Self {
        Self {
            detail: detail.into(),
            preflight: Some(preflight),
        }
    }

    pub(crate) fn preflight(&self) -> Option<&AgentPreflightMetadata> {
        self.preflight.as_ref()
    }
}

impl fmt::Display for AgentInfrastructureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl std::error::Error for AgentInfrastructureError {}
