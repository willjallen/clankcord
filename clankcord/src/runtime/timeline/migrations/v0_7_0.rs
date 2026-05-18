use serde::{Deserialize, Serialize};

use crate::Result;
use crate::runtime::jobs::JobMetadata;
use crate::runtime::{Job, JobKind, JobState, RuntimeScopeKind};

use super::job_payload_pre_v0_7::PreV0_7_0JobPayload;

const JOB_PAYLOAD_BLOB_MAGIC: &[u8; 8] = b"CLANKJOB";
const PRE_V0_7_0_JOB_PAYLOAD_BLOB_VERSION: u16 = 4;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PreV0_7_0Job {
    id: String,
    kind: JobKind,
    scope_kind: RuntimeScopeKind,
    guild_id: String,
    scope_id: String,
    state: JobState,
    requested_by_user_id: String,
    payload: PreV0_7_0JobPayload,
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
    metadata: JobMetadata,
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
        let job = decode_pre_v0_7_0_job_payload_blob(&payload_blob).map_err(|error| {
            anyhow::anyhow!("migrating v0.7.0 job payload blob {job_id}: {error:#}")
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

fn decode_pre_v0_7_0_job_payload_blob(bytes: &[u8]) -> Result<Job> {
    let body = pre_v0_7_0_envelope_body(bytes)?;
    let previous: PreV0_7_0Job = bincode::deserialize(body)?;
    previous.into_current()
}

fn pre_v0_7_0_envelope_body(bytes: &[u8]) -> Result<&[u8]> {
    let header_len = JOB_PAYLOAD_BLOB_MAGIC.len() + std::mem::size_of::<u16>();
    if bytes.len() < header_len || &bytes[..JOB_PAYLOAD_BLOB_MAGIC.len()] != JOB_PAYLOAD_BLOB_MAGIC
    {
        anyhow::bail!("job payload blob is not a pre-v0.7.0 encoded job payload");
    }
    let version_offset = JOB_PAYLOAD_BLOB_MAGIC.len();
    let version = u16::from_le_bytes([bytes[version_offset], bytes[version_offset + 1]]);
    if version != PRE_V0_7_0_JOB_PAYLOAD_BLOB_VERSION {
        anyhow::bail!(
            "unsupported pre-v0.7.0 job payload blob version {version}; expected {PRE_V0_7_0_JOB_PAYLOAD_BLOB_VERSION}"
        );
    }
    Ok(&bytes[header_len..])
}

impl PreV0_7_0Job {
    fn into_current(self) -> Result<Job> {
        Ok(Job {
            id: self.id,
            kind: self.kind,
            scope_kind: self.scope_kind,
            guild_id: self.guild_id,
            scope_id: self.scope_id,
            state: self.state,
            requested_by_user_id: self.requested_by_user_id,
            payload: self.payload.into_current()?,
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
            metadata: self.metadata,
        })
    }
}
