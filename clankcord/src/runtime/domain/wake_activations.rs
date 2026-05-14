use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{Value, json};

use crate::Result;
use crate::runtime::domain::interactions::{
    evaluate_voice_command, validate_voice_command_result, voice_command_action,
};
use crate::runtime::timeline::{
    event_end, event_speaker, event_start, first_value_string, isoformat_z, new_id, parse_instant,
    utc_now,
};
use crate::runtime::{
    DiscordVoicePlaybackCue, Job, JobKind, JobState, Runtime, WakeActivationPayload,
};

const DEFAULT_LOOKBACK_SECONDS: i64 = 30;
const DEFAULT_MIN_POST_SECONDS: i64 = 5;
const DEFAULT_IDLE_SECONDS: i64 = 5;
const DEFAULT_STT_FLUSH_GRACE_SECONDS: i64 = 2;
const DEFAULT_MAX_WINDOW_SECONDS: i64 = 60;
const DEFAULT_ADDITIVE_PREEMPT_SECONDS: i64 = 10;
const DEFAULT_INDEPENDENT_AFTER_SECONDS: i64 = 45;

pub fn event_has_wake(event: &Value) -> bool {
    event
        .get("wake")
        .and_then(|wake| wake.get("wake"))
        .and_then(Value::as_bool)
        == Some(true)
        || event.get("wake_detected").and_then(Value::as_bool) == Some(true)
}

