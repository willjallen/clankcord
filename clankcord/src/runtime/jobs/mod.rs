mod kind;
mod output;
mod payload;
mod record;
mod util;

pub use kind::{JobKind, JobState};
pub use output::{
    AgentSessionStartOutput, DiscordForumThreadCreateOutput, DiscordForumThreadRenameOutput,
    DiscordTextSendOutput, DiscordTypingIndicatorOutput, DiscordVoiceDeafenOutput,
    DiscordVoiceJoinOutput, DiscordVoiceLeaveOutput, DiscordVoiceMuteOutput,
    DiscordVoicePlayAudioOutput, DiscordVoicePlaybackOutput, DiscordVoiceStatusSnapshotOutput,
    JobCreatedOutput, JobFailure, JobOutput, RoomAgentPlacementOutput, RuntimeControlOutput,
    TextDeliveryOutput, TranscriptPublicationOutput,
};
pub use payload::{
    AgentSessionResumePayload, AgentSessionRetirementPayload, AgentSessionStartPayload,
    AgentSessionSunsetPayload, AgentTaskPayload, AgentThreadTitleRefreshPayload,
    AudioSegmentPayload, AutomationEvaluationPayload, BinaryPayload, CommandAction,
    CommandArguments, CommandKind, CommandPayload, CommandRequest, ConfirmationContext,
    ConfirmationRequiredPayload, DiscordForumThreadCreatePayload, DiscordForumThreadRenamePayload,
    DiscordSlashCommandPayload, DiscordTextMessagePayload, DiscordTextSendPayload,
    DiscordTypingAction, DiscordTypingIndicatorPayload, DiscordVoiceDeafenPayload,
    DiscordVoiceJoinPayload, DiscordVoiceLeavePayload, DiscordVoiceMutePayload,
    DiscordVoicePlayAudioPayload, DiscordVoicePlaybackCue, DiscordVoicePlaybackPayload,
    DiscordVoiceStatusSnapshotPayload, EphemeralJobGcPayload, JobPayload, OpaqueValue,
    RoomAgentPlacementAction, RoomAgentPlacementPayload, RuntimeControlAction,
    RuntimeControlPayload, RuntimeMaintenancePayload, StaleRunningJobSweepPayload,
    StaleWakeProbeSweepPayload, TextAttachmentPayload, TextDeliveryKind, TextDeliveryPayload,
    TextTarget, TextTargetKind, TranscriptPublicationPayload, TranscriptionMuxPayload,
    TranscriptionMuxPlanPayload, VoiceStatusSyncPayload, WakeActivationPayload, WakeProbePayload,
};
pub use record::{Job, JobMetadata};

pub use record::{DiscordPostMetadata, DiscordPostedMessageMetadata};

pub(crate) use record::{
    AgentInvocationMetadata, AgentPreflightCheck, AgentPreflightMetadata, AgentTaskMetadata,
};
