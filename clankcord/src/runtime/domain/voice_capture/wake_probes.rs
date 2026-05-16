use std::collections::BTreeSet;

use serde_json::{Value, json};

use crate::Result;
use crate::adapters::wakeword::detect_wake_file_sync;
use crate::config;
use crate::runtime::domain::voice_capture::wake_activations::schedule_from_wake_event;
use crate::runtime::timeline::{event_end, event_start, isoformat_z, sha256_file};
use crate::runtime::util::first_value_string;
use crate::runtime::{Job, Runtime, WakeProbePayload};

pub(crate) async fn execute_probe_job(
    runtime: &Runtime,
    job: &Job,
    payload: &WakeProbePayload,
) -> Result<Value> {
    let wav_path = payload.source_audio_path.clone();
    if !wav_path.is_file() {
        anyhow::bail!("wake probe artifact is missing: {}", wav_path.display());
    }
    let audio_checksum = sha256_file(&wav_path)?;
    if !payload.audio_checksum.trim().is_empty() && payload.audio_checksum != audio_checksum {
        anyhow::bail!(
            "wake probe checksum mismatch for {}: expected {}, got {}",
            wav_path.display(),
            payload.audio_checksum,
            audio_checksum
        );
    }
    let audio_bytes = wav_path.metadata()?.len();
    let detection_stream_id = payload.stream_id.clone();
    let wake = detect_wake_file_sync(&wav_path, &detection_stream_id, payload.reset_stream)?;
    let wake_metadata = wake.to_json();
    let mut result = json!({
        "kind": "wake_probe",
        "status": if wake.wake { "wake_detected" } else { "no_wake" },
        "probe_index": payload.probe_index,
        "speaker_user_id": payload.speaker_user_id,
        "speaker_label": payload.speaker_label,
        "duration_ms": payload.duration_ms,
        "source_audio_path": wav_path.display().to_string(),
        "audio_checksum": audio_checksum.clone(),
        "audio_bytes": audio_bytes,
        "stream_id": payload.stream_id,
        "wake": wake_metadata.clone(),
    });
    if !wake.wake {
        return Ok(result);
    }
    if let Some(existing) = overlapping_wake_event(runtime, payload).await? {
        merge_object(
            &mut result,
            json!({
                "status": "duplicate_wake",
                "event": existing,
            }),
        );
        return Ok(result);
    }

    let event = runtime
        .timeline_store
        .append_event(
            &payload.guild_id,
            &payload.voice_channel_id,
            json!({
                "event_kind": "wake_detected",
                "kind": "wake_detected",
                "job_id": job.id,
                "capture_run_id": payload.capture_run_id,
                "captureRunId": payload.capture_run_id,
                "guild_id": payload.guild_id,
                "guildId": payload.guild_id,
                "guild_slug": payload.guild_slug,
                "guildSlug": payload.guild_slug,
                "voice_channel_id": payload.voice_channel_id,
                "channelId": payload.voice_channel_id,
                "voice_channel_name": payload.voice_channel_name,
                "channelName": payload.voice_channel_name,
                "voice_channel_slug": payload.voice_channel_slug,
                "channelSlug": payload.voice_channel_slug,
                "voice_bot_id": payload.voice_bot_id,
                "botId": payload.voice_bot_id,
                "voice_bot_discord_user_id": payload.voice_bot_discord_user_id,
                "botUserId": payload.voice_bot_discord_user_id,
                "speaker_user_id": payload.speaker_user_id,
                "speakerId": payload.speaker_user_id,
                "speaker_label": payload.speaker_label,
                "speakerLabel": payload.speaker_label,
                "speaker_username": payload.speaker_username,
                "speakerUsername": payload.speaker_username,
                "probe_start_time": isoformat_z(Some(payload.probe_start_time)),
                "startedAt": isoformat_z(Some(payload.probe_start_time)),
                "probe_end_time": isoformat_z(Some(payload.probe_end_time)),
                "endedAt": isoformat_z(Some(payload.probe_end_time)),
                "probe_index": payload.probe_index,
                "probeIndex": payload.probe_index,
                "duration_ms": payload.duration_ms,
                "durationMs": payload.duration_ms,
                "source_audio_path": wav_path.display().to_string(),
                "sourceAudioPath": wav_path.display().to_string(),
                "audio_checksum": audio_checksum.clone(),
                "audioChecksum": audio_checksum,
                "audio_bytes": audio_bytes,
                "audioBytes": audio_bytes,
                "audio_format": payload.audio_format,
                "sample_rate_hz": payload.sample_rate_hz,
                "channels": payload.channels,
                "sample_width_bits": payload.sample_width_bits,
                "post_processing": payload.post_processing,
                "stream_id": payload.stream_id,
                "wake": wake_metadata,
                "wake_detected": true,
            }),
        )
        .await?;
    let route = schedule_from_wake_event(runtime, &event).await?;
    merge_object(
        &mut result,
        json!({
            "event": event,
            "route": route,
        }),
    );
    Ok(result)
}

async fn overlapping_wake_event(
    runtime: &Runtime,
    payload: &WakeProbePayload,
) -> Result<Option<Value>> {
    let mut kinds = BTreeSet::new();
    kinds.insert("wake_detected".to_string());
    let grace = config::wake_duplicate_overlap_grace_ms();
    let start = payload.probe_start_time - chrono::Duration::milliseconds(grace);
    let end = payload.probe_end_time + chrono::Duration::milliseconds(grace);
    Ok(runtime
        .timeline_store
        .load_events(
            &payload.guild_id,
            &payload.voice_channel_id,
            Some(start),
            Some(end),
            Some(&kinds),
            Some(&payload.capture_run_id),
            false,
        )
        .await?
        .into_iter()
        .find(|event| {
            first_value_string(event, &["speaker_user_id", "speakerId"]) == payload.speaker_user_id
                && event_overlaps(event, start, end)
        }))
}

fn event_overlaps(
    event: &Value,
    start: chrono::DateTime<chrono::Utc>,
    end: chrono::DateTime<chrono::Utc>,
) -> bool {
    let event_start = event_start(event).unwrap_or(start);
    let event_end = event_end(event).unwrap_or(event_start);
    event_start <= end && event_end >= start
}

fn merge_object(target: &mut Value, source: Value) {
    let Some(target) = target.as_object_mut() else {
        return;
    };
    let Some(source) = source.as_object() else {
        return;
    };
    for (key, value) in source {
        target.insert(key.clone(), value.clone());
    }
}