pub fn schedule_from_wake_event(runtime: &Runtime, event: &Value) -> Result<Value> {
    if !event_has_wake(event) {
        return Ok(Value::Null);
    }
    let guild_id = first_value_string(event, &["guild_id", "guildId"]);
    let voice_channel_id = first_value_string(event, &["voice_channel_id", "channelId"]);
    if guild_id.is_empty() || voice_channel_id.is_empty() {
        anyhow::bail!("wake event is missing guild/channel identity");
    }
    let wake_started_at = event_start(event).unwrap_or_else(utc_now);
    let wake_ended_at = event_end(event).unwrap_or(wake_started_at);
    let wake_event_id = first_value_string(event, &["event_id", "eventId"]);
    if wake_event_id.is_empty() {
        anyhow::bail!("wake event is missing event_id");
    }
    if let Some(existing) =
        activation_for_wake_event(runtime, &guild_id, &voice_channel_id, &wake_event_id)?
    {
        return Ok(json!({
            "status": "duplicate",
            "job": existing.to_value(),
        }));
    }
    let speaker_user_id = first_value_string(event, &["speaker_user_id", "speakerId", "user_id"]);
    let speaker_label = non_empty(
        first_value_string(event, &["speaker_label", "speakerLabel"]),
        event_speaker(event),
    );

    if let Some(existing) =
        activation_followup_target(runtime, &guild_id, &voice_channel_id, wake_started_at)?
    {
        let descendants = descendant_jobs(runtime, &existing.id)?;
        if descendants.is_empty() {
            let existing = amend_activation_job(
                runtime,
                existing,
                &wake_event_id,
                wake_started_at,
                wake_ended_at,
                speaker_user_id,
                speaker_label,
            )?;
            let _ = runtime.create_voice_playback_job_for_channel(
                &guild_id,
                &voice_channel_id,
                &existing.requested_by_user_id,
                DiscordVoicePlaybackCue::Preempt,
                "wake_activation_amended",
                &existing.id,
            )?;
            return Ok(json!({
                "status": "amended",
                "job": existing.to_value(),
            }));
        }
        let cancelled = cancel_job_tree(runtime, &existing)?;
        let replacement = replacement_activation_job(
            runtime,
            &existing,
            &wake_event_id,
            wake_started_at,
            wake_ended_at,
            speaker_user_id,
            speaker_label,
        )?;
        let _ = runtime.create_voice_playback_job_for_channel(
            &guild_id,
            &voice_channel_id,
            &replacement.requested_by_user_id,
            DiscordVoicePlaybackCue::Preempt,
            "wake_activation_replaced",
            &replacement.id,
        )?;
        runtime.timeline_store.append_event(
            &guild_id,
            &voice_channel_id,
            json!({
                "event_kind": "wake_activation_replaced",
                "kind": "wake_activation_replaced",
                "activation_id": replacement.wake_activation_payload().map(|payload| payload.activation_id.clone()).unwrap_or_default(),
                "replaced_job_id": existing.id.clone(),
                "replacement_job_id": replacement.id.clone(),
                "wake_event_id": wake_event_id,
                "cancelled_job_ids": cancelled,
            }),
        )?;
        return Ok(json!({
            "status": "replaced",
            "job": replacement.to_value(),
            "replaced_job_id": existing.id.clone(),
            "cancelled_job_ids": cancelled,
        }));
    }

    let payload = WakeActivationPayload {
        activation_id: new_id("act"),
        guild_id: guild_id.clone(),
        voice_channel_id: voice_channel_id.clone(),
        voice_channel_name: first_value_string(event, &["voice_channel_name", "channelName"]),
        speaker_user_id,
        speaker_label,
        wake_event_id: wake_event_id.clone(),
        wake_started_at: isoformat_z(Some(wake_started_at)),
        wake_ended_at: isoformat_z(Some(wake_ended_at)),
        latest_wake_event_id: wake_event_id,
        latest_wake_at: isoformat_z(Some(wake_started_at)),
        lookback_seconds: env_i64(
            "CLANKCORD_WAKE_ACTIVATION_LOOKBACK_SECONDS",
            DEFAULT_LOOKBACK_SECONDS,
        ),
        min_post_seconds: env_i64(
            "CLANKCORD_WAKE_ACTIVATION_MIN_POST_SECONDS",
            DEFAULT_MIN_POST_SECONDS,
        ),
        speaker_idle_seconds: env_i64(
            "CLANKCORD_WAKE_ACTIVATION_IDLE_SECONDS",
            DEFAULT_IDLE_SECONDS,
        ),
        stt_flush_grace_seconds: env_i64(
            "CLANKCORD_WAKE_ACTIVATION_STT_FLUSH_GRACE_SECONDS",
            DEFAULT_STT_FLUSH_GRACE_SECONDS,
        ),
        max_window_seconds: env_i64(
            "CLANKCORD_WAKE_ACTIVATION_MAX_SECONDS",
            DEFAULT_MAX_WINDOW_SECONDS,
        ),
        additive_preempt_seconds: env_i64(
            "CLANKCORD_WAKE_ACTIVATION_PREEMPT_SECONDS",
            DEFAULT_ADDITIVE_PREEMPT_SECONDS,
        ),
        independent_after_seconds: env_i64(
            "CLANKCORD_WAKE_ACTIVATION_INDEPENDENT_AFTER_SECONDS",
            DEFAULT_INDEPENDENT_AFTER_SECONDS,
        ),
        amended_wake_event_ids: Vec::new(),
        replacement_of_job_ids: Vec::new(),
    };
    let mut job = Job::wake_activation(payload.clone());
    job.next_run_at = Some(ready_at_string(
        wake_started_at,
        wake_ended_at,
        payload.min_post_seconds,
        payload.speaker_idle_seconds,
        payload.stt_flush_grace_seconds,
    ));
    let job = runtime.timeline_store.create_job(job)?;
    let _ = runtime.create_voice_playback_job_for_channel(
        &guild_id,
        &voice_channel_id,
        &job.requested_by_user_id,
        DiscordVoicePlaybackCue::Wake,
        "wake_detected",
        &job.id,
    )?;
    Ok(json!({
        "status": "scheduled",
        "job": job.to_value(),
    }))
}

