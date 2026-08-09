//! Frozen wire-format mirrors of current and pre-v1 job blobs, shared by
//! integrity and migration tests. Do not evolve these with src/ types.

use serde::{Deserialize, Serialize};

use clankcord::model::job::DiscordPostMetadata;
use clankcord::model::job::JobMetadata;
use clankcord::model::job::{
    BinaryPayload, Job, JobKind, JobOutput, JobPayload, JobState, TextDeliveryKind, TextTarget,
};
use clankcord::model::scope::RuntimeScopeKind;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreV0_3_0Job {
    pub id: String,
    pub kind: JobKind,
    pub guild_id: String,
    pub voice_channel_id: String,
    pub state: JobState,
    pub requested_by_user_id: String,
    pub payload: JobPayload,
    pub attempts: i64,
    pub created_at: String,
    pub updated_at: String,
    pub next_run_at: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub cancelled_at: Option<String>,
    pub parent_job_id: Option<String>,
    pub root_job_id: String,
    pub lineage_depth: u8,
    pub metadata: JobMetadata,
}
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PreV0_6_0AgentInvocationMetadata {
    pub session_id: String,
    pub provider: String,
    pub model: String,
    pub usage: BinaryPayload,
}
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PreV0_6_0AgentPreflightCheck {
    pub command: String,
    pub returncode: Option<i32>,
    pub ok: bool,
    pub stdout_preview: String,
    pub stderr_preview: String,
    pub error: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PreV0_6_0AgentPreflightMetadata {
    pub ok: bool,
    pub checked_at: String,
    pub checks: Vec<PreV0_6_0AgentPreflightCheck>,
}
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PreV0_6_0AgentTaskMetadata {
    pub dispatch_attempts: i64,
    pub dispatch_error: String,
    pub dispatch_error_after_cancel: String,
    pub workdir_path: String,
    pub prompt_path: String,
    pub result_path: String,
    pub raw_result_path: String,
    pub dispatch_stdout_preview: String,
    pub dispatch_stderr: String,
    pub agent: PreV0_6_0AgentInvocationMetadata,
    pub preflight: Option<PreV0_6_0AgentPreflightMetadata>,
    pub response_text: String,
    pub command: String,
    pub result_suppressed: bool,
    pub discord_post: Option<DiscordPostMetadata>,
}
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PreV0_6_0ConfirmationJobMetadata {
    pub delivery: String,
    pub channel_id: String,
    pub message_id: String,
    pub post_error: String,
    pub approved_by_user_id: String,
    pub approved_at: String,
    pub approval_error: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[allow(clippy::large_enum_variant)] // wire/decision enums: boxing buys nothing on the encoded form
pub enum PreV0_6_0JobMetadataDetail {
    AgentTask(PreV0_6_0AgentTaskMetadata),
    Confirmation(PreV0_6_0ConfirmationJobMetadata),
}
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PreV0_6_0JobMetadata {
    pub detail: Option<Box<PreV0_6_0JobMetadataDetail>>,
    pub error: String,
    pub timed_out_at: String,
    pub cancel_requested: bool,
    pub cancelled_by_user_id: String,
    pub output: Option<JobOutput>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreV0_6_0Job {
    pub id: String,
    pub kind: JobKind,
    pub scope_kind: RuntimeScopeKind,
    pub guild_id: String,
    pub scope_id: String,
    pub state: JobState,
    pub requested_by_user_id: String,
    pub payload: JobPayload,
    pub attempts: i64,
    pub created_at: String,
    pub updated_at: String,
    pub next_run_at: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub cancelled_at: Option<String>,
    pub parent_job_id: Option<String>,
    pub root_job_id: String,
    pub lineage_depth: u8,
    pub metadata: PreV0_6_0JobMetadata,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreV0_7_0TextDeliveryPayload {
    pub intent: TextDeliveryKind,
    pub target: TextTarget,
    pub content: String,
    pub source_job_id: String,
    pub requested_by_user_id: String,
    pub expects_reply: bool,
    pub opaque: BinaryPayload,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreV0_7_0DiscordTextSendPayload {
    pub intent: TextDeliveryKind,
    pub target: TextTarget,
    pub content: String,
    pub source_job_id: String,
    pub requested_by_user_id: String,
    pub allowed_mentions: BinaryPayload,
    pub components: BinaryPayload,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PreV0_7_0JobPayload {
    AudioSegment,
    WakeActivation,
    AgentTask,
    DiscordTextMessage,
    DiscordSlashCommand,
    TextDelivery(PreV0_7_0TextDeliveryPayload),
    DiscordTextSend(PreV0_7_0DiscordTextSendPayload),
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreV0_7_0Job {
    pub id: String,
    pub kind: JobKind,
    pub scope_kind: RuntimeScopeKind,
    pub guild_id: String,
    pub scope_id: String,
    pub state: JobState,
    pub requested_by_user_id: String,
    pub payload: PreV0_7_0JobPayload,
    pub attempts: i64,
    pub created_at: String,
    pub updated_at: String,
    pub next_run_at: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub cancelled_at: Option<String>,
    pub parent_job_id: Option<String>,
    pub root_job_id: String,
    pub lineage_depth: u8,
    pub metadata: JobMetadata,
}
#[derive(Debug, Clone, Serialize)]
pub struct EncodedCurrentAgentPreflightCheck {
    pub command: String,
    pub returncode: Option<i32>,
    pub ok: bool,
    pub stdout_preview: String,
    pub stderr_preview: String,
    pub error: String,
}
#[derive(Debug, Clone, Serialize)]
pub struct EncodedCurrentAgentPreflightMetadata {
    pub ok: bool,
    pub checked_at: String,
    pub checks: Vec<EncodedCurrentAgentPreflightCheck>,
}
#[derive(Debug, Clone, Default, Serialize)]
pub struct EncodedCurrentAgentInvocationMetadata {
    pub session_id: String,
    pub provider: String,
    pub model: String,
    pub reasoning_effort: String,
    pub fast_mode: bool,
    pub usage: BinaryPayload,
}
#[derive(Debug, Clone, Serialize)]
pub struct EncodedCurrentAgentTaskMetadata {
    pub outcome: EncodedCurrentAgentTaskOutcome,
    pub phase: EncodedCurrentAgentTaskPhase,
    pub await_delivery_until: String,
    pub dispatch_attempts: i64,
    pub dispatch_error: String,
    pub dispatch_error_after_cancel: String,
    pub workdir_path: String,
    pub prompt_path: String,
    pub result_path: String,
    pub raw_result_path: String,
    pub dispatch_stdout_preview: String,
    pub dispatch_stderr: String,
    pub agent: EncodedCurrentAgentInvocationMetadata,
    pub preflight: Option<EncodedCurrentAgentPreflightMetadata>,
    pub response_text: String,
    pub command: String,
    pub result_suppressed: bool,
    pub discord_post: Option<DiscordPostMetadata>,
}
#[derive(Debug, Clone, Copy, Serialize)]
pub enum EncodedCurrentAgentTaskOutcome {
    Pending,
}
#[derive(Debug, Clone, Copy, Serialize)]
pub enum EncodedCurrentAgentTaskPhase {
    #[allow(dead_code)]
    Dispatch,
    AwaitDelivery,
}
#[derive(Debug, Clone, Serialize)]
pub enum EncodedCurrentJobMetadataDetail {
    AgentTask(EncodedCurrentAgentTaskMetadata),
}
#[derive(Debug, Clone, Serialize)]
pub struct EncodedCurrentJobMetadata {
    pub detail: Option<Box<EncodedCurrentJobMetadataDetail>>,
    pub error: String,
    pub timed_out_at: String,
    pub cancel_requested: bool,
    pub cancelled_by_user_id: String,
    pub output: Option<JobOutput>,
}
#[derive(Debug, Clone, Serialize)]
pub struct EncodedCurrentJob {
    pub id: String,
    pub kind: JobKind,
    pub scope_kind: RuntimeScopeKind,
    pub guild_id: String,
    pub scope_id: String,
    pub state: JobState,
    pub requested_by_user_id: String,
    pub payload: JobPayload,
    pub attempts: i64,
    pub created_at: String,
    pub updated_at: String,
    pub next_run_at: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub cancelled_at: Option<String>,
    pub parent_job_id: Option<String>,
    pub root_job_id: String,
    pub lineage_depth: u8,
    pub metadata: EncodedCurrentJobMetadata,
}
pub fn encode_pre_v0_3_0_job(job: &Job) -> Vec<u8> {
    let previous = PreV0_3_0Job {
        id: job.id.clone(),
        kind: job.kind,
        guild_id: job.guild_id.clone(),
        voice_channel_id: job.scope_id.clone(),
        state: job.state,
        requested_by_user_id: job.requested_by_user_id.clone(),
        payload: job.payload.clone(),
        attempts: job.attempts,
        created_at: job.created_at.clone(),
        updated_at: job.updated_at.clone(),
        next_run_at: job.next_run_at.clone(),
        started_at: job.started_at.clone(),
        completed_at: job.completed_at.clone(),
        cancelled_at: job.cancelled_at.clone(),
        parent_job_id: job.parent_job_id.clone(),
        root_job_id: job.root_job_id.clone(),
        lineage_depth: job.lineage_depth,
        metadata: job.metadata.clone(),
    };
    let body = bincode::serialize(&previous).unwrap();
    let mut bytes = Vec::with_capacity(10 + body.len());
    bytes.extend_from_slice(b"CLANKJOB");
    bytes.extend_from_slice(&2_u16.to_le_bytes());
    bytes.extend_from_slice(&body);
    bytes
}
pub fn encode_pre_v0_6_0_agent_task_job(job: &Job) -> Vec<u8> {
    let previous = PreV0_6_0Job {
        id: job.id.clone(),
        kind: job.kind,
        scope_kind: job.scope_kind,
        guild_id: job.guild_id.clone(),
        scope_id: job.scope_id.clone(),
        state: job.state,
        requested_by_user_id: job.requested_by_user_id.clone(),
        payload: job.payload.clone(),
        attempts: job.attempts,
        created_at: job.created_at.clone(),
        updated_at: job.updated_at.clone(),
        next_run_at: job.next_run_at.clone(),
        started_at: job.started_at.clone(),
        completed_at: job.completed_at.clone(),
        cancelled_at: job.cancelled_at.clone(),
        parent_job_id: job.parent_job_id.clone(),
        root_job_id: job.root_job_id.clone(),
        lineage_depth: job.lineage_depth,
        metadata: PreV0_6_0JobMetadata {
            detail: Some(Box::new(PreV0_6_0JobMetadataDetail::AgentTask(
                PreV0_6_0AgentTaskMetadata {
                    dispatch_stdout_preview: "done".to_string(),
                    agent: PreV0_6_0AgentInvocationMetadata {
                        session_id: "codex-session-v3".to_string(),
                        provider: "codex".to_string(),
                        model: "codex-default".to_string(),
                        usage: BinaryPayload::empty(),
                    },
                    response_text: "done".to_string(),
                    ..PreV0_6_0AgentTaskMetadata::default()
                },
            ))),
            ..PreV0_6_0JobMetadata::default()
        },
    };
    let body = bincode::serialize(&previous).unwrap();
    let mut bytes = Vec::with_capacity(10 + body.len());
    bytes.extend_from_slice(b"CLANKJOB");
    bytes.extend_from_slice(&3_u16.to_le_bytes());
    bytes.extend_from_slice(&body);
    bytes
}
pub fn encode_pre_v0_7_0_text_delivery_job(job: &Job, opaque: BinaryPayload) -> Vec<u8> {
    let JobPayload::TextDelivery(payload) = &job.payload else {
        panic!("expected text delivery payload");
    };
    let previous = PreV0_7_0Job {
        id: job.id.clone(),
        kind: job.kind,
        scope_kind: job.scope_kind,
        guild_id: job.guild_id.clone(),
        scope_id: job.scope_id.clone(),
        state: job.state,
        requested_by_user_id: job.requested_by_user_id.clone(),
        payload: PreV0_7_0JobPayload::TextDelivery(PreV0_7_0TextDeliveryPayload {
            intent: payload.intent,
            target: payload.target.clone(),
            content: payload.content.clone(),
            source_job_id: payload.source_job_id.clone(),
            requested_by_user_id: payload.requested_by_user_id.clone(),
            expects_reply: payload.expects_reply,
            opaque,
        }),
        attempts: job.attempts,
        created_at: job.created_at.clone(),
        updated_at: job.updated_at.clone(),
        next_run_at: job.next_run_at.clone(),
        started_at: job.started_at.clone(),
        completed_at: job.completed_at.clone(),
        cancelled_at: job.cancelled_at.clone(),
        parent_job_id: job.parent_job_id.clone(),
        root_job_id: job.root_job_id.clone(),
        lineage_depth: job.lineage_depth,
        metadata: job.metadata.clone(),
    };
    let body = bincode::serialize(&previous).unwrap();
    let mut bytes = Vec::with_capacity(10 + body.len());
    bytes.extend_from_slice(b"CLANKJOB");
    bytes.extend_from_slice(&4_u16.to_le_bytes());
    bytes.extend_from_slice(&body);
    bytes
}
pub fn encode_pre_v0_10_0_voice_status_snapshot_job(job: &Job) -> Vec<u8> {
    #[derive(Debug, Clone, Serialize)]
    struct PreV0_10_0VoiceStatusSnapshotOutput {
        bots: Vec<()>,
        sessions: Vec<()>,
    }

    #[derive(Debug, Clone, Serialize)]
    #[allow(dead_code)]
    enum PreV0_10_0JobOutput {
        Empty,
        JobCreated,
        RuntimeControl,
        TextDelivery,
        DiscordTextSend,
        DiscordForumThreadCreate,
        DiscordForumThreadRename,
        AgentSessionStart,
        TranscriptPublication,
        RoomAgentPlacement,
        DiscordVoiceJoin,
        DiscordVoiceLeave,
        DiscordVoicePlayback,
        DiscordVoiceMute,
        DiscordVoicePlayAudio,
        DiscordVoiceStatusSnapshot(PreV0_10_0VoiceStatusSnapshotOutput),
        Record,
        DiscordVoiceDeafen,
        DiscordTypingIndicator,
    }

    #[derive(Debug, Clone, Serialize)]
    struct PreV0_10_0JobMetadata {
        detail: Option<Box<()>>,
        error: String,
        timed_out_at: String,
        cancel_requested: bool,
        cancelled_by_user_id: String,
        output: Option<PreV0_10_0JobOutput>,
    }

    #[derive(Debug, Clone, Serialize)]
    struct PreV0_10_0Job {
        id: String,
        kind: JobKind,
        scope_kind: RuntimeScopeKind,
        guild_id: String,
        scope_id: String,
        state: JobState,
        requested_by_user_id: String,
        payload: JobPayload,
        attempts: i64,
        created_at: String,
        updated_at: String,
        next_run_at: Option<String>,
        started_at: Option<String>,
        completed_at: Option<String>,
        cancelled_at: Option<String>,
        parent_job_id: Option<String>,
        root_job_id: String,
        lineage_depth: u8,
        metadata: PreV0_10_0JobMetadata,
    }

    let previous = PreV0_10_0Job {
        id: job.id.clone(),
        kind: job.kind,
        scope_kind: job.scope_kind,
        guild_id: job.guild_id.clone(),
        scope_id: job.scope_id.clone(),
        state: job.state,
        requested_by_user_id: job.requested_by_user_id.clone(),
        payload: job.payload.clone(),
        attempts: job.attempts,
        created_at: job.created_at.clone(),
        updated_at: job.updated_at.clone(),
        next_run_at: job.next_run_at.clone(),
        started_at: job.started_at.clone(),
        completed_at: job.completed_at.clone(),
        cancelled_at: job.cancelled_at.clone(),
        parent_job_id: job.parent_job_id.clone(),
        root_job_id: job.root_job_id.clone(),
        lineage_depth: job.lineage_depth,
        metadata: PreV0_10_0JobMetadata {
            detail: None,
            error: job.metadata.error.clone(),
            timed_out_at: job.metadata.timed_out_at.clone(),
            cancel_requested: job.metadata.cancel_requested,
            cancelled_by_user_id: job.metadata.cancelled_by_user_id.clone(),
            output: Some(PreV0_10_0JobOutput::DiscordVoiceStatusSnapshot(
                PreV0_10_0VoiceStatusSnapshotOutput {
                    bots: Vec::new(),
                    sessions: Vec::new(),
                },
            )),
        },
    };
    let body = bincode::serialize(&previous).unwrap();
    let mut bytes = Vec::with_capacity(10 + body.len());
    bytes.extend_from_slice(b"CLANKJOB");
    bytes.extend_from_slice(&7_u16.to_le_bytes());
    bytes.extend_from_slice(&body);
    bytes
}
pub fn encode_job_with_blob_version(job: &Job, version: u16) -> Vec<u8> {
    let body = bincode::serialize(job).unwrap();
    let mut bytes = Vec::with_capacity(10 + body.len());
    bytes.extend_from_slice(b"CLANKJOB");
    bytes.extend_from_slice(&version.to_le_bytes());
    bytes.extend_from_slice(&body);
    bytes
}
pub fn encode_current_agent_task(
    job: &Job,
    response_text: &str,
    command: &str,
    dispatch_error: &str,
) -> Vec<u8> {
    let encoded = EncodedCurrentJob {
        id: job.id.clone(),
        kind: job.kind,
        scope_kind: job.scope_kind,
        guild_id: job.guild_id.clone(),
        scope_id: job.scope_id.clone(),
        state: job.state,
        requested_by_user_id: job.requested_by_user_id.clone(),
        payload: job.payload.clone(),
        attempts: job.attempts,
        created_at: job.created_at.clone(),
        updated_at: job.updated_at.clone(),
        next_run_at: job.next_run_at.clone(),
        started_at: job.started_at.clone(),
        completed_at: job.completed_at.clone(),
        cancelled_at: job.cancelled_at.clone(),
        parent_job_id: job.parent_job_id.clone(),
        root_job_id: job.root_job_id.clone(),
        lineage_depth: job.lineage_depth,
        metadata: EncodedCurrentJobMetadata {
            detail: Some(Box::new(EncodedCurrentJobMetadataDetail::AgentTask(
                EncodedCurrentAgentTaskMetadata {
                    outcome: EncodedCurrentAgentTaskOutcome::Pending,
                    phase: EncodedCurrentAgentTaskPhase::AwaitDelivery,
                    await_delivery_until: "2020-01-01T00:00:00.000Z".to_string(),
                    dispatch_attempts: 0,
                    dispatch_error: dispatch_error.to_string(),
                    dispatch_error_after_cancel: String::new(),
                    workdir_path: "/tmp/clankcord-agent-workdir".to_string(),
                    prompt_path: "/tmp/clankcord-agent-prompt.txt".to_string(),
                    result_path: "/tmp/clankcord-agent-result.txt".to_string(),
                    raw_result_path: "/tmp/clankcord-agent.codex.jsonl".to_string(),
                    dispatch_stdout_preview: response_text.to_string(),
                    dispatch_stderr: String::new(),
                    agent: EncodedCurrentAgentInvocationMetadata {
                        session_id: "codex-session".to_string(),
                        provider: "codex".to_string(),
                        model: "gpt-test".to_string(),
                        reasoning_effort: "medium".to_string(),
                        fast_mode: false,
                        usage: BinaryPayload::empty(),
                    },
                    preflight: Some(EncodedCurrentAgentPreflightMetadata {
                        ok: true,
                        checked_at: "2026-05-20T00:00:00.000Z".to_string(),
                        checks: Vec::new(),
                    }),
                    response_text: response_text.to_string(),
                    command: command.to_string(),
                    result_suppressed: false,
                    discord_post: None,
                },
            ))),
            error: String::new(),
            timed_out_at: String::new(),
            cancel_requested: false,
            cancelled_by_user_id: String::new(),
            output: None,
        },
    };
    let body = bincode::serialize(&encoded).unwrap();
    let mut bytes = Vec::with_capacity(10 + body.len());
    bytes.extend_from_slice(b"CLANKJOB");
    bytes.extend_from_slice(&9_u16.to_le_bytes());
    bytes.extend_from_slice(&body);
    bytes
}
