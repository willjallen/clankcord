//! Audio segment intake: verifies the captured artifact and queues its
//! transcription slot for the mux planner. Transcription execution lives
//! in domain/transcription.

use serde_json::{Value, json};

use crate::Result;
use crate::domain::Ctx;
use crate::domain::transcription::mux;
use crate::model::job::AudioSegmentPayload;
use crate::util::sha256_file;

pub(crate) async fn execute_segment_job(
    runtime: &Ctx,
    job: &crate::model::job::Job,
    payload: &AudioSegmentPayload,
) -> Result<Value> {
    if let Some(event) = runtime
        .store
        .speech_event_for_segment(
            &payload.guild_id,
            &payload.voice_channel_id,
            &payload.capture_run_id,
            &payload.speaker_user_id,
            payload.segment_index,
        )
        .await?
    {
        return Ok(json!({
            "kind": "audio_segment",
            "status": "already_transcribed",
            "event": event,
        }));
    }

    let wav_path = payload.source_audio_path.clone();
    if !wav_path.is_file() {
        anyhow::bail!("audio segment artifact is missing: {}", wav_path.display());
    }
    let audio_checksum = sha256_file(&wav_path)?;
    if !payload.audio_checksum.trim().is_empty() && payload.audio_checksum != audio_checksum {
        anyhow::bail!(
            "audio segment checksum mismatch for {}: expected {}, got {}",
            wav_path.display(),
            payload.audio_checksum,
            audio_checksum
        );
    }
    let audio_bytes = wav_path.metadata()?.len();

    let priority = runtime
        .store
        .audio_segment_transcription_priority(payload)
        .await?;
    let slot = runtime
        .store
        .create_transcription_slot_for_audio_segment(&job.id, payload, priority)
        .await?;
    let planner_delay_ms = if priority >= 1000 {
        0
    } else {
        crate::config::transcription_mux_batch_delay_ms()
    };
    let planner_job = mux::ensure_transcription_mux_plan_job(
        runtime,
        &crate::config::active_transcription_source_id(),
        planner_delay_ms,
    )
    .await?;
    Ok(json!({
        "kind": "audio_segment",
        "status": "queued_for_transcription",
        "segment_index": payload.segment_index,
        "speaker_user_id": payload.speaker_user_id,
        "speaker_label": payload.speaker_label,
        "duration_ms": payload.duration_ms,
        "source_audio_path": wav_path.display().to_string(),
        "audio_checksum": audio_checksum.clone(),
        "audio_bytes": audio_bytes,
        "audio_format": payload.audio_format,
        "sample_rate_hz": payload.sample_rate_hz,
        "channels": payload.channels,
        "sample_width_bits": payload.sample_width_bits,
        "post_processing": payload.post_processing,
        "transcription_slot": slot,
        "transcription_mux_plan_job": planner_job.map(|job| job.to_value()),
    }))
}