pub fn execute(runtime: &mut Runtime, job: &Job, payload: &WakeActivationPayload) -> Result<Value> {
    let original_wake_at = parse_instant(&payload.wake_started_at).unwrap_or_else(utc_now);
    let latest_wake_at = parse_instant(&payload.latest_wake_at).unwrap_or(original_wake_at);
    let window_start = original_wake_at - chrono::Duration::seconds(payload.lookback_seconds);
    let hard_cap = original_wake_at + chrono::Duration::seconds(payload.max_window_seconds);
    let now = utc_now();
    let window_end = if now < hard_cap { now } else { hard_cap };
    let events = runtime.timeline_store.load_events(
        &payload.guild_id,
        &payload.voice_channel_id,
        Some(window_start),
        Some(window_end + chrono::Duration::milliseconds(1)),
        None,
        None,
        false,
    )?;
    let latest_wake_event = events
        .iter()
        .find(|event| {
            first_value_string(event, &["event_id", "eventId"]) == payload.latest_wake_event_id
        })
        .cloned()
        .or_else(|| {
            runtime
                .timeline_store
                .get_event(&payload.latest_wake_event_id)
                .ok()
        })
        .unwrap_or_else(|| json!({}));
    let due_at = activation_due_at(payload, &events, latest_wake_at);
    if now < due_at && now < hard_cap {
        let mut deferred = job.clone();
        deferred.state = JobState::Queued;
        deferred.next_run_at = Some(isoformat_z(Some(due_at)));
        runtime.timeline_store.update_job(&deferred)?;
        return Ok(json!({
            "kind": "wake_activation",
            "status": "deferred",
            "next_run_at": deferred.next_run_at,
        }));
    }

    let room = runtime.room_for_channel_ids(
        &payload.guild_id,
        &payload.voice_channel_id,
        Some(&payload.voice_channel_name),
    );
    let room_status = runtime.status_for_room(&room);
    let candidate = candidate_event(payload, &events, &latest_wake_event);
    let mut result = evaluate_voice_command(&candidate, &events, &room_status);
    attach_activation_bundle(&mut result, payload, &events, &room_status)?;
    let (valid, reason) = validate_voice_command_result(&result);
    if !valid || voice_command_action(&result) != "dispatch_now" {
        let _ = runtime.create_voice_playback_job_for_channel(
            &payload.guild_id,
            &payload.voice_channel_id,
            &payload.speaker_user_id,
            DiscordVoicePlaybackCue::Ack,
            "wake_activation_window_closed",
            &job.id,
        )?;
        runtime.timeline_store.append_event(
            &payload.guild_id,
            &payload.voice_channel_id,
            json!({
                "event_kind": "wake_activation_ignored",
                "kind": "wake_activation_ignored",
                "job_id": job.id,
                "activation_id": payload.activation_id,
                "reason": reason,
                "result": result,
            }),
        )?;
        return Ok(json!({
            "kind": "wake_activation",
            "status": "ignored",
            "valid": valid,
            "reason": reason,
            "result": result,
        }));
    }
    let command = crate::runtime::CommandRequest::from_json(&result)?;
    let _ = runtime.create_voice_playback_job_for_channel(
        &payload.guild_id,
        &payload.voice_channel_id,
        &payload.speaker_user_id,
        DiscordVoicePlaybackCue::Ack,
        "wake_activation_window_closed",
        &job.id,
    )?;
    let created = runtime.create_command_job_sync(command, Some(job))?;
    runtime.timeline_store.append_event(
        &payload.guild_id,
        &payload.voice_channel_id,
        json!({
            "event_kind": "wake_activation_dispatched",
            "kind": "wake_activation_dispatched",
            "job_id": job.id,
            "activation_id": payload.activation_id,
            "created": created,
        }),
    )?;
    Ok(json!({
        "kind": "wake_activation",
        "status": "dispatched",
        "result": result,
        "created": created,
    }))
}

fn activation_followup_target(
    runtime: &Runtime,
    guild_id: &str,
    voice_channel_id: &str,
    wake_started_at: DateTime<Utc>,
) -> Result<Option<Job>> {
    Ok(runtime
        .timeline_store
        .list_jobs(Some(guild_id), None)?
        .into_iter()
        .filter(|job| {
            job.kind == JobKind::WakeActivation
                && job.voice_channel_id == voice_channel_id
                && !job.state.is_terminal()
        })
        .filter_map(|job| {
            let payload = job.wake_activation_payload()?;
            let latest_wake_at = payload.latest_wake_at.clone();
            let additive_preempt_seconds = payload.additive_preempt_seconds;
            let independent_after_seconds = payload.independent_after_seconds;
            let latest = parse_instant(&latest_wake_at).unwrap_or(wake_started_at);
            let seconds_since_latest = (wake_started_at - latest).num_seconds().abs();
            if seconds_since_latest <= additive_preempt_seconds {
                return Some(job);
            }
            if seconds_since_latest <= independent_after_seconds
                && activation_can_be_rewritten(runtime, &job).unwrap_or(false)
            {
                return Some(job);
            }
            None
        })
        .min_by(|left, right| right.created_at.cmp(&left.created_at)))
}

fn activation_can_be_rewritten(runtime: &Runtime, activation: &Job) -> Result<bool> {
    if activation.state == JobState::Queued {
        return Ok(true);
    }
    let descendants = descendant_jobs(runtime, &activation.id)?;
    Ok(descendants.is_empty())
}

