use std::fmt;

use serde_json::{Value, json};

use crate::Result;
use crate::adapters::stt::{
    should_drop_low_confidence_transcription, stt_drop_decision, transcribe_file_result_sync,
};
use crate::runtime::timeline::{SpeechEventInput, sha256_file};
use crate::runtime::{AudioSegmentPayload, Runtime};

pub(crate) struct AudioSegmentRetryPlan {
    pub delay_for_attempt: fn(i64) -> chrono::Duration,
    pub error: String,
    pub log_prefix: &'static str,
}

#[derive(Debug)]
pub(crate) struct RetryableAudioSegmentError {
    class: RetryableSttErrorClass,
    message: String,
}

impl RetryableAudioSegmentError {
    fn new(class: RetryableSttErrorClass, error: anyhow::Error) -> Self {
        Self {
            class,
            message: error.to_string(),
        }
    }
}

impl fmt::Display for RetryableAudioSegmentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "retryable STT {} error: {}",
            self.class.as_str(),
            self.message
        )
    }
}

impl std::error::Error for RetryableAudioSegmentError {}

#[derive(Debug, Clone, Copy)]
enum RetryableSttErrorClass {
    Timeout,
    Connection,
    RateLimit,
    Server,
}

impl RetryableSttErrorClass {
    fn as_str(self) -> &'static str {
        match self {
            Self::Timeout => "timeout",
            Self::Connection => "connection",
            Self::RateLimit => "rate_limit",
            Self::Server => "server",
        }
    }
}

pub(crate) fn is_retryable_audio_segment_error(error: &anyhow::Error) -> bool {
    error.downcast_ref::<RetryableAudioSegmentError>().is_some()
}

pub(crate) fn is_retryable_audio_segment_error_text(error: &str) -> bool {
    let error = error.trim().to_ascii_lowercase();
    if error.is_empty() {
        return false;
    }
    error.starts_with("retryable stt timeout error:")
        || error.starts_with("retryable stt connection error:")
        || error.starts_with("retryable stt rate_limit error:")
        || error.starts_with("retryable stt server error:")
        || error.contains("operation timed out")
        || error.contains("timed out")
        || error.contains("connection refused")
        || error.contains("connection reset")
        || error.contains("connection closed")
        || error.contains("connect error")
        || error.contains("too many requests")
        || error.contains("client error (429")
        || error.contains("429 too many requests")
        || error.contains("408 request timeout")
        || error.contains("http status server error")
}

pub(crate) fn retry_delay_seconds(attempts: i64) -> i64 {
    let initial = crate::config::stt_retry_backoff_initial_seconds();
    let max = crate::config::stt_retry_backoff_max_seconds();
    let exponent = attempts.saturating_sub(1).clamp(0, 30) as u32;
    initial
        .saturating_mul(2_i64.saturating_pow(exponent))
        .min(max)
}

pub(crate) fn retry_plan(error: anyhow::Error) -> AudioSegmentRetryPlan {
    AudioSegmentRetryPlan {
        delay_for_attempt: retry_delay,
        error: error.to_string(),
        log_prefix: "audio segment job retry scheduled",
    }
}

fn retry_delay(attempts: i64) -> chrono::Duration {
    chrono::Duration::seconds(retry_delay_seconds(attempts))
}

pub(crate) async fn execute_segment_job(
    runtime: &Runtime,
    _job: &crate::runtime::Job,
    payload: &AudioSegmentPayload,
) -> Result<Value> {
    if let Some(event) = runtime
        .timeline_store
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

    let mut capture = json!({
        "kind": "audio_segment",
        "status": "artifact_ready",
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
    });

    let transcription = match transcribe_file_result_sync(&wav_path) {
        Ok(transcription) => transcription,
        Err(error) => {
            let Some(class) = retryable_stt_error_class(&error) else {
                return Err(error);
            };
            return Err(RetryableAudioSegmentError::new(class, error).into());
        }
    };
    let text = transcription.text.trim().to_string();
    let stt_metadata = transcription.metadata;
    if text.is_empty() {
        merge_object(
            &mut capture,
            json!({"status": "empty_transcript", "stt": stt_metadata}),
        );
        return Ok(capture);
    }
    if should_drop_low_confidence_transcription(Some(&stt_metadata), None, None) {
        let decision = stt_drop_decision(Some(&stt_metadata), None, None);
        merge_object(
            &mut capture,
            json!({
                "status": "dropped_low_confidence",
                "decision": decision,
                "text_preview": text.chars().take(120).collect::<String>(),
            }),
        );
        return Ok(capture);
    }

    let event = runtime
        .timeline_store
        .append_speech_event(SpeechEventInput {
            guild_id: payload.guild_id.clone(),
            guild_slug: payload.guild_slug.clone(),
            voice_channel_id: payload.voice_channel_id.clone(),
            voice_channel_name: payload.voice_channel_name.clone(),
            voice_channel_slug: payload.voice_channel_slug.clone(),
            capture_run_id: payload.capture_run_id.clone(),
            voice_bot_id: payload.voice_bot_id.clone(),
            voice_bot_discord_user_id: payload.voice_bot_discord_user_id.clone(),
            speaker_user_id: payload.speaker_user_id.clone(),
            speaker_label: payload.speaker_label.clone(),
            speaker_username: payload.speaker_username.clone(),
            segment_start_time: payload.segment_start_time,
            segment_end_time: payload.segment_end_time,
            text_draft: text,
            source_audio_path: wav_path,
            audio_checksum,
            segment_index: payload.segment_index,
            duration_ms: payload.duration_ms,
            stt_metadata,
            ..Default::default()
        })
        .await?;
    let _ = runtime.timeline_store.set_occupancy(json!({
        "guild_id": payload.guild_id,
        "voice_channel_id": payload.voice_channel_id,
        "voice_channel_name": payload.voice_channel_name,
        "last_speech_at": payload.segment_end_time.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
    }))
    .await;
    merge_object(
        &mut capture,
        json!({"status": "transcribed", "event": event}),
    );
    Ok(capture)
}

fn retryable_stt_error_class(error: &anyhow::Error) -> Option<RetryableSttErrorClass> {
    error.chain().find_map(|cause| {
        let Some(error) = cause.downcast_ref::<reqwest::Error>() else {
            return None;
        };
        if error.is_timeout() {
            return Some(RetryableSttErrorClass::Timeout);
        }
        if error.is_connect() {
            return Some(RetryableSttErrorClass::Connection);
        }
        match error.status() {
            Some(reqwest::StatusCode::TOO_MANY_REQUESTS) => Some(RetryableSttErrorClass::RateLimit),
            Some(reqwest::StatusCode::REQUEST_TIMEOUT) => Some(RetryableSttErrorClass::Timeout),
            Some(status) if status.is_server_error() => Some(RetryableSttErrorClass::Server),
            _ => None,
        }
    })
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
