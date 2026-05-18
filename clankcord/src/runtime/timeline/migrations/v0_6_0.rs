use serde::{Deserialize, Serialize};

use crate::Result;
use crate::runtime::jobs::{
    AgentInvocationMetadata, AgentPreflightMetadata, AgentTaskMetadata, DiscordPostMetadata,
};
use crate::runtime::{
    BinaryPayload, Job, JobKind, JobOutput, JobPayload, JobState, RuntimeScopeKind,
};

const JOB_PAYLOAD_BLOB_MAGIC: &[u8; 8] = b"CLANKJOB";
const PRE_V0_6_0_JOB_PAYLOAD_BLOB_VERSION: u16 = 3;

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
struct PreV0_6_0AgentInvocationMetadata {
    session_id: String,
    provider: String,
    model: String,
    usage: BinaryPayload,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
struct PreV0_6_0AgentTaskMetadata {
    dispatch_attempts: i64,
    dispatch_error: String,
    dispatch_error_after_cancel: String,
    workdir_path: String,
    prompt_path: String,
    result_path: String,
    raw_result_path: String,
    dispatch_stdout_preview: String,
    dispatch_stderr: String,
    agent: PreV0_6_0AgentInvocationMetadata,
    preflight: Option<AgentPreflightMetadata>,
    response_text: String,
    command: String,
    result_suppressed: bool,
    discord_post: Option<DiscordPostMetadata>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
struct PreV0_6_0ConfirmationJobMetadata {
    delivery: String,
    channel_id: String,
    message_id: String,
    post_error: String,
    approved_by_user_id: String,
    approved_at: String,
    approval_error: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
enum PreV0_6_0JobMetadataDetail {
    AgentTask(PreV0_6_0AgentTaskMetadata),
    Confirmation(PreV0_6_0ConfirmationJobMetadata),
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
struct PreV0_6_0JobMetadata {
    detail: Option<Box<PreV0_6_0JobMetadataDetail>>,
    error: String,
    timed_out_at: String,
    cancel_requested: bool,
    cancelled_by_user_id: String,
    output: Option<JobOutput>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PreV0_6_0Job {
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
    metadata: PreV0_6_0JobMetadata,
}

pub(super) async fn run(transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>) -> Result<()> {
    let rows = sqlx::query(
        r#"
        SELECT job_id, payload_blob
        FROM job_payloads
        ORDER BY job_id
        "#,
    )
    .fetch_all(transaction.as_mut())
    .await?;
    for row in rows {
        let job_id: String = sqlx::Row::try_get(&row, "job_id")?;
        let payload_blob: Vec<u8> = sqlx::Row::try_get(&row, "payload_blob")?;
        if Job::is_current_payload_blob(&payload_blob) {
            continue;
        }
        let job = decode_pre_v0_6_0_job_payload_blob(&payload_blob).map_err(|error| {
            anyhow::anyhow!("migrating v0.6.0 job payload blob {job_id}: {error:#}")
        })?;
        sqlx::query(
            r#"
            UPDATE job_payloads
            SET payload_blob = $2
            WHERE job_id = $1
            "#,
        )
        .bind(&job.id)
        .bind(job.encode()?)
        .execute(transaction.as_mut())
        .await?;
    }
    Ok(())
}

fn decode_pre_v0_6_0_job_payload_blob(bytes: &[u8]) -> Result<Job> {
    let body = pre_v0_6_0_envelope_body(bytes)?;
    if let Ok(job) = bincode::deserialize::<Job>(body) {
        return Ok(job);
    }
    let previous: PreV0_6_0Job = bincode::deserialize(body)?;
    Ok(previous.into_current())
}

fn pre_v0_6_0_envelope_body(bytes: &[u8]) -> Result<&[u8]> {
    let header_len = JOB_PAYLOAD_BLOB_MAGIC.len() + std::mem::size_of::<u16>();
    if bytes.len() < header_len || &bytes[..JOB_PAYLOAD_BLOB_MAGIC.len()] != JOB_PAYLOAD_BLOB_MAGIC
    {
        anyhow::bail!("job payload blob is not a pre-v0.6.0 encoded job payload");
    }
    let version_offset = JOB_PAYLOAD_BLOB_MAGIC.len();
    let version = u16::from_le_bytes([bytes[version_offset], bytes[version_offset + 1]]);
    if version != PRE_V0_6_0_JOB_PAYLOAD_BLOB_VERSION {
        anyhow::bail!(
            "unsupported pre-v0.6.0 job payload blob version {version}; expected {PRE_V0_6_0_JOB_PAYLOAD_BLOB_VERSION}"
        );
    }
    Ok(&bytes[header_len..])
}

impl PreV0_6_0Job {
    fn into_current(self) -> Job {
        Job {
            id: self.id,
            kind: self.kind,
            scope_kind: self.scope_kind,
            guild_id: self.guild_id,
            scope_id: self.scope_id,
            state: self.state,
            requested_by_user_id: self.requested_by_user_id,
            payload: self.payload,
            attempts: self.attempts,
            created_at: self.created_at,
            updated_at: self.updated_at,
            next_run_at: self.next_run_at,
            started_at: self.started_at,
            completed_at: self.completed_at,
            cancelled_at: self.cancelled_at,
            parent_job_id: self.parent_job_id,
            root_job_id: self.root_job_id,
            lineage_depth: self.lineage_depth,
            metadata: self.metadata.into_current(),
        }
    }
}

impl PreV0_6_0JobMetadata {
    fn into_current(self) -> crate::runtime::jobs::JobMetadata {
        let mut metadata = crate::runtime::jobs::JobMetadata {
            error: self.error,
            timed_out_at: self.timed_out_at,
            cancel_requested: self.cancel_requested,
            cancelled_by_user_id: self.cancelled_by_user_id,
            output: self.output,
            ..crate::runtime::jobs::JobMetadata::default()
        };
        match self.detail.map(|detail| *detail) {
            Some(PreV0_6_0JobMetadataDetail::AgentTask(task)) => {
                metadata.set_agent_task(task.into_current());
            }
            Some(PreV0_6_0JobMetadataDetail::Confirmation(confirmation)) => {
                let current = metadata.confirmation_mut();
                current.delivery = confirmation.delivery;
                current.channel_id = confirmation.channel_id;
                current.message_id = confirmation.message_id;
                current.post_error = confirmation.post_error;
                current.approved_by_user_id = confirmation.approved_by_user_id;
                current.approved_at = confirmation.approved_at;
                current.approval_error = confirmation.approval_error;
            }
            None => {}
        }
        metadata
    }
}

impl PreV0_6_0AgentTaskMetadata {
    fn into_current(self) -> AgentTaskMetadata {
        AgentTaskMetadata {
            dispatch_attempts: self.dispatch_attempts,
            dispatch_error: self.dispatch_error,
            dispatch_error_after_cancel: self.dispatch_error_after_cancel,
            workdir_path: self.workdir_path,
            prompt_path: self.prompt_path,
            result_path: self.result_path,
            raw_result_path: self.raw_result_path,
            dispatch_stdout_preview: self.dispatch_stdout_preview,
            dispatch_stderr: self.dispatch_stderr,
            agent: self.agent.into_current(),
            preflight: self.preflight,
            response_text: self.response_text,
            command: self.command,
            result_suppressed: self.result_suppressed,
            discord_post: self.discord_post,
        }
    }
}

impl PreV0_6_0AgentInvocationMetadata {
    fn into_current(self) -> AgentInvocationMetadata {
        AgentInvocationMetadata {
            session_id: self.session_id,
            provider: self.provider,
            model: self.model,
            reasoning_effort: String::new(),
            fast_mode: false,
            usage: self.usage,
        }
    }
}