fn activation_for_wake_event(
    runtime: &Runtime,
    guild_id: &str,
    voice_channel_id: &str,
    wake_event_id: &str,
) -> Result<Option<Job>> {
    Ok(runtime
        .timeline_store
        .list_jobs(Some(guild_id), None)?
        .into_iter()
        .filter(|job| {
            job.kind == JobKind::WakeActivation && job.voice_channel_id == voice_channel_id
        })
        .find(|job| {
            let Some(payload) = job.wake_activation_payload() else {
                return false;
            };
            payload.wake_event_id == wake_event_id
                || payload.latest_wake_event_id == wake_event_id
                || payload
                    .amended_wake_event_ids
                    .iter()
                    .any(|id| id == wake_event_id)
        }))
}

fn amend_activation_job(
    runtime: &Runtime,
    mut activation: Job,
    wake_event_id: &str,
    wake_started_at: DateTime<Utc>,
    wake_ended_at: DateTime<Utc>,
    speaker_user_id: String,
    speaker_label: String,
) -> Result<Job> {
    let (activation_id, min_post_seconds, idle_seconds, flush_grace_seconds) = {
        let payload = activation
            .wake_activation_payload_mut()
            .ok_or_else(|| anyhow::anyhow!("wake activation job has wrong payload"))?;
        amend_payload(
            payload,
            wake_event_id,
            wake_started_at,
            speaker_user_id,
            speaker_label,
        );
        (
            payload.activation_id.clone(),
            payload.min_post_seconds,
            payload.speaker_idle_seconds,
            payload.stt_flush_grace_seconds,
        )
    };
    activation.state = JobState::Queued;
    activation.next_run_at = Some(ready_at_string(
        wake_started_at,
        wake_ended_at,
        min_post_seconds,
        idle_seconds,
        flush_grace_seconds,
    ));
    runtime.timeline_store.update_job(&activation)?;
    runtime.timeline_store.append_event(
        &activation.guild_id,
        &activation.voice_channel_id,
        json!({
            "event_kind": "wake_activation_amended",
            "kind": "wake_activation_amended",
            "activation_id": activation_id,
            "job_id": activation.id.clone(),
            "wake_event_id": wake_event_id,
        }),
    )?;
    Ok(activation)
}

fn replacement_activation_job(
    runtime: &Runtime,
    replaced: &Job,
    wake_event_id: &str,
    wake_started_at: DateTime<Utc>,
    wake_ended_at: DateTime<Utc>,
    speaker_user_id: String,
    speaker_label: String,
) -> Result<Job> {
    let mut payload = replaced
        .wake_activation_payload()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("wake activation job has wrong payload"))?;
    amend_payload(
        &mut payload,
        wake_event_id,
        wake_started_at,
        speaker_user_id,
        speaker_label,
    );
    if !payload.replacement_of_job_ids.contains(&replaced.id) {
        payload.replacement_of_job_ids.push(replaced.id.clone());
    }
    let mut job = Job::wake_activation(payload.clone());
    job.next_run_at = Some(ready_at_string(
        wake_started_at,
        wake_ended_at,
        payload.min_post_seconds,
        payload.speaker_idle_seconds,
        payload.stt_flush_grace_seconds,
    ));
    runtime.timeline_store.create_job(job)
}

fn amend_payload(
    payload: &mut WakeActivationPayload,
    wake_event_id: &str,
    wake_started_at: DateTime<Utc>,
    speaker_user_id: String,
    speaker_label: String,
) {
    payload.latest_wake_event_id = wake_event_id.to_string();
    payload.latest_wake_at = isoformat_z(Some(wake_started_at));
    payload.speaker_user_id = speaker_user_id;
    payload.speaker_label = speaker_label;
    if payload.wake_event_id != wake_event_id
        && !payload
            .amended_wake_event_ids
            .iter()
            .any(|id| id == wake_event_id)
    {
        payload
            .amended_wake_event_ids
            .push(wake_event_id.to_string());
    }
}

fn cancel_job_tree(runtime: &Runtime, root: &Job) -> Result<Vec<String>> {
    let mut jobs = descendant_jobs(runtime, &root.id)?;
    jobs.sort_by(|left, right| right.lineage_depth.cmp(&left.lineage_depth));
    jobs.push(root.clone());
    let mut cancelled = Vec::new();
    for mut job in jobs {
        if !job.state.is_cancellable() {
            continue;
        }
        if job.state == JobState::Running {
            job.mark_cancel_requested();
        } else {
            job.mark_cancelled();
        }
        cancelled.push(job.id.clone());
        runtime.timeline_store.update_job(&job)?;
    }
    Ok(cancelled)
}

