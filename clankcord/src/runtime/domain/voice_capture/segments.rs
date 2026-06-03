use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::Result;
use crate::adapters::stt::{
    SttHttpStatusError, TranscriptionResult, TranscriptionSpan, TranscriptionWord,
    should_drop_low_confidence_transcription, stt_drop_decision,
    transcribe_file_with_source_result_sync,
};
use crate::runtime::timeline::store::TranscriptionSlotRecord;
use crate::runtime::timeline::{SpeechEventInput, read_wav_mono, sha256_file};
use crate::runtime::{
    AudioSegmentPayload, Runtime, TranscriptionMuxPayload, TranscriptionMuxPlanPayload,
};

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
    retryable_stt_error_class_from_text(error).is_some()
}

pub(crate) fn retry_delay_seconds(attempts: i64) -> i64 {
    let source = crate::config::active_transcription_source().ok();
    let initial = source
        .as_ref()
        .map(|source| source.config.retry_backoff_initial_seconds)
        .unwrap_or(5);
    let max = source
        .as_ref()
        .map(|source| source.config.retry_backoff_max_seconds)
        .unwrap_or(300);
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
    job: &crate::runtime::Job,
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

    let priority = runtime
        .timeline_store
        .audio_segment_transcription_priority(payload)
        .await?;
    let slot = runtime
        .timeline_store
        .create_transcription_slot_for_audio_segment(&job.id, payload, priority)
        .await?;
    let planner_delay_ms = if priority >= 1000 {
        0
    } else {
        crate::config::transcription_mux_batch_delay_ms()
    };
    let planner_job = runtime
        .timeline_store
        .ensure_transcription_mux_plan_job(
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

pub(crate) async fn execute_transcription_mux_plan_job(
    runtime: &Runtime,
    _job: &crate::runtime::Job,
    payload: &TranscriptionMuxPlanPayload,
) -> Result<Value> {
    crate::config::transcription_source(&payload.transcription_source_id)?;
    runtime
        .timeline_store
        .plan_transcription_mux_jobs(&payload.transcription_source_id)
        .await
}

pub(crate) async fn execute_transcription_mux_job(
    runtime: &Runtime,
    job: &crate::runtime::Job,
    payload: &TranscriptionMuxPayload,
) -> Result<Value> {
    let source = crate::config::transcription_source(&payload.transcription_source_id)?;
    let slots = runtime
        .timeline_store
        .start_transcription_slots_for_mux(&job.id, &source.id)
        .await?;
    if slots.is_empty() {
        return Ok(json!({
            "kind": "transcription_mux",
            "status": "idle",
            "transcription_source_id": source.id,
        }));
    }
    let mux = match build_mux_audio(runtime, job, &slots).await {
        Ok(mux) => mux,
        Err(error) => {
            runtime
                .timeline_store
                .fail_transcription_slots_for_mux(&job.id, &error.to_string())
                .await?;
            return Err(error);
        }
    };
    let transcription = match transcribe_file_with_source_result_sync(&mux.path, &source) {
        Ok(transcription) => transcription,
        Err(error) => {
            let Some(class) = retryable_stt_error_class(&error) else {
                runtime
                    .timeline_store
                    .fail_transcription_slots_for_mux(&job.id, &error.to_string())
                    .await?;
                return Err(error);
            };
            return Err(RetryableAudioSegmentError::new(class, error).into());
        }
    };
    let assignments = match assign_transcription_to_slots(&transcription, &mux.slots) {
        Ok(assignments) => assignments,
        Err(error) => {
            runtime
                .timeline_store
                .fail_transcription_slots_for_mux(&job.id, &error.to_string())
                .await?;
            return Err(error);
        }
    };
    let mut events = Vec::new();
    for slot in &mux.slots {
        let assignment = assignments.get(&slot.slot_id).cloned().unwrap_or_default();
        let text = assignment.text.trim().to_string();
        let mut metadata = transcription.metadata.clone();
        merge_object(
            &mut metadata,
            json!({
                "transcription_source_id": slot.transcription_source_id,
                "mux_job_id": job.id,
                "mux_stream_id": mux.stream_id,
                "mux_audio_path": mux.path.display().to_string(),
                "mux_audio_checksum": mux.checksum,
                "mux_start_ms": slot.mux_start_ms,
                "mux_end_ms": slot.mux_end_ms,
                "slot_id": slot.slot_id,
                "provider": slot.provider,
                "model": slot.model,
            }),
        );
        if text.is_empty() {
            runtime
                .timeline_store
                .complete_transcription_slot(&slot.slot_id, "", "")
                .await?;
            continue;
        }
        if should_drop_low_confidence_transcription(Some(&metadata), None, None) {
            let decision = stt_drop_decision(Some(&metadata), None, None);
            runtime
                .timeline_store
                .complete_transcription_slot(&slot.slot_id, "", "")
                .await?;
            events.push(json!({
                "slot_id": slot.slot_id,
                "status": "dropped_low_confidence",
                "decision": decision,
                "text_preview": text.chars().take(120).collect::<String>(),
            }));
            continue;
        }
        if let Some(event) = runtime
            .timeline_store
            .speech_event_for_segment(
                &slot.guild_id,
                &slot.voice_channel_id,
                &slot.capture_run_id,
                &slot.speaker_user_id,
                slot.segment_index,
            )
            .await?
        {
            let event_id =
                crate::runtime::util::first_value_string(&event, &["event_id", "eventId"]);
            runtime
                .timeline_store
                .complete_transcription_slot(&slot.slot_id, &event_id, &text)
                .await?;
            events.push(json!({
                "slot_id": slot.slot_id,
                "status": "already_transcribed",
                "event": event,
            }));
            continue;
        }
        let (start_time, end_time) = assigned_slot_times(slot, &assignment);
        let event = runtime
            .timeline_store
            .append_speech_event(SpeechEventInput {
                guild_id: slot.guild_id.clone(),
                guild_slug: slot.guild_slug.clone(),
                voice_channel_id: slot.voice_channel_id.clone(),
                voice_channel_name: slot.voice_channel_name.clone(),
                voice_channel_slug: slot.voice_channel_slug.clone(),
                capture_run_id: slot.capture_run_id.clone(),
                voice_bot_id: slot.voice_bot_id.clone(),
                voice_bot_discord_user_id: slot.voice_bot_discord_user_id.clone(),
                speaker_user_id: slot.speaker_user_id.clone(),
                speaker_label: slot.speaker_label.clone(),
                speaker_username: slot.speaker_username.clone(),
                segment_start_time: start_time,
                segment_end_time: end_time,
                text_draft: text.clone(),
                source_audio_path: slot.source_audio_path.clone(),
                audio_checksum: slot.audio_checksum.clone(),
                segment_index: slot.segment_index,
                duration_ms: (end_time - start_time).num_milliseconds().max(0),
                transcription_source_id: slot.transcription_source_id.clone(),
                stt_provider: slot.provider.clone(),
                stt_model: slot.model.clone(),
                stt_metadata: metadata,
                ..Default::default()
            })
            .await?;
        let event_id = crate::runtime::util::first_value_string(&event, &["event_id", "eventId"]);
        runtime
            .timeline_store
            .complete_transcription_slot(&slot.slot_id, &event_id, &text)
            .await?;
        let _ = runtime
            .timeline_store
            .set_occupancy(json!({
                "guild_id": slot.guild_id,
                "voice_channel_id": slot.voice_channel_id,
                "voice_channel_name": slot.voice_channel_name,
                "last_speech_at": slot.segment_end_time.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            }))
            .await;
        events.push(json!({
            "slot_id": slot.slot_id,
            "status": "transcribed",
            "event": event,
        }));
    }
    let next_plan_job = runtime
        .timeline_store
        .ensure_transcription_mux_plan_job(&source.id, 0)
        .await?;
    Ok(json!({
        "kind": "transcription_mux",
        "status": "transcribed",
        "transcription_source_id": source.id,
        "mux_stream_id": mux.stream_id,
        "mux_audio_path": mux.path.display().to_string(),
        "mux_audio_checksum": mux.checksum,
        "slot_count": mux.slots.len(),
        "next_transcription_mux_plan_job": next_plan_job.map(|job| job.to_value()),
        "events": events,
    }))
}

#[derive(Debug, Clone)]
struct BuiltMuxAudio {
    stream_id: String,
    path: PathBuf,
    checksum: String,
    slots: Vec<TranscriptionSlotRecord>,
}

#[derive(Debug, Clone, Default)]
struct AssignedTranscript {
    text: String,
    start_mux_ms: Option<i64>,
    end_mux_ms: Option<i64>,
}

async fn build_mux_audio(
    runtime: &Runtime,
    job: &crate::runtime::Job,
    slots: &[TranscriptionSlotRecord],
) -> Result<BuiltMuxAudio> {
    let Some(first) = slots.first() else {
        anyhow::bail!("transcription mux job {} has no slots", job.id);
    };
    let sample_rate = first.sample_rate_hz;
    if sample_rate == 0 {
        anyhow::bail!(
            "transcription slot {} has invalid sample rate",
            first.slot_id
        );
    }
    let guard_ms = crate::config::transcription_mux_guard_ms();
    let guard_samples = samples_for_ms(sample_rate, guard_ms);
    let mut mixed = Vec::<i16>::new();
    let mut updated_slots = Vec::new();
    let stream_id = format!("mux:{}:{}", first.transcription_source_id, job.id);
    for slot in slots {
        if slot.sample_rate_hz != sample_rate {
            anyhow::bail!(
                "transcription slot {} sample rate {} does not match mux sample rate {}",
                slot.slot_id,
                slot.sample_rate_hz,
                sample_rate
            );
        }
        if !mixed.is_empty() && guard_samples > 0 {
            mixed.extend(std::iter::repeat(0).take(guard_samples));
        }
        let mux_start_ms = ms_for_samples(sample_rate, mixed.len());
        let samples = read_wav_mono(&slot.source_audio_path, sample_rate)?;
        mixed.extend(samples);
        let mux_end_ms = ms_for_samples(sample_rate, mixed.len());
        if guard_samples > 0 {
            mixed.extend(std::iter::repeat(0).take(guard_samples));
        }
        runtime
            .timeline_store
            .update_transcription_slot_mux_offsets(
                &slot.slot_id,
                &stream_id,
                mux_start_ms,
                mux_end_ms,
                if mux_start_ms == 0 { 0 } else { guard_ms },
                guard_ms,
            )
            .await?;
        let mut updated = slot.clone();
        updated.mux_stream_id = stream_id.clone();
        updated.mux_start_ms = Some(mux_start_ms);
        updated.mux_end_ms = Some(mux_end_ms);
        updated_slots.push(updated);
    }
    let output_dir = runtime
        .timeline_store
        .channel_dir(&first.guild_id, &first.voice_channel_id)
        .join("jobs")
        .join(&job.id);
    fs::create_dir_all(&output_dir)?;
    let path = output_dir.join("mux.wav");
    write_mono_wav(&path, sample_rate, &mixed)?;
    let checksum = sha256_file(&path)?;
    Ok(BuiltMuxAudio {
        stream_id,
        path,
        checksum,
        slots: updated_slots,
    })
}

fn write_mono_wav(path: &Path, sample_rate: u32, samples: &[i16]) -> Result<()> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec)?;
    for sample in samples {
        writer.write_sample(*sample)?;
    }
    writer.finalize()?;
    Ok(())
}

