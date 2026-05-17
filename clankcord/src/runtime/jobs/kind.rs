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
    DiscordSlashCommand,
    TextDelivery,
    DiscordTextSend,
    DiscordForumThreadCreate,
    AgentSessionStart,
    AgentSessionSunset,
    AgentSessionResume,
    AgentSessionRetirement,
    TranscriptPublication,
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
    VoiceStatusSync,
    DiscordVoiceStatusSnapshot,
    AutomationEvaluation,
    StaleWakeProbeSweep,
    StaleRunningJobSweep,
    EphemeralJobGc,
    DiscordVoiceDeafen,
}

impl JobKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AudioSegment => "audio_segment",
            Self::WakeActivation => "wake_activation",
            Self::AgentTask => "agent_task",
            Self::DiscordTextMessage => "discord_text_message",
            Self::DiscordSlashCommand => "discord_slash_command",
            Self::TextDelivery => "text_delivery",
            Self::DiscordTextSend => "discord_text_send",
            Self::DiscordForumThreadCreate => "discord_forum_thread_create",
            Self::AgentSessionStart => "agent_session_start",
            Self::AgentSessionSunset => "agent_session_sunset",
            Self::AgentSessionResume => "agent_session_resume",
            Self::AgentSessionRetirement => "agent_session_retirement",
            Self::TranscriptPublication => "transcript_publication",
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
            Self::VoiceStatusSync => "voice_status_sync",
            Self::DiscordVoiceStatusSnapshot => "discord_voice_status_snapshot",
            Self::AutomationEvaluation => "automation_evaluation",
            Self::StaleWakeProbeSweep => "stale_wake_probe_sweep",
            Self::StaleRunningJobSweep => "stale_running_job_sweep",
            Self::EphemeralJobGc => "ephemeral_job_gc",
            Self::DiscordVoiceDeafen => "discord_voice_deafen",
        }
    }

    pub fn is_agent_task(self) -> bool {
        matches!(self, Self::AgentTask)
    }

    pub fn is_ephemeral(self) -> bool {
        matches!(
            self,
            Self::AudioSegment
                | Self::WakeProbe
                | Self::RuntimeMaintenance
                | Self::VoiceStatusSync
                | Self::DiscordVoiceStatusSnapshot
                | Self::AutomationEvaluation
                | Self::AgentSessionRetirement
                | Self::StaleWakeProbeSweep
                | Self::StaleRunningJobSweep
                | Self::EphemeralJobGc
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
            "discord_slash_command" => Ok(Self::DiscordSlashCommand),
            "text_delivery" => Ok(Self::TextDelivery),
            "discord_text_send" => Ok(Self::DiscordTextSend),
            "discord_forum_thread_create" => Ok(Self::DiscordForumThreadCreate),
            "agent_session_start" => Ok(Self::AgentSessionStart),
            "agent_session_sunset" => Ok(Self::AgentSessionSunset),
            "agent_session_resume" => Ok(Self::AgentSessionResume),
            "agent_session_retirement" => Ok(Self::AgentSessionRetirement),
            "transcript_publication" => Ok(Self::TranscriptPublication),
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
            "voice_status_sync" => Ok(Self::VoiceStatusSync),
            "discord_voice_status_snapshot" => Ok(Self::DiscordVoiceStatusSnapshot),
            "automation_evaluation" => Ok(Self::AutomationEvaluation),
            "stale_wake_probe_sweep" => Ok(Self::StaleWakeProbeSweep),
            "stale_running_job_sweep" => Ok(Self::StaleRunningJobSweep),
            "ephemeral_job_gc" => Ok(Self::EphemeralJobGc),
            "discord_voice_deafen" => Ok(Self::DiscordVoiceDeafen),
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
            "failed_draft_retained" => Ok(Self::FailedDraftRetained),
            value => anyhow::bail!("unknown job state: {value}"),
        }
    }
}
