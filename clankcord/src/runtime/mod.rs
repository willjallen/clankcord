pub mod agents;
pub mod automations;
pub mod coding;
pub(crate) mod core;
pub mod domain;
pub mod jobs;
pub mod rooms;
pub mod scope;
pub mod service;
pub mod timeline;
pub(crate) mod util;

pub use crate::config::{ControlConfig, GuildConfig};
pub use agents::{
    AgentRuntime, AgentSessionRecord, AgentSessionRecordState, AgentSessionRouteKind, dm_route_key,
    thread_route_key, voice_route_key,
};
pub use core::Runtime;
pub use domain::voice::{
    ArtifactStatus, SessionArtifacts, SessionCaptureStats, SessionSpeakerCaptureStats,
    VoiceAssignment, VoiceBotStatus, VoiceCaptureSessionStatus,
};
pub use jobs::{
    AgentSessionResumePayload, AgentSessionRetirementPayload, AgentSessionStartOutput,
    AgentSessionStartPayload, AgentSessionSunsetPayload, AgentTaskPayload,
    AgentThreadTitleRefreshPayload, AudioSegmentPayload, BinaryPayload, CommandAction,
    CommandArguments, CommandKind, CommandPayload, CommandRequest, ConfirmationContext,
    ConfirmationRequiredPayload, DiscordForumThreadCreateOutput, DiscordForumThreadCreatePayload,
    DiscordForumThreadRenameOutput, DiscordForumThreadRenamePayload, DiscordSlashCommandPayload,
    DiscordTextMessagePayload, DiscordTextSendOutput, DiscordTextSendPayload, DiscordTypingAction,
    DiscordTypingIndicatorOutput, DiscordTypingIndicatorPayload, DiscordVoiceDeafenOutput,
    DiscordVoiceDeafenPayload, DiscordVoiceJoinOutput, DiscordVoiceJoinPayload,
    DiscordVoiceLeaveOutput, DiscordVoiceLeavePayload, DiscordVoiceMuteOutput,
    DiscordVoiceMutePayload, DiscordVoicePlayAudioOutput, DiscordVoicePlayAudioPayload,
    DiscordVoicePlaybackCue, DiscordVoicePlaybackOutput, DiscordVoicePlaybackPayload,
    DiscordVoiceStatusSnapshotOutput, DiscordVoiceStatusSnapshotPayload, EphemeralJobGcPayload,
    Job, JobCreatedOutput, JobFailure, JobKind, JobOutput, JobPayload, JobState,
    RoomAgentPlacementAction, RoomAgentPlacementOutput, RoomAgentPlacementPayload,
    RuntimeControlAction, RuntimeControlOutput, RuntimeControlPayload, RuntimeMaintenancePayload,
    StaleRunningJobSweepPayload, StaleWakeProbeSweepPayload, TextAttachmentPayload,
    TextDeliveryKind, TextDeliveryOutput, TextDeliveryPayload, TextTarget, TextTargetKind,
    TranscriptPublicationOutput, TranscriptPublicationPayload, TranscriptionMuxPayload,
    TranscriptionMuxPlanPayload, VoiceStatusSyncPayload, WakeActivationPayload, WakeProbePayload,
};
pub use rooms::{RoomConfig, RoomControl};
pub use scope::{RuntimeScope, RuntimeScopeKind};
pub use service::{
    RuntimeHandle, RuntimeJobSink, RuntimeService, start_blocking, start_persistent_process,
};
pub use timeline::views::{
    ContextResolveRequest, DebugOverviewRequest, ForgetRequest, JobsRequest,
    ListConversationsRequest, MaterializeTranscriptRequest, MemberGetRequest, MemberResolveRequest,
    MemberSearchRequest, ParticipantTraceRequest, RenderTranscriptRequest,
    SearchTranscriptsRequest, TimelineRangeRequest, TimelineTailRequest,
};
pub use util::log;
