pub mod agents;
pub mod automations;
pub(crate) mod core;
pub mod domain;
pub mod jobs;
pub mod refinement;
pub mod rooms;
pub mod runtime_config;
pub mod service;
pub mod timeline;
pub(crate) mod util;
pub mod views;

pub use agents::{
    AgentRuntime, AgentSessionRecord, AgentSessionRecordState, AgentSessionRouteKind, dm_route_key,
    thread_route_key, voice_route_key,
};
pub use core::Runtime;
pub use domain::voice::{
    ArtifactStatus, SessionArtifacts, SessionCaptureStats, SessionSpeakerCaptureStats,
    VoiceBotStatus, VoiceCaptureSessionStatus,
};
pub use jobs::{
    AgentSessionStartOutput, AgentSessionStartPayload, AgentTaskPayload, AudioSegmentPayload,
    BinaryPayload, CommandAction, CommandArguments, CommandKind, CommandPayload, CommandRequest,
    ConfirmationContext, ConfirmationRequiredPayload, DiscordForumThreadCreateOutput,
    DiscordForumThreadCreatePayload, DiscordSlashCommandPayload, DiscordTextMessagePayload,
    DiscordTextSendOutput, DiscordTextSendPayload, DiscordVoiceJoinOutput, DiscordVoiceJoinPayload,
    DiscordVoiceLeaveOutput, DiscordVoiceLeavePayload, DiscordVoiceMuteOutput,
    DiscordVoiceMutePayload, DiscordVoicePlayAudioOutput, DiscordVoicePlayAudioPayload,
    DiscordVoicePlaybackCue, DiscordVoicePlaybackOutput, DiscordVoicePlaybackPayload, Job,
    JobCreatedOutput, JobFailure, JobKind, JobOutput, JobPayload, JobState,
    RefineTranscriptPayload, RoomAgentPlacementAction, RoomAgentPlacementOutput,
    RoomAgentPlacementPayload, RuntimeControlAction, RuntimeControlOutput, RuntimeControlPayload,
    RuntimeMaintenancePayload, TextDeliveryKind, TextDeliveryOutput, TextDeliveryPayload,
    TextTarget, TextTargetKind, TranscriptPublicationOutput, TranscriptPublicationPayload,
    WakeActivationPayload, WakeProbePayload,
};
pub use rooms::{RoomConfig, RoomControl};
pub use runtime_config::{ControlConfig, GuildConfig};
pub use service::{
    RuntimeHandle, RuntimeJobSink, RuntimeService, start_blocking, start_persistent_process,
};
pub use util::{duration_to_seconds, log};
pub use views::{
    ContextResolveRequest, DebugOverviewRequest, ForgetRequest, JobsRequest,
    ListConversationsRequest, MaterializeTranscriptRequest, MemberGetRequest, MemberResolveRequest,
    MemberSearchRequest, ParticipantTraceRequest, RenderTranscriptRequest,
    SearchTranscriptsRequest, TimelineRangeRequest, TimelineTailRequest,
};
