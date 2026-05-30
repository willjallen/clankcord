use super::*;

use crate::config;
use crate::runtime::AudioSegmentPayload;
use std::collections::VecDeque;

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
    pub mux_stream_id: String,
    pub mux_start_ms: Option<i64>,
    pub mux_end_ms: Option<i64>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone)]
struct ActiveMuxStream {
    available_at_ms: i64,
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
        payload: &crate::runtime::WakeActivationPayload,
    ) -> Result<Vec<String>> {
        let Some(wake_started_at) = parse_instant(&payload.wake_started_at) else {
            return Ok(Vec::new());
        };
        let hard_cap = wake_started_at + chrono::Duration::seconds(payload.max_window_seconds);
        let window_end = std::cmp::min(utc_now(), hard_cap);
        let now_ms = instant_ms_dt(utc_now());
        let rows = sqlx::query(
            r#"
            UPDATE transcription_slots
            SET priority = 1000,
                updated_at_ms = $4,
                payload_json = payload_json || jsonb_build_object(
                  'priority', 1000,
                  'wake_activation_id', $5
                )
            WHERE guild_id = $1
              AND voice_channel_id = $2
              AND state = 'queued'
              AND segment_start_ms <= $3
              AND priority < 1000
            RETURNING transcription_source_id
            "#,
        )
        .bind(&payload.guild_id)
        .bind(&payload.voice_channel_id)
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

    pub(crate) async fn ensure_transcription_mux_plan_job(
        &self,
        source_id: &str,
        delay_ms: i64,
    ) -> Result<Option<Job>> {
        let source_id = source_id.trim();
        if source_id.is_empty() || !self.has_queued_transcription_slots(source_id).await? {
            return Ok(None);
        }
        let ordering_key = transcription_mux_plan_ordering_key(source_id);
        if let Some(mut existing) = self
            .active_transcription_mux_plan_job(&ordering_key)
            .await?
        {
            if delay_ms <= 0
                && existing.state == crate::runtime::JobState::Queued
                && existing.next_run_at.is_some()
            {
                existing.next_run_at = None;
                self.update_job(&existing).await?;
            }
            return Ok(Some(existing));
        }
        self.create_job(Job::transcription_mux_plan(source_id, delay_ms.max(0)))
            .await
            .map(Some)
    }

    pub(crate) async fn ensure_transcription_mux_plan_jobs_for_queued_slots(
        &self,
        delay_ms: i64,
    ) -> Result<Vec<Job>> {
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
        let mut jobs = Vec::new();
        for row in rows {
            let source_id: String = row.try_get("transcription_source_id")?;
            if let Some(job) = self
                .ensure_transcription_mux_plan_job(&source_id, delay_ms)
                .await?
            {
                jobs.push(job);
            }
        }
        Ok(jobs)
    }

    pub(crate) async fn plan_transcription_mux_jobs(&self, source_id: &str) -> Result<Value> {
        let source_id = source_id.trim();
        let max_streams = config::transcription_mux_provider_streams();
        let max_slots = config::transcription_mux_max_slots();
        let max_audio_ms = config::transcription_mux_max_audio_ms();
        let guard_ms = config::transcription_mux_guard_ms();
        let normal_budget_ms = config::transcription_mux_normal_latency_budget_ms();
        let wake_budget_ms = config::transcription_mux_wake_latency_budget_ms();
        let overflow_backlog_ms = config::transcription_mux_overflow_backlog_ms();
        let now_ms = instant_ms_dt(utc_now());
        let mut transaction = self.pool.begin().await?;
        let mut active_streams =
            active_mux_streams_for_source(&mut transaction, source_id, now_ms, guard_ms).await?;
        active_streams.sort_by_key(|stream| stream.available_at_ms);
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
        .bind((max_slots * max_streams * 64).clamp(max_slots, 2048) as i64)
        .fetch_all(transaction.as_mut())
        .await?;
        let mut queued = rows
            .iter()
            .map(transcription_slot_from_row)
            .collect::<Result<Vec<_>>>()?;
        if queued.is_empty() {
            transaction.commit().await?;
            return Ok(serde_json::json!({
                "kind": "transcription_mux_plan",
                "status": "idle",
                "transcription_source_id": source_id,
                "active_provider_streams": active_streams.len(),
            }));
        }
        let initial_active_streams = active_streams.len();
        let capacity = max_streams.saturating_sub(initial_active_streams);
        if capacity == 0 {
            transaction.commit().await?;
            return Ok(serde_json::json!({
                "kind": "transcription_mux_plan",
                "status": "provider_streams_full",
                "transcription_source_id": source_id,
                "active_provider_streams": initial_active_streams,
                "queued_slots": queued.len(),
            }));
        }
        let mut mux_jobs = Vec::new();
        for _ in 0..capacity {
            let has_no_provider_stream = active_streams.is_empty();
            let should_start = has_no_provider_stream
                || predicted_mux_lateness_ms(
                    &queued,
                    &active_streams,
                    now_ms,
                    max_slots,
                    max_audio_ms,
                    guard_ms,
                    normal_budget_ms,
                    wake_budget_ms,
                ) > overflow_backlog_ms;
            if !should_start {
                break;
            }
            let batch = select_fair_mux_batch(&queued, max_slots, max_audio_ms, guard_ms);
            if batch.is_empty() {
                break;
            }
            let mux_job = Job::transcription_mux(source_id);
            let slot_ids = batch
                .iter()
                .map(|slot| slot.slot_id.clone())
                .collect::<Vec<_>>();
            mark_transcription_slots_planned(
                &mut transaction,
                &slot_ids,
                &mux_job.id,
                source_id,
                now_ms,
            )
            .await?;
            super::jobs::upsert_job_rows(&mut transaction, &mux_job).await?;
            let selected_ids = slot_ids.into_iter().collect::<BTreeSet<_>>();
            let batch_audio_ms = mux_audio_ms_for_slots(&batch, guard_ms);
            queued.retain(|slot| !selected_ids.contains(&slot.slot_id));
            active_streams.push(ActiveMuxStream {
                available_at_ms: now_ms.saturating_add(
                    estimated_transcription_provider_processing_ms(batch_audio_ms),
                ),
            });
            active_streams.sort_by_key(|stream| stream.available_at_ms);
            mux_jobs.push(serde_json::json!({
                "job_id": mux_job.id,
                "slot_count": batch.len(),
                "mux_audio_ms": batch_audio_ms,
            }));
            if queued.is_empty() {
                break;
            }
        }
        transaction.commit().await?;
        Ok(serde_json::json!({
            "kind": "transcription_mux_plan",
            "status": if mux_jobs.is_empty() { "deferred" } else { "planned" },
            "transcription_source_id": source_id,
            "max_provider_streams": max_streams,
            "active_provider_streams_before": initial_active_streams,
            "created_mux_jobs": mux_jobs,
            "remaining_queued_slots": queued.len(),
        }))
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

    async fn has_queued_transcription_slots(&self, source_id: &str) -> Result<bool> {
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

    async fn active_transcription_mux_plan_job(&self, ordering_key: &str) -> Result<Option<Job>> {
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

    pub(crate) async fn has_pending_transcription_slot_for_room_until(
        &self,
        guild_id: &str,
        voice_channel_id: &str,
        window_end: DateTime<Utc>,
    ) -> Result<bool> {
        let row = sqlx::query(
            r#"
            SELECT 1
            FROM transcription_slots
            WHERE guild_id = $1
              AND voice_channel_id = $2
              AND state IN ('queued', 'planned', 'muxing', 'failed')
              AND segment_start_ms <= $3
            LIMIT 1
            "#,
        )
        .bind(guild_id)
        .bind(voice_channel_id)
        .bind(instant_ms_dt(window_end))
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.is_some())
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
    guard_ms: i64,
) -> Result<Vec<ActiveMuxStream>> {
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
            let started_at_ms: i64 = row.try_get("stream_started_at_ms")?;
            let speech_ms: i64 = row.try_get("speech_ms")?;
            let slot_count: i64 = row.try_get("slot_count")?;
            let mux_audio_ms = mux_audio_ms_for_counts(speech_ms, slot_count as usize, guard_ms);
            Ok(ActiveMuxStream {
                available_at_ms: now_ms.max(
                    started_at_ms.saturating_add(estimated_transcription_provider_processing_ms(
                        mux_audio_ms,
                    )),
                ),
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

fn predicted_mux_lateness_ms(
    queued: &[TranscriptionSlotRecord],
    active_streams: &[ActiveMuxStream],
    now_ms: i64,
    max_slots: usize,
    max_audio_ms: i64,
    guard_ms: i64,
    normal_budget_ms: i64,
    wake_budget_ms: i64,
) -> i64 {
    if queued.is_empty() || active_streams.is_empty() {
        return 0;
    }
    let mut remaining = queued.to_vec();
    let mut stream_available = active_streams
        .iter()
        .map(|stream| stream.available_at_ms)
        .collect::<Vec<_>>();
    let mut max_lateness = 0i64;
    while !remaining.is_empty() {
        stream_available.sort_unstable();
        let batch = select_fair_mux_batch(&remaining, max_slots, max_audio_ms, guard_ms);
        if batch.is_empty() {
            break;
        }
        let start_ms = now_ms.max(stream_available[0]);
        let finish_ms = start_ms.saturating_add(estimated_transcription_provider_processing_ms(
            mux_audio_ms_for_slots(&batch, guard_ms),
        ));
        for slot in &batch {
            let budget_ms = if slot.priority >= 1000 {
                wake_budget_ms
            } else {
                normal_budget_ms
            };
            let deadline_ms = instant_ms_dt(slot.segment_end_time).saturating_add(budget_ms);
            max_lateness = max_lateness.max(finish_ms.saturating_sub(deadline_ms));
        }
        stream_available[0] = finish_ms;
        let selected = batch
            .iter()
            .map(|slot| slot.slot_id.clone())
            .collect::<BTreeSet<_>>();
        remaining.retain(|slot| !selected.contains(&slot.slot_id));
    }
    max_lateness
}

fn select_fair_mux_batch(
    queued: &[TranscriptionSlotRecord],
    max_slots: usize,
    max_audio_ms: i64,
    guard_ms: i64,
) -> Vec<TranscriptionSlotRecord> {
    let max_slots = max_slots.clamp(1, 128);
    let max_audio_ms = max_audio_ms.max(1);
    let mut selected = Vec::new();
    let mut priorities = queued
        .iter()
        .map(|slot| slot.priority)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    priorities.sort_by(|left, right| right.cmp(left));
    for priority in priorities {
        let mut flows = fair_slot_flows(queued, priority);
        loop {
            let mut added = false;
            for (_flow_key, slots) in &mut flows {
                let Some(slot) = slots.pop_front() else {
                    continue;
                };
                if selected.len() >= max_slots {
                    return selected;
                }
                let mut candidate = selected.clone();
                candidate.push(slot.clone());
                let candidate_audio_ms = mux_audio_ms_for_slots(&candidate, guard_ms);
                if !selected.is_empty() && candidate_audio_ms > max_audio_ms {
                    continue;
                }
                selected.push(slot);
                added = true;
            }
            if !added
                || selected.len() >= max_slots
                || flows.iter().all(|(_, slots)| slots.is_empty())
            {
                break;
            }
        }
        if selected.len() >= max_slots
            || mux_audio_ms_for_slots(&selected, guard_ms) >= max_audio_ms
        {
            break;
        }
    }
    selected
}

fn fair_slot_flows(
    queued: &[TranscriptionSlotRecord],
    priority: i64,
) -> Vec<(String, VecDeque<TranscriptionSlotRecord>)> {
    let mut grouped = BTreeMap::<String, Vec<TranscriptionSlotRecord>>::new();
    for slot in queued.iter().filter(|slot| slot.priority == priority) {
        grouped
            .entry(slot_flow_key(slot))
            .or_default()
            .push(slot.clone());
    }
    let mut flows = grouped
        .into_iter()
        .map(|(flow_key, mut slots)| {
            slots.sort_by(|left, right| {
                left.created_at_ms
                    .cmp(&right.created_at_ms)
                    .then_with(|| left.slot_id.cmp(&right.slot_id))
            });
            let first_created_at_ms = slots.first().map(|slot| slot.created_at_ms).unwrap_or(0);
            (first_created_at_ms, flow_key, VecDeque::from(slots))
        })
        .collect::<Vec<_>>();
    flows.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    flows
        .into_iter()
        .map(|(_, flow_key, slots)| (flow_key, slots))
        .collect()
}

fn slot_flow_key(slot: &TranscriptionSlotRecord) -> String {
    format!(
        "{}:{}:{}",
        slot.guild_id, slot.voice_channel_id, slot.speaker_user_id
    )
}

fn mux_audio_ms_for_slots(slots: &[TranscriptionSlotRecord], guard_ms: i64) -> i64 {
    let speech_ms = slots
        .iter()
        .map(|slot| slot.duration_ms.max(0))
        .sum::<i64>();
    mux_audio_ms_for_counts(speech_ms, slots.len(), guard_ms)
}

fn mux_audio_ms_for_counts(speech_ms: i64, slot_count: usize, guard_ms: i64) -> i64 {
    if slot_count == 0 {
        return 0;
    }
    speech_ms
        .max(0)
        .saturating_add(guard_ms.max(0).saturating_mul((slot_count * 2 - 1) as i64))
}

fn estimated_transcription_provider_processing_ms(audio_ms: i64) -> i64 {
    ((audio_ms.max(0) as f64 * 0.3) + 2_500.0).ceil() as i64
}

fn transcription_mux_plan_ordering_key(source_id: &str) -> String {
    format!("transcription:mux_plan:{}", normalize_key_part(source_id))
}

fn normalize_key_part(value: &str) -> String {
    let normalized = value
        .trim()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    if normalized.is_empty() {
        "unknown".to_string()
    } else {
        normalized
    }
}
