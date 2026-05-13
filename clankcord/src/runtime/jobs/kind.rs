use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum JobKind {
    AudioSegment,
    AgentTask,
    RefineTranscript,
    ConfirmationRequired,
    RouterCommand,
    RoomAgentPlacement,
    RuntimeControl,
}

impl JobKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AudioSegment => "audio_segment",
            Self::AgentTask => "agent_task",
            Self::RefineTranscript => "refine_transcript",
            Self::ConfirmationRequired => "confirmation_required",
            Self::RouterCommand => "router_command",
            Self::RoomAgentPlacement => "room_agent_placement",
            Self::RuntimeControl => "runtime_control",
        }
    }

    pub fn is_agent_task(self) -> bool {
        matches!(self, Self::AgentTask)
    }
}

impl fmt::Display for JobKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for JobKind {
    type Err = anyhow::Error;

    fn from_str(raw: &str) -> Result<Self> {
        match raw.trim() {
            "audio_segment" => Ok(Self::AudioSegment),
            "agent_task" => Ok(Self::AgentTask),
            "refine_transcript" => Ok(Self::RefineTranscript),
            "confirmation_required" => Ok(Self::ConfirmationRequired),
            "router_command" => Ok(Self::RouterCommand),
            "room_agent_placement" => Ok(Self::RoomAgentPlacement),
            "runtime_control" => Ok(Self::RuntimeControl),
            value => anyhow::bail!("unknown job kind: {value}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum JobState {
    Queued,
    Running,
    Waiting,
    Complete,
    Cancelled,
    CancelRequested,
    ConfirmationPending,
    Approved,
    ApprovalFailed,
    Failed,
    FailedTimeout,
    AgentDispatchFailed,
    FailedDraftRetained,
}

impl JobState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Waiting => "waiting",
            Self::Complete => "complete",
            Self::Cancelled => "cancelled",
            Self::CancelRequested => "cancel_requested",
            Self::ConfirmationPending => "confirmation_pending",
            Self::Approved => "approved",
            Self::ApprovalFailed => "approval_failed",
            Self::Failed => "failed",
            Self::FailedTimeout => "failed_timeout",
            Self::AgentDispatchFailed => "agent_dispatch_failed",
            Self::FailedDraftRetained => "failed_draft_retained",
        }
    }

    pub fn is_cancellable(self) -> bool {
        matches!(
            self,
            Self::Queued
                | Self::Running
                | Self::Waiting
                | Self::CancelRequested
                | Self::ConfirmationPending
        )
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Complete
                | Self::Cancelled
                | Self::ApprovalFailed
                | Self::Failed
                | Self::FailedTimeout
                | Self::AgentDispatchFailed
                | Self::FailedDraftRetained
        )
    }
}

impl fmt::Display for JobState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for JobState {
    type Err = anyhow::Error;

    fn from_str(raw: &str) -> Result<Self> {
        match raw.trim() {
            "queued" => Ok(Self::Queued),
            "running" => Ok(Self::Running),
            "waiting" => Ok(Self::Waiting),
            "complete" => Ok(Self::Complete),
            "cancelled" => Ok(Self::Cancelled),
            "cancel_requested" => Ok(Self::CancelRequested),
            "confirmation_pending" => Ok(Self::ConfirmationPending),
            "approved" => Ok(Self::Approved),
            "approval_failed" => Ok(Self::ApprovalFailed),
            "failed" => Ok(Self::Failed),
            "failed_timeout" => Ok(Self::FailedTimeout),
            "agent_dispatch_failed" => Ok(Self::AgentDispatchFailed),
            "failed_draft_retained" => Ok(Self::FailedDraftRetained),
            value => anyhow::bail!("unknown job state: {value}"),
        }
    }
}
