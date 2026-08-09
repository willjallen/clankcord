use super::*;

use crate::config;
use crate::model::job::AudioSegmentPayload;
use crate::runtime::domain::voice_capture::segments;

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub(crate) struct TranscriptionSlotRecord {
    pub slot_id: String,
    pub source_job_id: String,
    pub mux_job_id: String,
    pub state: String,
    pub guild_id: String,
    pub guild_slug: String,
    pub voice_channel_id: String,
    pub voice_channel_name: String,
    pub voice_channel_slug: String,
    pub capture_run_id: String,
    pub voice_bot_id: String,
    pub voice_bot_discord_user_id: String,
    pub speaker_user_id: String,
    pub speaker_label: String,
    pub speaker_username: String,
    pub segment_index: i64,
    pub segment_start_time: DateTime<Utc>,
    pub segment_end_time: DateTime<Utc>,
    pub duration_ms: i64,
    pub source_audio_path: PathBuf,
    pub audio_checksum: String,
    pub audio_bytes: u64,
    pub audio_format: String,
    pub sample_rate_hz: u32,
    pub channels: u16,
    pub sample_width_bits: u16,
    pub post_processing: String,
    pub transcription_source_id: String,
    pub provider: String,
    pub model: String,
    pub priority: i64,
    pub requires_single_slot: bool,
    pub error: String,
    pub mux_stream_id: String,
    pub mux_start_ms: Option<i64>,
    pub mux_end_ms: Option<i64>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ActiveMuxStreamRow {
    pub started_at_ms: i64,
    pub speech_ms: i64,
    pub slot_count: i64,
}

impl TimelineStore {
    pub(crate) async fn create_transcription_slot_for_audio_segment(
        &self,
        source_job_id: &str,
        payload: &AudioSegmentPayload,
        priority: i64,
    ) -> Result<Value> {
        let source = config::active_transcription_source()?;
        let now_ms = instant_ms_dt(utc_now());
        let slot_id = new_id("tslot");
        let provider = source.config.provider.as_str().to_string();
        let model = source.config.model.trim().to_string();
        let payload_json = serde_json::json!({
            "slot_id": slot_id,
            "source_job_id": source_job_id,
            "state": "queued",
            "guild_id": payload.guild_id,
            "guild_slug": payload.guild_slug,
            "voice_channel_id": payload.voice_channel_id,
            "voice_channel_name": payload.voice_channel_name,
            "voice_channel_slug": payload.voice_channel_slug,
            "capture_run_id": payload.capture_run_id,
            "voice_bot_id": payload.voice_bot_id,
            "voice_bot_discord_user_id": payload.voice_bot_discord_user_id,
            "speaker_user_id": payload.speaker_user_id,
            "speaker_label": payload.speaker_label,
            "speaker_username": payload.speaker_username,
            "segment_index": payload.segment_index,
            "segment_start_time": isoformat_z(Some(payload.segment_start_time)),
            "segment_end_time": isoformat_z(Some(payload.segment_end_time)),
            "duration_ms": payload.duration_ms,
            "source_audio_path": payload.source_audio_path.display().to_string(),
            "audio_checksum": payload.audio_checksum,
            "audio_bytes": payload.audio_bytes,
            "audio_format": payload.audio_format,
            "sample_rate_hz": payload.sample_rate_hz,
            "channels": payload.channels,
            "sample_width_bits": payload.sample_width_bits,
            "post_processing": payload.post_processing,
            "transcription_source_id": source.id,
            "provider": provider,
            "model": model,
            "priority": priority,
            "created_at": isoformat_z(None),
        });
        sqlx::query(
            r#"
            INSERT INTO transcription_slots(
              slot_id,
              source_job_id,
              mux_job_id,
              state,
              guild_id,
              voice_channel_id,
              capture_run_id,
              voice_bot_id,
              voice_bot_discord_user_id,
              speaker_user_id,
              speaker_label,
              speaker_username,
              segment_index,
              segment_start_ms,
              segment_end_ms,
              duration_ms,
              source_audio_path,
              audio_checksum,
              audio_bytes,
              audio_format,
              sample_rate_hz,
              channels,
              sample_width_bits,
              post_processing,
              transcription_source_id,
              provider,
              model,
              priority,
              mux_stream_id,
              guard_before_ms,
              guard_after_ms,
              created_at_ms,
              updated_at_ms,
              payload_json
            )
            VALUES (
              $1, $2, '', 'queued', $3, $4, $5, $6, $7, $8, $9, $10,
              $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21,
              $22, $23, $24, $25, $26, '', 0, 0, $27, $28, $29
            )
            ON CONFLICT(source_job_id) DO NOTHING
            "#,
        )
        .bind(&slot_id)
        .bind(source_job_id)
        .bind(&payload.guild_id)
        .bind(&payload.voice_channel_id)
        .bind(&payload.capture_run_id)
        .bind(&payload.voice_bot_id)
        .bind(&payload.voice_bot_discord_user_id)
        .bind(&payload.speaker_user_id)
        .bind(&payload.speaker_label)
        .bind(&payload.speaker_username)
        .bind(payload.segment_index)
        .bind(instant_ms_dt(payload.segment_start_time))
        .bind(instant_ms_dt(payload.segment_end_time))
        .bind(payload.duration_ms)
        .bind(payload.source_audio_path.display().to_string())
        .bind(&payload.audio_checksum)
        .bind(payload.audio_bytes as i64)
        .bind(&payload.audio_format)
        .bind(payload.sample_rate_hz as i64)
        .bind(payload.channels as i64)
        .bind(payload.sample_width_bits as i64)
        .bind(&payload.post_processing)
        .bind(&source.id)
        .bind(&provider)
        .bind(&model)
        .bind(priority)
        .bind(now_ms)
        .bind(now_ms)
        .bind(&payload_json)
        .execute(&self.pool)
        .await?;
        let row =
            sqlx::query("SELECT payload_json FROM transcription_slots WHERE source_job_id = $1")
                .bind(source_job_id)
                .fetch_one(&self.pool)
                .await?;
        json_value(&row, "payload_json")
    }

    pub(crate) async fn promote_transcription_slots_for_wake_activation(
        &self,
        payload: &crate::model::job::WakeActivationPayload,
    ) -> Result<Vec<String>> {
        let Some(wake_started_at) = parse_instant(&payload.wake_started_at) else {
            return Ok(Vec::new());
        };
        let hard_cap = wake_started_at + chrono::Duration::seconds(payload.max_window_seconds);
        let window_start = wake_started_at - chrono::Duration::seconds(payload.lookback_seconds);
        let window_end = std::cmp::min(utc_now(), hard_cap);
        let now_ms = instant_ms_dt(utc_now());
        let rows = sqlx::query(
            r#"
            UPDATE transcription_slots
            SET priority = 1000,
                updated_at_ms = $5,
                payload_json = payload_json || jsonb_build_object(
                  'priority', 1000,
                  'wake_activation_id', $6
                )
            WHERE guild_id = $1
              AND voice_channel_id = $2
              AND state = 'queued'
              AND segment_start_ms <= $4
              AND segment_end_ms >= $3
              AND priority < 1000
            RETURNING transcription_source_id
            "#,
        )
        .bind(&payload.guild_id)
        .bind(&payload.voice_channel_id)
        .bind(instant_ms_dt(window_start))
        .bind(instant_ms_dt(window_end))
        .bind(now_ms)
        .bind(&payload.activation_id)
        .fetch_all(&self.pool)
        .await?;
        let mut sources = rows
            .iter()
            .map(|row| row.try_get::<String, _>("transcription_source_id"))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        sources.sort();
        sources.dedup();
        Ok(sources)
    }

    pub(crate) async fn start_transcription_slots_for_mux(
        &self,
        mux_job_id: &str,
        source_id: &str,
    ) -> Result<Vec<TranscriptionSlotRecord>> {
        let existing = self
            .list_transcription_slots_by_mux_job(mux_job_id, Some("muxing"))
            .await?;
        if !existing.is_empty() {
            return Ok(existing);
        }
        let rows = sqlx::query(
            r#"
            UPDATE transcription_slots
            SET state = 'muxing',
                updated_at_ms = $3,
                payload_json = payload_json
                  || jsonb_build_object('state', 'muxing', 'mux_job_id', $1)
            WHERE mux_job_id = $1
              AND transcription_source_id = $2
              AND state = 'planned'
            RETURNING *
            "#,
        )
        .bind(mux_job_id)
        .bind(source_id)
        .bind(instant_ms_dt(utc_now()))
        .fetch_all(&self.pool)
        .await?;
        if !rows.is_empty() {
            return rows.iter().map(transcription_slot_from_row).collect();
        }
        self.list_transcription_slots_by_mux_job(mux_job_id, Some("muxing"))
            .await
    }

    pub(crate) async fn update_transcription_slot_mux_offsets(
        &self,
        slot_id: &str,
        mux_stream_id: &str,
        mux_start_ms: i64,
        mux_end_ms: i64,
        guard_before_ms: i64,
        guard_after_ms: i64,
    ) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE transcription_slots
            SET mux_stream_id = $2,
                mux_start_ms = $3,
                mux_end_ms = $4,
                guard_before_ms = $5,
                guard_after_ms = $6,
                updated_at_ms = $7,
                payload_json = payload_json || jsonb_build_object(
                  'mux_stream_id', $2,
                  'mux_start_ms', $3,
                  'mux_end_ms', $4,
                  'guard_before_ms', $5,
                  'guard_after_ms', $6
                )
            WHERE slot_id = $1
            "#,
        )
        .bind(slot_id)
        .bind(mux_stream_id)
        .bind(mux_start_ms)
        .bind(mux_end_ms)
        .bind(guard_before_ms)
        .bind(guard_after_ms)
        .bind(instant_ms_dt(utc_now()))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub(crate) async fn complete_transcription_slot(
        &self,
        slot_id: &str,
        event_id: &str,
        text: &str,
    ) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE transcription_slots
            SET state = 'complete',
                updated_at_ms = $2,
                payload_json = payload_json || jsonb_build_object(
                  'state', 'complete',
                  'speech_event_id', $3,
                  'text', $4
                )
            WHERE slot_id = $1
            "#,
        )
        .bind(slot_id)
        .bind(instant_ms_dt(utc_now()))
        .bind(event_id)
        .bind(text)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub(crate) async fn fail_transcription_slots_for_mux(
        &self,
        mux_job_id: &str,
        error: &str,
    ) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE transcription_slots
            SET state = 'failed',
                updated_at_ms = $2,
                payload_json = payload_json || jsonb_build_object(
                  'state', 'failed',
                  'error', $3
                )
            WHERE mux_job_id = $1
              AND state IN ('planned', 'muxing')
            "#,
        )
        .bind(mux_job_id)
        .bind(instant_ms_dt(utc_now()))
        .bind(error)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub(crate) async fn requeue_transcription_slots_as_single_slot_muxes(
        &self,
        mux_job_id: &str,
        reason: &str,
    ) -> Result<Vec<String>> {
        let rows = sqlx::query(
            r#"
            UPDATE transcription_slots
            SET state = 'queued',
                mux_job_id = '',
                mux_stream_id = '',
                mux_start_ms = NULL,
                mux_end_ms = NULL,
                guard_before_ms = 0,
                guard_after_ms = 0,
                requires_single_slot = TRUE,
                updated_at_ms = $2,
                payload_json =
                  payload_json
                    - 'mux_job_id'
                    - 'mux_stream_id'
                    - 'mux_start_ms'
                    - 'mux_end_ms'
                    - 'guard_before_ms'
                    - 'guard_after_ms'
                    - 'error'
                    || jsonb_build_object(
                      'state', 'queued',
                      'requires_single_slot', TRUE,
                      'timestamp_replan_reason', $3,
                      'timestamp_replanned_from_mux_job_id', $1,
                      'timestamp_replanned_at_ms', $2
                    )
            WHERE mux_job_id = $1
              AND state IN ('planned', 'muxing')
            RETURNING slot_id
            "#,
        )
        .bind(mux_job_id)
        .bind(instant_ms_dt(utc_now()))
        .bind(reason)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|row| row.try_get::<String, _>("slot_id").map_err(Into::into))
            .collect()
    }

    pub(crate) async fn list_transcription_slots_by_mux_job(
        &self,
        mux_job_id: &str,
        state: Option<&str>,
    ) -> Result<Vec<TranscriptionSlotRecord>> {
        let mut query = QueryBuilder::<Postgres>::new("SELECT * FROM transcription_slots");
        query.push(" WHERE mux_job_id = ").push_bind(mux_job_id);
        if let Some(state) = state.filter(|value| !value.trim().is_empty()) {
            query.push(" AND state = ").push_bind(state);
        }
        query.push(" ORDER BY mux_start_ms NULLS FIRST, created_at_ms, slot_id");
        query
            .build()
            .fetch_all(&self.pool)
            .await?
            .iter()
            .map(transcription_slot_from_row)
            .collect()
    }

    pub async fn recover_abandoned_transcription_slots(&self) -> Result<Value> {
        let now_ms = instant_ms_dt(utc_now());
        let requeued = sqlx::query(
            r#"
            UPDATE transcription_slots slot
            SET state = 'queued',
                mux_job_id = '',
                mux_stream_id = '',
                mux_start_ms = NULL,
                mux_end_ms = NULL,
                guard_before_ms = 0,
                guard_after_ms = 0,
                updated_at_ms = $1,
                payload_json =
                  payload_json
                    - 'mux_job_id'
                    - 'mux_stream_id'
                    - 'mux_start_ms'
                    - 'mux_end_ms'
                    - 'guard_before_ms'
                    - 'guard_after_ms'
                    || jsonb_build_object(
                      'state', 'queued',
                      'recovered_from_mux_job_id', slot.mux_job_id,
                      'recovered_at_ms', $1
                    )
            FROM jobs mux
            WHERE slot.state IN ('planned', 'muxing')
              AND slot.mux_job_id = mux.job_id
              AND mux.kind = 'transcription_mux'
              AND mux.state = 'failed_timeout'
            RETURNING slot.slot_id
            "#,
        )
        .bind(now_ms)
        .fetch_all(&self.pool)
        .await?;
        let failed = sqlx::query(
            r#"
            UPDATE transcription_slots slot
            SET state = 'failed',
                updated_at_ms = $1,
                payload_json = payload_json || jsonb_build_object(
                  'state', 'failed',
                  'error', 'terminal transcription mux job did not complete this slot',
                  'failed_mux_job_id', slot.mux_job_id,
                  'failed_at_ms', $1
                )
            FROM jobs mux
            WHERE slot.state IN ('planned', 'muxing')
              AND slot.mux_job_id = mux.job_id
              AND mux.kind = 'transcription_mux'
              AND mux.terminal = TRUE
              AND mux.state <> 'failed_timeout'
            RETURNING slot.slot_id
            "#,
        )
        .bind(now_ms)
        .fetch_all(&self.pool)
        .await?;
        Ok(serde_json::json!({
            "requeued": requeued
                .iter()
                .map(|row| row.try_get::<String, _>("slot_id"))
                .collect::<std::result::Result<Vec<_>, _>>()?,
            "failed": failed
                .iter()
                .map(|row| row.try_get::<String, _>("slot_id"))
                .collect::<std::result::Result<Vec<_>, _>>()?,
        }))
    }

    pub async fn requeue_retryable_failed_transcription_slots(
        &self,
        limit: usize,
    ) -> Result<Vec<Value>> {
        let limit = limit.clamp(1, 1000) as i64;
        let rows = sqlx::query(
            r#"
            SELECT slot_id,
                   source_job_id,
                   transcription_source_id,
                   payload_json
            FROM transcription_slots
            WHERE state = 'failed'
            ORDER BY updated_at_ms, slot_id
            LIMIT $1
            "#,
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        let mut requeued = Vec::new();
        for row in rows {
            let payload = json_value(&row, "payload_json")?;
            let error = first_value_string(&payload, &["error"]);
            if !segments::is_retryable_audio_segment_error_text(&error) {
                continue;
            }
            let slot_id: String = row.try_get("slot_id")?;
            let source_job_id: String = row.try_get("source_job_id")?;
            let transcription_source_id: String = row.try_get("transcription_source_id")?;
            let now_ms = instant_ms_dt(utc_now());
            let updated = sqlx::query(
                r#"
                UPDATE transcription_slots
                SET state = 'queued',
                    mux_job_id = '',
                    mux_stream_id = '',
                    mux_start_ms = NULL,
                    mux_end_ms = NULL,
                    guard_before_ms = 0,
                    guard_after_ms = 0,
                    updated_at_ms = $2,
                    payload_json =
                      payload_json
                        - 'mux_job_id'
                        - 'mux_stream_id'
                        - 'mux_start_ms'
                        - 'mux_end_ms'
                        - 'guard_before_ms'
                        - 'guard_after_ms'
                        - 'error'
                        - 'failed_mux_job_id'
                        - 'failed_at_ms'
                        || jsonb_build_object(
                          'state', 'queued',
                          'requeued_from_failed_at_ms', $2,
                          'requeued_from_failed_error', $3
                        )
                WHERE slot_id = $1
                  AND state = 'failed'
                RETURNING payload_json
                "#,
            )
            .bind(&slot_id)
            .bind(now_ms)
            .bind(&error)
            .fetch_optional(&self.pool)
            .await?;
            if updated.is_some() {
                requeued.push(serde_json::json!({
                    "slot_id": slot_id,
                    "source_job_id": source_job_id,
                    "transcription_source_id": transcription_source_id,
                    "error": error,
                }));
            }
        }
        Ok(requeued)
    }

    pub(crate) async fn queued_transcription_source_ids(&self) -> Result<Vec<String>> {
        let rows = sqlx::query(
            r#"
            SELECT DISTINCT transcription_source_id
            FROM transcription_slots
            WHERE state = 'queued'
            ORDER BY transcription_source_id
            "#,
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|row| Ok(row.try_get::<String, _>("transcription_source_id")?))
            .collect()
    }

    pub(crate) async fn has_queued_transcription_slots(&self, source_id: &str) -> Result<bool> {
        let row = sqlx::query(
            r#"
            SELECT 1
            FROM transcription_slots
            WHERE transcription_source_id = $1
              AND state = 'queued'
            LIMIT 1
            "#,
        )
        .bind(source_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.is_some())
    }

    pub(crate) async fn active_transcription_mux_plan_job(
        &self,
        ordering_key: &str,
    ) -> Result<Option<Job>> {
        let row = sqlx::query(
            r#"
            SELECT p.payload_blob
            FROM jobs j
            JOIN job_payloads p ON p.job_id = j.job_id
            WHERE j.kind = 'transcription_mux_plan'
              AND j.terminal = FALSE
              AND j.ordering_key = $1
            ORDER BY j.ready_at_ms, j.created_at_ms, j.job_id
            LIMIT 1
            "#,
        )
        .bind(ordering_key)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| {
            let payload: Vec<u8> = row.try_get("payload_blob")?;
            Job::decode(&payload)
        })
        .transpose()
    }

    pub(crate) async fn transcription_slots_for_room_window(
        &self,
        guild_id: &str,
        voice_channel_id: &str,
        window_start: DateTime<Utc>,
        window_end: DateTime<Utc>,
    ) -> Result<Vec<TranscriptionSlotRecord>> {
        let rows = sqlx::query(
            r#"
            SELECT *
            FROM transcription_slots
            WHERE guild_id = $1
              AND voice_channel_id = $2
              AND state IN ('queued', 'planned', 'muxing', 'failed')
              AND segment_start_ms <= $4
              AND segment_end_ms >= $3
            ORDER BY segment_start_ms, segment_end_ms, slot_id
            "#,
        )
        .bind(guild_id)
        .bind(voice_channel_id)
        .bind(instant_ms_dt(window_start))
        .bind(instant_ms_dt(window_end))
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(transcription_slot_from_row).collect()
    }
}

