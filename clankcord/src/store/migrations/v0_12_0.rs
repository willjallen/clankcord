use sqlx::Row;

use crate::Result;
use crate::model::job::{Job, JobState};

use crate::store::upsert_job_rows;

const EXPIRED_ACTIVATION_ERROR: &str =
    "expired_transcription_wait: activation predates bounded transcription settlement";

pub(super) async fn run(transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>) -> Result<()> {
    create_wake_settlement_state(transaction).await?;
    terminalize_preexisting_wake_activations(transaction).await
}

async fn create_wake_settlement_state(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<()> {
    sqlx::raw_sql(
        r#"
        ALTER TABLE transcription_slots
          ADD COLUMN IF NOT EXISTS requires_single_slot BOOLEAN NOT NULL DEFAULT FALSE;

        CREATE INDEX IF NOT EXISTS idx_transcription_slots_scope_state_interval
          ON transcription_slots(
            guild_id,
            voice_channel_id,
            state,
            segment_end_ms,
            segment_start_ms
          )
          WHERE state IN ('queued', 'planned', 'muxing', 'failed');

        CREATE TABLE IF NOT EXISTS wake_activation_progress (
          job_id TEXT PRIMARY KEY REFERENCES jobs(job_id) ON DELETE CASCADE,
          request_audio_closed_at_ms BIGINT NOT NULL,
          transcription_wait_deadline_at_ms BIGINT NOT NULL,
          created_at_ms BIGINT NOT NULL,
          updated_at_ms BIGINT NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_wake_activation_progress_deadline
          ON wake_activation_progress(transcription_wait_deadline_at_ms, job_id);
        "#,
    )
    .execute(transaction.as_mut())
    .await?;
    Ok(())
}

async fn terminalize_preexisting_wake_activations(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<()> {
    let rows = sqlx::query(
        r#"
        SELECT p.payload_blob
        FROM jobs j
        JOIN job_payloads p ON p.job_id = j.job_id
        WHERE j.kind = 'wake_activation'
          AND j.terminal = FALSE
        ORDER BY j.created_at_ms, j.job_id
        FOR UPDATE OF j
        "#,
    )
    .fetch_all(transaction.as_mut())
    .await?;
    for row in rows {
        let payload_blob: Vec<u8> = row.try_get("payload_blob")?;
        let mut job = Job::decode(&payload_blob)?;
        job.set_state(JobState::Failed);
        job.next_run_at = None;
        job.metadata.error = EXPIRED_ACTIVATION_ERROR.to_string();
        job.touch();
        upsert_job_rows(transaction, &job).await?;
    }
    Ok(())
}