fn assign_transcription_to_slots(
    transcription: &TranscriptionResult,
    slots: &[TranscriptionSlotRecord],
) -> Result<BTreeMap<String, AssignedTranscript>> {
    let mut assignments = slots
        .iter()
        .map(|slot| (slot.slot_id.clone(), AssignedTranscript::default()))
        .collect::<BTreeMap<_, _>>();
    if !transcription.words.is_empty() {
        assign_words_to_slots(&transcription.words, slots, &mut assignments);
        return Ok(assignments);
    }
    if !transcription.segments.is_empty() {
        assign_segments_to_slots(&transcription.segments, slots, &mut assignments)?;
        return Ok(assignments);
    }
    if slots.len() == 1 {
        let slot = &slots[0];
        assignments.insert(
            slot.slot_id.clone(),
            AssignedTranscript {
                text: transcription.text.trim().to_string(),
                start_mux_ms: slot.mux_start_ms,
                end_mux_ms: slot.mux_end_ms,
            },
        );
        return Ok(assignments);
    }
    anyhow::bail!(
        "transcription provider returned no timestamps for {} mux slots",
        slots.len()
    );
}

fn assign_words_to_slots(
    words: &[TranscriptionWord],
    slots: &[TranscriptionSlotRecord],
    assignments: &mut BTreeMap<String, AssignedTranscript>,
) {
    let mut last_slot_id = String::new();
    for word in words {
        if word.kind == "audio_event" || word.text.trim().is_empty() {
            continue;
        }
        let slot = word
            .start_seconds
            .or(word.end_seconds)
            .and_then(|seconds| slot_for_mux_ms(slots, seconds_to_ms(seconds)))
            .or_else(|| {
                if last_slot_id.is_empty() {
                    None
                } else {
                    slots.iter().find(|slot| slot.slot_id == last_slot_id)
                }
            });
        let Some(slot) = slot else {
            continue;
        };
        last_slot_id = slot.slot_id.clone();
        let assignment = assignments.entry(slot.slot_id.clone()).or_default();
        append_assignment_text(&mut assignment.text, &word.text);
        if let Some(start) = word.start_seconds.map(seconds_to_ms) {
            assignment.start_mux_ms =
                Some(assignment.start_mux_ms.map_or(start, |old| old.min(start)));
        }
        if let Some(end) = word.end_seconds.map(seconds_to_ms) {
            assignment.end_mux_ms = Some(assignment.end_mux_ms.map_or(end, |old| old.max(end)));
        }
    }
}