fn transcription_slot_from_row(row: &PgRow) -> Result<TranscriptionSlotRecord> {
    let segment_start_ms: i64 = row.try_get("segment_start_ms")?;
    let segment_end_ms: i64 = row.try_get("segment_end_ms")?;
    let payload = json_value(row, "payload_json")?;
    Ok(TranscriptionSlotRecord {
        slot_id: row.try_get("slot_id")?,
        source_job_id: row.try_get("source_job_id")?,
        mux_job_id: row.try_get("mux_job_id")?,
        state: row.try_get("state")?,
        guild_id: row.try_get("guild_id")?,
        guild_slug: string_field(&payload, "guild_slug"),
        voice_channel_id: row.try_get("voice_channel_id")?,
        voice_channel_name: string_field(&payload, "voice_channel_name"),
        voice_channel_slug: string_field(&payload, "voice_channel_slug"),
        capture_run_id: row.try_get("capture_run_id")?,
        voice_bot_id: row.try_get("voice_bot_id")?,
        voice_bot_discord_user_id: row.try_get("voice_bot_discord_user_id")?,
        speaker_user_id: row.try_get("speaker_user_id")?,
        speaker_label: row.try_get("speaker_label")?,
        speaker_username: row.try_get("speaker_username")?,
        segment_index: row.try_get("segment_index")?,
        segment_start_time: ms_to_datetime(segment_start_ms)
            .ok_or_else(|| anyhow::anyhow!("transcription slot has invalid segment_start_ms"))?,
        segment_end_time: ms_to_datetime(segment_end_ms)
            .ok_or_else(|| anyhow::anyhow!("transcription slot has invalid segment_end_ms"))?,
        duration_ms: row.try_get("duration_ms")?,
        source_audio_path: PathBuf::from(row.try_get::<String, _>("source_audio_path")?),
        audio_checksum: row.try_get("audio_checksum")?,
        audio_bytes: row.try_get::<i64, _>("audio_bytes")?.max(0) as u64,
        audio_format: row.try_get("audio_format")?,
        sample_rate_hz: row.try_get::<i64, _>("sample_rate_hz")?.max(0) as u32,
        channels: row.try_get::<i64, _>("channels")?.max(0) as u16,
        sample_width_bits: row.try_get::<i64, _>("sample_width_bits")?.max(0) as u16,
        post_processing: row.try_get("post_processing")?,
        transcription_source_id: row.try_get("transcription_source_id")?,
        provider: row.try_get("provider")?,
        model: row.try_get("model")?,
        priority: row.try_get("priority")?,
        requires_single_slot: row.try_get("requires_single_slot")?,
        error: first_value_string(&payload, &["error"]),
        mux_stream_id: row.try_get("mux_stream_id")?,
        mux_start_ms: row.try_get("mux_start_ms")?,
        mux_end_ms: row.try_get("mux_end_ms")?,
        created_at_ms: row.try_get("created_at_ms")?,
        updated_at_ms: row.try_get("updated_at_ms")?,
    })
}

