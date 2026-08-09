use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::Result;

/// One declaration per kind. The macro generates the enum, the string
/// mirrors, and `ALL` together so they cannot drift; per-kind policy lives in
/// the exhaustive match in [`super::spec::spec`].
macro_rules! job_kinds {
    ($(($variant:ident, $name:literal)),* $(,)?) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        pub enum JobKind {
            $($variant),*
        }

        impl JobKind {
            pub const ALL: &'static [JobKind] = &[$(JobKind::$variant),*];

            pub fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $name),*
                }
            }
        }

        impl FromStr for JobKind {
            type Err = anyhow::Error;

            fn from_str(raw: &str) -> Result<Self> {
                match raw.trim() {
                    $($name => Ok(Self::$variant),)*
                    value => anyhow::bail!("unknown job kind: {value}"),
                }
            }
        }
    };
}

job_kinds! {
    (AudioSegment, "audio_segment"),
    (WakeActivation, "wake_activation"),
    (AgentTask, "agent_task"),
    (DiscordTextMessage, "discord_text_message"),
    (DiscordSlashCommand, "discord_slash_command"),
    (TextDelivery, "text_delivery"),
    (DiscordTextSend, "discord_text_send"),
    (DiscordForumThreadCreate, "discord_forum_thread_create"),
    (DiscordForumThreadRename, "discord_forum_thread_rename"),
    (AgentSessionStart, "agent_session_start"),
    (AgentSessionSunset, "agent_session_sunset"),
    (AgentSessionResume, "agent_session_resume"),
    (AgentSessionRetirement, "agent_session_retirement"),
    (AgentThreadTitleRefresh, "agent_thread_title_refresh"),
    (TranscriptPublication, "transcript_publication"),
    (ConfirmationRequired, "confirmation_required"),
    (Command, "command"),
    (RoomAgentPlacement, "room_agent_placement"),
    (DiscordVoiceJoin, "discord_voice_join"),
    (DiscordVoiceLeave, "discord_voice_leave"),
    (DiscordVoicePlayback, "discord_voice_playback"),
    (DiscordVoiceMute, "discord_voice_mute"),
    (DiscordVoicePlayAudio, "discord_voice_play_audio"),
    (RuntimeControl, "runtime_control"),
    (WakeProbe, "wake_probe"),
    (RuntimeMaintenance, "runtime_maintenance"),
    (VoiceStatusSync, "voice_status_sync"),
    (DiscordVoiceStatusSnapshot, "discord_voice_status_snapshot"),
    (AutomationEvaluation, "automation_evaluation"),
    (StaleWakeProbeSweep, "stale_wake_probe_sweep"),
    (StaleRunningJobSweep, "stale_running_job_sweep"),
    (EphemeralJobGc, "ephemeral_job_gc"),
    (DiscordVoiceDeafen, "discord_voice_deafen"),
    (DiscordTypingIndicator, "discord_typing_indicator"),
    (TranscriptionMux, "transcription_mux"),
    (TranscriptionMuxPlan, "transcription_mux_plan"),
}

impl JobKind {
    pub fn is_agent_task(self) -> bool {
        matches!(self, Self::AgentTask)
    }

    pub fn is_ephemeral(self) -> bool {
        super::spec::spec(self).ephemeral
    }
}

impl fmt::Display for JobKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
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