fn assign_segments_to_slots(
    segments: &[TranscriptionSpan],
    slots: &[TranscriptionSlotRecord],
    assignments: &mut BTreeMap<String, AssignedTranscript>,
) -> Result<()> {
    for segment in segments {
        let start_ms = segment.start_seconds.map(seconds_to_ms);
        let end_ms = segment.end_seconds.map(seconds_to_ms);
        let reference_ms = match (start_ms, end_ms) {
            (Some(start), Some(end)) => (start + end) / 2,
            (Some(start), None) => start,
            (None, Some(end)) => end,
            (None, None) if slots.len() == 1 => slots[0].mux_start_ms.unwrap_or(0),
            (None, None) => anyhow::bail!("transcription segment has no timestamp"),
        };
        let Some(slot) = slot_for_mux_ms(slots, reference_ms) else {
            continue;
        };
        let assignment = assignments.entry(slot.slot_id.clone()).or_default();
        append_assignment_text(&mut assignment.text, &segment.text);
        if let Some(start) = start_ms {
            assignment.start_mux_ms =
                Some(assignment.start_mux_ms.map_or(start, |old| old.min(start)));
        }
        if let Some(end) = end_ms {
            assignment.end_mux_ms = Some(assignment.end_mux_ms.map_or(end, |old| old.max(end)));
        }
    }
    Ok(())
}