async fn active_mux_streams_for_source(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    source_id: &str,
    now_ms: i64,
) -> Result<Vec<ActiveMuxStreamRow>> {
    let rows = sqlx::query(
        r#"
        SELECT
          slot.mux_job_id,
          COALESCE(mux.started_at_ms, mux.ready_at_ms, $2) AS stream_started_at_ms,
          COALESCE(SUM(slot.duration_ms), 0)::BIGINT AS speech_ms,
          COUNT(*) AS slot_count
        FROM transcription_slots slot
        JOIN jobs mux ON mux.job_id = slot.mux_job_id
        WHERE slot.transcription_source_id = $1
          AND slot.state IN ('planned', 'muxing')
          AND mux.kind = 'transcription_mux'
          AND mux.terminal = FALSE
        GROUP BY slot.mux_job_id, mux.started_at_ms, mux.ready_at_ms
        ORDER BY stream_started_at_ms, slot.mux_job_id
        "#,
    )
    .bind(source_id)
    .bind(now_ms)
    .fetch_all(transaction.as_mut())
    .await?;
    rows.iter()
        .map(|row| {
            Ok(ActiveMuxStreamRow {
                started_at_ms: row.try_get("stream_started_at_ms")?,
                speech_ms: row.try_get("speech_ms")?,
                slot_count: row.try_get("slot_count")?,
            })
        })
        .collect()
}

