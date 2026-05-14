use serde_json::{Value, json};

use crate::Result;
use crate::adapters::stt::{
    should_drop_low_confidence_transcription, stt_drop_decision, transcribe_file_result_sync,
};
use crate::adapters::wakeword::detect_wake_file_sync;
use crate::runtime::domain::interactions::{
    VOICE_ACTIVATION_LOOKBACK_SECONDS, evaluate_voice_command, validate_voice_command_result,
    voice_command_action,
};
use crate::runtime::timeline::{SpeechEventInput, event_start, sha256_file, string_field};
use crate::runtime::{AudioSegmentPayload, CommandRequest, Job, Runtime};

pub(crate) fn execute_segment_job(
    runtime: &Runtime,
    job: &Job,
    payload: &AudioSegmentPayload,
) -> Result<Value> {
    if let Some(event) = runtime.timeline_store.speech_event_for_segment(
        &payload.guild_id,
        &payload.voice_channel_id,
        &payload.capture_run_id,
        &payload.speaker_user_id,
        payload.segment_index,
    )? {
        let route = route_voice_command(runtime, job, &event)?;
        return Ok(json!({
            "kind": "audio_segment",
            "status": "already_transcribed",
            "event": event,
            "route": route,
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

    let stream_id = wake_stream_id(payload);
    let wake = detect_wake_file_sync(&wav_path, &stream_id, false)?;
    let wake_metadata = wake.to_json();
    merge_object(&mut capture, json!({"wake": wake_metadata.clone()}));

    let transcription = transcribe_file_result_sync(&wav_path)?;
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
            wake_metadata,
            ..Default::default()
        })?;
    let _ = runtime.timeline_store.set_occupancy(json!({
        "guild_id": payload.guild_id,
        "voice_channel_id": payload.voice_channel_id,
        "voice_channel_name": payload.voice_channel_name,
        "last_speech_at": payload.segment_end_time.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
    }));
    merge_object(
        &mut capture,
        json!({"status": "transcribed", "event": event}),
    );
    let route = route_voice_command(runtime, job, &event)?;
    if !route.is_null() {
        merge_object(&mut capture, json!({"route": route}));
    }
    Ok(capture)
}

fn route_voice_command(runtime: &Runtime, parent_job: &Job, event: &Value) -> Result<Value> {
    let existing = runtime
        .timeline_store
        .list_child_jobs(&parent_job.id)?
        .into_iter()
        .filter(|job| {
            matches!(
                job.kind,
                crate::runtime::JobKind::Command | crate::runtime::JobKind::ConfirmationRequired
            )
        })
        .map(|job| job.to_value())
        .collect::<Vec<_>>();
    if !existing.is_empty() {
        return Ok(json!({"status": "already_routed", "jobs": existing}));
    }

    let Some(started_at) = event_start(event) else {
        return Ok(Value::Null);
    };
    let recent_events = runtime.timeline_store.load_events(
        &string_field(event, "guild_id"),
        &string_field(event, "voice_channel_id"),
        Some(started_at - chrono::Duration::seconds(VOICE_ACTIVATION_LOOKBACK_SECONDS)),
        Some(started_at + chrono::Duration::seconds(1)),
        Some(&std::collections::BTreeSet::from([
            "speech_segment".to_string()
        ])),
        None,
        false,
    )?;
    let room = runtime.room_for_channel_ids(
        &string_field(event, "guild_id"),
        &string_field(event, "voice_channel_id"),
        Some(&string_field(event, "voice_channel_name")),
    );
    let room_status = runtime.status_for_room(&room);
    let result = evaluate_voice_command(event, &recent_events, &room_status);
    let (valid, reason) = validate_voice_command_result(&result);
    if !valid || voice_command_action(&result) != "dispatch_now" {
        return Ok(json!({
            "status": "not_routed",
            "valid": valid,
            "reason": reason,
            "result": result,
        }));
    }
    let duplicate = existing_command_jobs_for_dedupe(runtime, &result)?;
    if !duplicate.is_empty() {
        return Ok(json!({"status": "duplicate", "result": result, "jobs": duplicate}));
    }
    let command = CommandRequest::from_json(&result)?;
    let created = runtime.create_command_job_sync(command, Some(parent_job))?;
    Ok(json!({"status": "routed", "result": result, "created": created}))
}

fn existing_command_jobs_for_dedupe(runtime: &Runtime, result: &Value) -> Result<Vec<Value>> {
    let dedupe_hash = string_field(result, "dedupe_hash");
    if dedupe_hash.is_empty() {
        return Ok(Vec::new());
    }
    let guild_id = string_field(result, "guild_id");
    let channel_id = string_field(result, "voice_channel_id");
    Ok(runtime
        .timeline_store
        .list_jobs(Some(&guild_id), None)?
        .into_iter()
        .filter(|job| job.voice_channel_id == channel_id)
        .filter(|job| {
            job.command_value()
                .is_some_and(|command| string_field(&command, "dedupe_hash") == dedupe_hash)
        })
        .map(|job| job.to_value())
        .collect())
}

fn wake_stream_id(payload: &AudioSegmentPayload) -> String {
    format!(
        "{}:{}:{}",
        payload.guild_id, payload.voice_channel_id, payload.speaker_user_id
    )
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