fn slot_for_mux_ms(
    slots: &[TranscriptionSlotRecord],
    mux_ms: i64,
) -> Option<&TranscriptionSlotRecord> {
    slots.iter().find(|slot| {
        let start = slot.mux_start_ms.unwrap_or(i64::MIN);
        let end = slot.mux_end_ms.unwrap_or(i64::MAX);
        mux_ms >= start && mux_ms <= end
    })
}

fn append_assignment_text(target: &mut String, text: &str) {
    if text.is_empty() {
        return;
    }
    if target.is_empty()
        || text.starts_with(char::is_whitespace)
        || matches!(text, "." | "," | "!" | "?" | ":" | ";" | ")" | "]")
    {
        target.push_str(text);
    } else {
        target.push(' ');
        target.push_str(text);
    }
}

fn assigned_slot_times(
    slot: &TranscriptionSlotRecord,
    assignment: &AssignedTranscript,
) -> (chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>) {
    let mux_start = slot.mux_start_ms.unwrap_or(0);
    let slot_start = assignment.start_mux_ms.unwrap_or(mux_start);
    let slot_end = assignment
        .end_mux_ms
        .unwrap_or(slot.mux_end_ms.unwrap_or(mux_start));
    let start_offset = slot_start
        .saturating_sub(mux_start)
        .clamp(0, slot.duration_ms);
    let end_offset = slot_end
        .saturating_sub(mux_start)
        .clamp(start_offset, slot.duration_ms);
    (
        slot.segment_start_time + chrono::Duration::milliseconds(start_offset),
        slot.segment_start_time + chrono::Duration::milliseconds(end_offset),
    )
}

