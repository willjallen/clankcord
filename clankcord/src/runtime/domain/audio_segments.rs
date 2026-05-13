use serde_json::{Value, json};

use crate::Result;
use crate::adapters::stt::{
    should_drop_low_confidence_transcription, stt_drop_decision, transcribe_file_result_sync,
};
use crate::runtime::domain::voice_commands::{
    ROUTER_LOOKBACK_SECONDS, acknowledgement_text_for_command, clean_question_text, dedupe_hash,
    evaluate_router_candidate, router_action, validate_router_result,
};
use crate::runtime::timeline::{SpeechEventInput, event_start, sha256_file, string_field};
use crate::runtime::util::first_non_empty;
use crate::runtime::{AudioSegmentPayload, Job, RouterCommand, Runtime};

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
                crate::runtime::JobKind::RouterCommand
                    | crate::runtime::JobKind::ConfirmationRequired
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
        Some(started_at - chrono::Duration::seconds(ROUTER_LOOKBACK_SECONDS)),
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
    let result = promote_general_voice_request(
        evaluate_router_candidate(event, &recent_events, &room_status, None),
        event,
    );
    let (valid, reason) = validate_router_result(&result);
    if !valid || router_action(&result) != "dispatch_now" {
        return Ok(json!({
            "status": "not_routed",
            "valid": valid,
            "reason": reason,
            "result": result,
        }));
    }
    let command = RouterCommand::from_json(&result)?;
    let created = runtime.create_router_command_job_sync(command, Some(parent_job))?;
    Ok(json!({"status": "routed", "result": result, "created": created}))
}

fn promote_general_voice_request(mut result: Value, event: &Value) -> Value {
    if router_action(&result) == "dispatch_now" {
        return result;
    }
    if result.get("wake_phrase_detected").and_then(Value::as_bool) != Some(true) {
        return result;
    }
    let instruction_text = string_field(&result, "instruction_text");
    let request = clean_question_text(&instruction_text);
    if request.split_whitespace().count() < 3 && !request.contains('?') {
        return result;
    }
    let guild_id = first_non_empty([
        string_field(&result, "guild_id"),
        string_field(event, "guild_id"),
        string_field(event, "guildId"),
    ]);
    let channel_id = first_non_empty([
        string_field(&result, "voice_channel_id"),
        string_field(event, "voice_channel_id"),
        string_field(event, "channelId"),
    ]);
    let requested_by_user_id = first_non_empty([
        string_field(&result, "requested_by_user_id"),
        string_field(event, "speaker_user_id"),
        string_field(event, "speakerId"),
    ]);
    let requested_by_speaker_label = first_non_empty([
        string_field(&result, "requested_by_speaker_label"),
        string_field(event, "speaker_label"),
        string_field(event, "speakerLabel"),
        requested_by_user_id.clone(),
    ]);
    let mut source_event_ids = result
        .get("source_event_ids")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if source_event_ids.is_empty() {
        let event_id = first_non_empty([
            string_field(event, "event_id"),
            string_field(event, "eventId"),
        ]);
        if !event_id.is_empty() {
            source_event_ids.push(event_id);
        }
    }
    let arguments = json!({
        "request": request,
        "raw_text": string_field(&result, "candidate_text"),
        "activated_text": string_field(&result, "activated_text"),
        "instruction_text": instruction_text,
        "respond_in": "agent_chat",
    });
    let dedupe = dedupe_hash(
        &guild_id,
        &channel_id,
        &source_event_ids.join("|"),
        "voice_agent_task",
        &arguments,
    );
    merge_object(
        &mut result,
        json!({
            "action": "dispatch_now",
            "is_command": true,
            "confidence": 0.76,
            "command_kind": "voice_agent_task",
            "guild_id": guild_id,
            "voice_channel_id": channel_id,
            "requested_by_user_id": requested_by_user_id,
            "requested_by_speaker_label": requested_by_speaker_label,
            "source_event_ids": source_event_ids,
            "arguments": arguments,
            "requires_confirmation": false,
            "acknowledgement_text": acknowledgement_text_for_command("voice_agent_task"),
            "reason": "Native router promoted a wake-addressed non-control request to voice_agent_task.",
            "dedupe_hash": dedupe,
        }),
    );
    result
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
