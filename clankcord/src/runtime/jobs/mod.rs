mod kind;
mod output;
mod payload;
mod record;
mod util;

pub use kind::{JobKind, JobState};
pub use output::{
    AgentSessionStartOutput, DiscordForumThreadCreateOutput, DiscordTextSendOutput,
    DiscordVoiceDeafenOutput, DiscordVoiceJoinOutput, DiscordVoiceLeaveOutput,
    DiscordVoiceMuteOutput, DiscordVoicePlayAudioOutput, DiscordVoicePlaybackOutput,
    DiscordVoiceStatusSnapshotOutput, JobCreatedOutput, JobFailure, JobOutput,
    RoomAgentPlacementOutput, RuntimeControlOutput, TextDeliveryOutput,
    TranscriptPublicationOutput,
};
pub use payload::{
    AgentSessionResumePayload, AgentSessionRetirementPayload, AgentSessionStartPayload,
    AgentSessionSunsetPayload, AgentTaskPayload, AudioSegmentPayload, AutomationEvaluationPayload,
    BinaryPayload, CommandAction, CommandArguments, CommandKind, CommandPayload, CommandRequest,
    ConfirmationContext, ConfirmationRequiredPayload, DiscordForumThreadCreatePayload,
    DiscordSlashCommandPayload, DiscordTextMessagePayload, DiscordTextSendPayload,
    DiscordVoiceDeafenPayload, DiscordVoiceJoinPayload, DiscordVoiceLeavePayload,
    DiscordVoiceMutePayload, DiscordVoicePlayAudioPayload, DiscordVoicePlaybackCue,
    DiscordVoicePlaybackPayload, DiscordVoiceStatusSnapshotPayload, EphemeralJobGcPayload,
    JobPayload, OpaqueValue, RefineTranscriptPayload, RoomAgentPlacementAction,
    RoomAgentPlacementPayload, RuntimeControlAction, RuntimeControlPayload,
    RuntimeMaintenancePayload, StaleRunningJobSweepPayload, StaleWakeProbeSweepPayload,
    TextDeliveryKind, TextDeliveryPayload, TextTarget, TextTargetKind,
    TranscriptPublicationPayload, VoiceStatusSyncPayload, WakeActivationPayload, WakeProbePayload,
};
pub use record::{Job, JobMetadata};

pub use record::{DiscordPostMetadata, DiscordPostedMessageMetadata};

pub(crate) use record::{
    AgentInvocationMetadata, AgentPreflightCheck, AgentPreflightMetadata, AgentTaskMetadata,
};