fn samples_for_ms(sample_rate: u32, ms: i64) -> usize {
    ((sample_rate as i64).saturating_mul(ms).max(0) / 1000) as usize
}

fn ms_for_samples(sample_rate: u32, samples: usize) -> i64 {
    ((samples as i64).saturating_mul(1000)) / sample_rate as i64
}

fn seconds_to_ms(seconds: f64) -> i64 {
    (seconds * 1000.0).round() as i64
}

fn retryable_stt_error_class(error: &anyhow::Error) -> Option<RetryableSttErrorClass> {
    error
        .chain()
        .find_map(|cause| {
            if let Some(error) = cause.downcast_ref::<SttHttpStatusError>() {
                return retryable_stt_status_class(error.status());
            }
            let Some(error) = cause.downcast_ref::<reqwest::Error>() else {
                return None;
            };
            if error.is_timeout() {
                return Some(RetryableSttErrorClass::Timeout);
            }
            if error.is_connect() {
                return Some(RetryableSttErrorClass::Connection);
            }
            if error.is_body() {
                return Some(RetryableSttErrorClass::Connection);
            }
            match error.status() {
                Some(reqwest::StatusCode::TOO_MANY_REQUESTS) => {
                    Some(RetryableSttErrorClass::RateLimit)
                }
                Some(reqwest::StatusCode::REQUEST_TIMEOUT) => Some(RetryableSttErrorClass::Timeout),
                Some(status) if status.is_server_error() => Some(RetryableSttErrorClass::Server),
                _ => None,
            }
        })
        .or_else(|| retryable_stt_error_class_from_text(&error.to_string()))
}

fn retryable_stt_error_class_from_text(error: &str) -> Option<RetryableSttErrorClass> {
    let error = error.trim().to_ascii_lowercase();
    if error.is_empty() {
        return None;
    }
    if error.starts_with("retryable stt timeout error:")
        || error.contains("operation timed out")
        || error.contains("timed out")
    {
        return Some(RetryableSttErrorClass::Timeout);
    }
    if error.starts_with("retryable stt connection error:")
        || error.contains("request or response body error")
        || error.contains("connection refused")
        || error.contains("connection reset")
        || error.contains("connection closed")
        || error.contains("connect error")
    {
        return Some(RetryableSttErrorClass::Connection);
    }
    if error.starts_with("retryable stt rate_limit error:")
        || error.contains("too many requests")
        || error.contains("quota_exceeded")
    {
        return Some(RetryableSttErrorClass::RateLimit);
    }
    if error.starts_with("retryable stt server error:")
        || error.contains("http status server error")
    {
        return Some(RetryableSttErrorClass::Server);
    }
    retryable_stt_status_code_from_text(&error).and_then(|code| {
        reqwest::StatusCode::from_u16(code)
            .ok()
            .and_then(retryable_stt_status_class)
    })
}

fn retryable_stt_status_class(status: reqwest::StatusCode) -> Option<RetryableSttErrorClass> {
    match status {
        reqwest::StatusCode::TOO_MANY_REQUESTS => Some(RetryableSttErrorClass::RateLimit),
        reqwest::StatusCode::REQUEST_TIMEOUT => Some(RetryableSttErrorClass::Timeout),
        status if status.is_server_error() => Some(RetryableSttErrorClass::Server),
        _ => None,
    }
}

fn retryable_stt_status_code_from_text(error: &str) -> Option<u16> {
    let bytes = error.as_bytes();
    if bytes.len() < 3 {
        return None;
    }
    for index in 0..=bytes.len() - 3 {
        if !bytes[index].is_ascii_digit()
            || !bytes[index + 1].is_ascii_digit()
            || !bytes[index + 2].is_ascii_digit()
        {
            continue;
        }
        let previous = index.checked_sub(1).and_then(|idx| bytes.get(idx).copied());
        let next = bytes.get(index + 3).copied();
        if previous.is_some_and(|byte| byte.is_ascii_digit())
            || next.is_some_and(|byte| byte.is_ascii_digit())
        {
            continue;
        }
        let code = ((bytes[index] - b'0') as u16 * 100)
            + ((bytes[index + 1] - b'0') as u16 * 10)
            + (bytes[index + 2] - b'0') as u16;
        if code == 408 || code == 429 || (500..=599).contains(&code) {
            return Some(code);
        }
    }
    None
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