async fn mark_transcription_slots_planned(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    slot_ids: &[String],
    mux_job_id: &str,
    source_id: &str,
    now_ms: i64,
) -> Result<()> {
    if slot_ids.is_empty() {
        return Ok(());
    }
    sqlx::query(
        r#"
        UPDATE transcription_slots
        SET state = 'planned',
            mux_job_id = $2,
            updated_at_ms = $4,
            payload_json = payload_json
              || jsonb_build_object('state', 'planned', 'mux_job_id', $2)
        WHERE slot_id = ANY($1)
          AND transcription_source_id = $3
          AND state = 'queued'
        "#,
    )
    .bind(slot_ids)
    .bind(mux_job_id)
    .bind(source_id)
    .bind(now_ms)
    .execute(transaction.as_mut())
    .await?;
    Ok(())
}

/// One planning session over locked queued slots. SQL mechanics only:
/// what to batch and when to start streams is decided by
/// `runtime::domain::transcription::mux`.
pub(crate) struct MuxPlanning {
    transaction: sqlx::Transaction<'static, Postgres>,
    pub queued: Vec<TranscriptionSlotRecord>,
    pub active: Vec<ActiveMuxStreamRow>,
}

impl TimelineStore {
    pub(crate) async fn begin_mux_planning(
        &self,
        source_id: &str,
        candidate_limit: i64,
        now_ms: i64,
    ) -> Result<MuxPlanning> {
        let mut transaction = self.pool.begin().await?;
        let active = active_mux_streams_for_source(&mut transaction, source_id, now_ms).await?;
        let rows = sqlx::query(
            r#"
            SELECT *
            FROM transcription_slots
            WHERE state = 'queued'
              AND transcription_source_id = $1
            ORDER BY priority DESC, created_at_ms, slot_id
            LIMIT $2
            FOR UPDATE SKIP LOCKED
            "#,
        )
        .bind(source_id)
        .bind(candidate_limit)
        .fetch_all(transaction.as_mut())
        .await?;
        let queued = rows
            .iter()
            .map(transcription_slot_from_row)
            .collect::<Result<Vec<_>>>()?;
        Ok(MuxPlanning {
            transaction,
            queued,
            active,
        })
    }
}

impl MuxPlanning {
    pub(crate) async fn commit_batch(
        &mut self,
        slot_ids: &[String],
        mux_job: &Job,
        source_id: &str,
        now_ms: i64,
    ) -> Result<()> {
        mark_transcription_slots_planned(
            &mut self.transaction,
            slot_ids,
            &mux_job.id,
            source_id,
            now_ms,
        )
        .await?;
        super::jobs::upsert_job_rows(&mut self.transaction, mux_job).await?;
        Ok(())
    }

    pub(crate) async fn finish(self) -> Result<()> {
        self.transaction.commit().await?;
        Ok(())
    }
}
