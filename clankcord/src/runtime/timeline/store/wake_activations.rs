use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WakeActivationProgress {
    pub request_audio_closed_at: DateTime<Utc>,
    pub transcription_wait_deadline_at: DateTime<Utc>,
}

impl TimelineStore {
    pub(crate) async fn wake_activation_progress(
        &self,
        job_id: &str,
    ) -> Result<Option<WakeActivationProgress>> {
        let row = sqlx::query(
            r#"
            SELECT request_audio_closed_at_ms,
                   transcription_wait_deadline_at_ms
            FROM wake_activation_progress
            WHERE job_id = $1
            "#,
        )
        .bind(job_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| {
            let closed_at_ms: i64 = row.try_get("request_audio_closed_at_ms")?;
            let deadline_at_ms: i64 = row.try_get("transcription_wait_deadline_at_ms")?;
            Ok(WakeActivationProgress {
                request_audio_closed_at: ms_to_datetime(closed_at_ms).ok_or_else(|| {
                    anyhow::anyhow!("wake activation {job_id} has invalid close time")
                })?,
                transcription_wait_deadline_at: ms_to_datetime(deadline_at_ms).ok_or_else(
                    || anyhow::anyhow!("wake activation {job_id} has invalid settlement deadline"),
                )?,
            })
        })
        .transpose()
    }

    pub(crate) async fn record_wake_activation_progress(
        &self,
        job_id: &str,
        request_audio_closed_at: DateTime<Utc>,
        transcription_wait_deadline_at: DateTime<Utc>,
    ) -> Result<WakeActivationProgress> {
        let now_ms = instant_ms_dt(utc_now());
        let row = sqlx::query(
            r#"
            INSERT INTO wake_activation_progress(
              job_id,
              request_audio_closed_at_ms,
              transcription_wait_deadline_at_ms,
              created_at_ms,
              updated_at_ms
            )
            VALUES ($1, $2, $3, $4, $4)
            ON CONFLICT(job_id) DO UPDATE SET
              request_audio_closed_at_ms = EXCLUDED.request_audio_closed_at_ms,
              transcription_wait_deadline_at_ms = EXCLUDED.transcription_wait_deadline_at_ms,
              updated_at_ms = EXCLUDED.updated_at_ms
            RETURNING request_audio_closed_at_ms,
                      transcription_wait_deadline_at_ms
            "#,
        )
        .bind(job_id)
        .bind(instant_ms_dt(request_audio_closed_at))
        .bind(instant_ms_dt(transcription_wait_deadline_at))
        .bind(now_ms)
        .fetch_one(&self.pool)
        .await?;
        let closed_at_ms: i64 = row.try_get("request_audio_closed_at_ms")?;
        let deadline_at_ms: i64 = row.try_get("transcription_wait_deadline_at_ms")?;
        Ok(WakeActivationProgress {
            request_audio_closed_at: ms_to_datetime(closed_at_ms).ok_or_else(|| {
                anyhow::anyhow!("wake activation {job_id} has invalid close time")
            })?,
            transcription_wait_deadline_at: ms_to_datetime(deadline_at_ms).ok_or_else(|| {
                anyhow::anyhow!("wake activation {job_id} has invalid settlement deadline")
            })?,
        })
    }

    pub(crate) async fn clear_wake_activation_progress(&self, job_id: &str) -> Result<()> {
        sqlx::query("DELETE FROM wake_activation_progress WHERE job_id = $1")
            .bind(job_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}
