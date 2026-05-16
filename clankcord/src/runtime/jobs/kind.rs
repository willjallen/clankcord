use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum JobKind {
    AudioSegment,
    WakeActivation,
    AgentTask,
    DiscordTextMessage,
    Response,
    RefineTranscript,
    ConfirmationRequired,
    Command,
    RoomAgentPlacement,
    DiscordVoiceJoin,
    DiscordVoiceLeave,
    DiscordVoicePlayback,
    DiscordVoiceMute,
    DiscordVoicePlayAudio,
    RuntimeControl,
    WakeProbe,
    RuntimeMaintenance,
}

impl JobKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AudioSegment => "audio_segment",
            Self::WakeActivation => "wake_activation",
            Self::AgentTask => "agent_task",
            Self::DiscordTextMessage => "discord_text_message",
            Self::Response => "response",
            Self::RefineTranscript => "refine_transcript",
            Self::ConfirmationRequired => "confirmation_required",
            Self::Command => "command",
            Self::RoomAgentPlacement => "room_agent_placement",
            Self::DiscordVoiceJoin => "discord_voice_join",
            Self::DiscordVoiceLeave => "discord_voice_leave",
            Self::DiscordVoicePlayback => "discord_voice_playback",
            Self::DiscordVoiceMute => "discord_voice_mute",
            Self::DiscordVoicePlayAudio => "discord_voice_play_audio",
            Self::RuntimeControl => "runtime_control",
            Self::WakeProbe => "wake_probe",
            Self::RuntimeMaintenance => "runtime_maintenance",
        }
    }

    pub fn is_agent_task(self) -> bool {
        matches!(self, Self::AgentTask)
    }

    pub fn is_ephemeral(self) -> bool {
        matches!(
            self,
            Self::AudioSegment | Self::WakeProbe | Self::RuntimeMaintenance
        )
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
            "wake_activation" => Ok(Self::WakeActivation),
            "agent_task" => Ok(Self::AgentTask),
            "discord_text_message" => Ok(Self::DiscordTextMessage),
            "response" => Ok(Self::Response),
            "refine_transcript" => Ok(Self::RefineTranscript),
            "confirmation_required" => Ok(Self::ConfirmationRequired),
            "command" => Ok(Self::Command),
            "room_agent_placement" => Ok(Self::RoomAgentPlacement),
            "discord_voice_join" => Ok(Self::DiscordVoiceJoin),
            "discord_voice_leave" => Ok(Self::DiscordVoiceLeave),
            "discord_voice_playback" => Ok(Self::DiscordVoicePlayback),
            "discord_voice_mute" => Ok(Self::DiscordVoiceMute),
            "discord_voice_play_audio" => Ok(Self::DiscordVoicePlayAudio),
            "runtime_control" => Ok(Self::RuntimeControl),
            "wake_probe" => Ok(Self::WakeProbe),
            "runtime_maintenance" => Ok(Self::RuntimeMaintenance),
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
