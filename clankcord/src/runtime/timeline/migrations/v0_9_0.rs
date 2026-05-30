use crate::Result;
use crate::runtime::Job;

const JOB_PAYLOAD_BLOB_MAGIC: &[u8; 8] = b"CLANKJOB";
const PRE_V0_9_0_JOB_PAYLOAD_BLOB_VERSION: u16 = 6;

pub(super) async fn run(transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>) -> Result<()> {
    reencode_job_payloads(transaction).await?;
    sqlx::raw_sql(
        r#"
        CREATE INDEX IF NOT EXISTS idx_transcription_slots_source_state_created
          ON transcription_slots(transcription_source_id, state, priority DESC, created_at_ms, slot_id);
        CREATE INDEX IF NOT EXISTS idx_transcription_slots_source_mux_state
          ON transcription_slots(transcription_source_id, mux_job_id, state)
          WHERE mux_job_id <> '';
        "#,
    )
    .execute(transaction.as_mut())
    .await?;
    Ok(())
}

async fn reencode_job_payloads(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<()> {
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
        let job = decode_pre_v0_9_0_job_payload_blob(&payload_blob).map_err(|error| {
            anyhow::anyhow!("migrating v0.9.0 job payload blob {job_id}: {error:#}")
        })?;
        sqlx::query(
            r#"
            UPDATE job_payloads
            SET payload_blob = $2
            WHERE job_id = $1
            "#,
        )
        .bind(&job_id)
        .bind(job.encode()?)
        .execute(transaction.as_mut())
        .await?;
    }
    Ok(())
}

fn decode_pre_v0_9_0_job_payload_blob(bytes: &[u8]) -> Result<Job> {
    let body = pre_v0_9_0_envelope_body(bytes)?;
    Ok(bincode::deserialize(body)?)
}

fn pre_v0_9_0_envelope_body(bytes: &[u8]) -> Result<&[u8]> {
    if bytes.len() < JOB_PAYLOAD_BLOB_MAGIC.len() + std::mem::size_of::<u16>()
        || &bytes[..JOB_PAYLOAD_BLOB_MAGIC.len()] != JOB_PAYLOAD_BLOB_MAGIC
    {
        anyhow::bail!("job payload blob is not a pre-v0.9.0 encoded job payload");
    }
    let version_offset = JOB_PAYLOAD_BLOB_MAGIC.len();
    let version = u16::from_le_bytes([bytes[version_offset], bytes[version_offset + 1]]);
    if version != PRE_V0_9_0_JOB_PAYLOAD_BLOB_VERSION {
        anyhow::bail!(
            "unsupported pre-v0.9.0 job payload blob version {version}; expected {PRE_V0_9_0_JOB_PAYLOAD_BLOB_VERSION}"
        );
    }
    Ok(&bytes[version_offset + std::mem::size_of::<u16>()..])
}