fn descendant_jobs(runtime: &Runtime, root_job_id: &str) -> Result<Vec<Job>> {
    let mut descendants = Vec::new();
    let children = runtime.timeline_store.list_child_jobs(root_job_id)?;
    for child in children {
        descendants.extend(runtime.timeline_store.list_child_jobs(&child.id)?);
        descendants.push(child);
    }
    Ok(descendants)
}

fn activation_due_at(
    payload: &WakeActivationPayload,
    events: &[Value],
    latest_wake_at: DateTime<Utc>,
) -> DateTime<Utc> {
    let latest_speaker_end = events
        .iter()
        .filter(|event| same_speaker(event, &payload.speaker_user_id))
        .filter(|event| event_start(event).is_some_and(|started| started >= latest_wake_at))
        .filter_map(event_end)
        .max()
        .unwrap_or(latest_wake_at);
    let min_post_at = latest_wake_at + chrono::Duration::seconds(payload.min_post_seconds);
    let idle_at = latest_speaker_end + chrono::Duration::seconds(payload.speaker_idle_seconds);
    std::cmp::max(min_post_at, idle_at) + chrono::Duration::seconds(payload.stt_flush_grace_seconds)
}

fn candidate_event(
    payload: &WakeActivationPayload,
    events: &[Value],
    latest_wake_event: &Value,
) -> Value {
    events
        .iter()
        .filter(|event| same_speaker(event, &payload.speaker_user_id))
        .filter(|event| {
            event_start(event).is_some_and(|started| {
                parse_instant(&payload.latest_wake_at)
                    .map(|wake_at| started >= wake_at)
                    .unwrap_or(true)
            })
        })
        .max_by_key(|event| event_start(event).unwrap_or_else(utc_now))
        .cloned()
        .filter(|event| !event.as_object().is_none_or(|object| object.is_empty()))
        .unwrap_or_else(|| latest_wake_event.clone())
}

fn attach_activation_bundle(
    result: &mut Value,
    payload: &WakeActivationPayload,
    events: &[Value],
    room_status: &Value,
) -> Result<()> {
    let original_wake_at = parse_instant(&payload.wake_started_at).unwrap_or_else(utc_now);
    let latest_wake_at = parse_instant(&payload.latest_wake_at).unwrap_or(original_wake_at);
    let prior = events
        .iter()
        .filter(|event| event_start(event).is_some_and(|started| started < original_wake_at))
        .cloned()
        .collect::<Vec<_>>();
    let post = events
        .iter()
        .filter(|event| event_start(event).is_some_and(|started| started >= latest_wake_at))
        .cloned()
        .collect::<Vec<_>>();
    let bundle = json!({
        "activation_id": payload.activation_id,
        "prior_to_activation": prior,
        "wake_event_id": payload.wake_event_id,
        "latest_wake_event_id": payload.latest_wake_event_id,
        "amended_wake_event_ids": payload.amended_wake_event_ids,
        "post_activation_turn": post,
        "room_snapshot": room_status,
        "source_event_ids": source_event_ids(events),
    });
    let Some(map) = result.as_object_mut() else {
        return Ok(());
    };
    let arguments = map
        .entry("arguments".to_string())
        .or_insert_with(|| json!({}));
    if let Some(arguments) = arguments.as_object_mut() {
        arguments.insert("activation".to_string(), bundle);
    }
    Ok(())
}

fn source_event_ids(events: &[Value]) -> Vec<String> {
    events
        .iter()
        .map(|event| first_value_string(event, &["event_id", "eventId"]))
        .filter(|event_id| !event_id.is_empty())
        .collect()
}

fn ready_at_string(
    started_at: DateTime<Utc>,
    ended_at: DateTime<Utc>,
    min_post_seconds: i64,
    idle_seconds: i64,
    flush_grace_seconds: i64,
) -> String {
    let due_at = std::cmp::max(
        started_at + chrono::Duration::seconds(min_post_seconds),
        ended_at + chrono::Duration::seconds(idle_seconds),
    ) + chrono::Duration::seconds(flush_grace_seconds);
    due_at.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn same_speaker(event: &Value, speaker_user_id: &str) -> bool {
    first_value_string(event, &["speaker_user_id", "speakerId", "user_id"]) == speaker_user_id
}

fn env_i64(key: &str, fallback: i64) -> i64 {
    std::env::var(key)
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(fallback)
        .max(0)
}

fn non_empty(value: String, fallback: String) -> String {
    if value.trim().is_empty() {
        fallback
    } else {
        value
    }
}
