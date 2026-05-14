mod kind;
mod output;
mod payload;
mod record;
mod util;

pub use kind::{JobKind, JobState};
pub use output::{
    DiscordVoiceJoinOutput, DiscordVoiceLeaveOutput, JobCreatedOutput, JobFailure, JobOutput,
    ResponseOutput, RoomAgentPlacementOutput, RuntimeControlOutput,
};
pub use payload::{
    AgentTaskPayload, AudioSegmentPayload, BinaryPayload, CommandAction, CommandArguments,
    CommandKind, CommandPayload, CommandRequest, ConfirmationContext, ConfirmationRequiredPayload,
    DiscordVoiceJoinPayload, DiscordVoiceLeavePayload, JobPayload, OpaqueValue,
    RefineTranscriptPayload, ResponseKind, ResponsePayload, ResponseSink, ResponseSinkKind,
    RoomAgentPlacementAction, RoomAgentPlacementPayload, RuntimeControlAction,
    RuntimeControlPayload, WakeActivationPayload,
};
pub use record::{Job, JobMetadata};

pub use record::{DiscordPostMetadata, DiscordPostedMessageMetadata};

pub(crate) use record::{
    AgentInvocationMetadata, AgentPreflightCheck, AgentPreflightMetadata, AgentTaskMetadata,
};
