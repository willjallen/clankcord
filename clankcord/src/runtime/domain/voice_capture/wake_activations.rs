use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{Value, json};

use crate::Result;
use crate::config;
use crate::runtime::timeline::{
    event_end, event_speaker, event_start, event_text, isoformat_z, new_id, parse_instant, utc_now,
};
use crate::runtime::util::{first_value_string, non_empty};
use crate::runtime::{
    CommandRequest, DiscordVoicePlaybackCue, Job, JobKind, JobState, Runtime, WakeActivationPayload,
};

#[derive(Debug, Clone, Copy)]
struct CaptureHold {
    reason: &'static str,
    next_run_at: DateTime<Utc>,
}

pub fn event_has_wake(event: &Value) -> bool {
    event
        .get("wake")
        .and_then(|wake| wake.get("wake"))
        .and_then(Value::as_bool)
        == Some(true)
        || event.get("wake_detected").and_then(Value::as_bool) == Some(true)
}

pub async fn schedule_from_wake_event(runtime: &Runtime, event: &Value) -> Result<Value> {
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
        activation_for_wake_event(runtime, &guild_id, &voice_channel_id, &wake_event_id).await?
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
        activation_followup_target(runtime, &guild_id, &voice_channel_id, wake_started_at).await?
    {
        let descendants = descendant_jobs(runtime, &existing.id).await?;
        if descendants.is_empty() {
            let existing = amend_activation_job(
                runtime,
                existing,
                &wake_event_id,
                wake_started_at,
                wake_ended_at,
                speaker_user_id,
                speaker_label,
            )
            .await?;
            let _ = runtime
                .create_voice_playback_job_for_channel(
                    &guild_id,
                    &voice_channel_id,
                    &existing.requested_by_user_id,
                    DiscordVoicePlaybackCue::Preempt,
                    "wake_activation_amended",
                    &existing.id,
                )
                .await?;
            return Ok(json!({
                "status": "amended",
                "job": existing.to_value(),
            }));
        }
        let cancelled = cancel_job_tree(runtime, &existing).await?;
        let replacement = replacement_activation_job(
            runtime,
            &existing,
            &wake_event_id,
            wake_started_at,
            wake_ended_at,
            speaker_user_id,
            speaker_label,
        )
        .await?;
        let _ = runtime
            .create_voice_playback_job_for_channel(
                &guild_id,
                &voice_channel_id,
                &replacement.requested_by_user_id,
                DiscordVoicePlaybackCue::Preempt,
                "wake_activation_replaced",
                &replacement.id,
            )
            .await?;
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
        )
        .await?;
        return Ok(json!({
            "status": "replaced",
            "job": replacement.to_value(),
            "replaced_job_id": existing.id.clone(),
            "cancelled_job_ids": cancelled,
        }));
    }

    let activation = config::wake_activation_config();
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
        lookback_seconds: activation.lookback_seconds.max(0),
        min_post_seconds: activation.min_post_seconds.max(0),
        speaker_idle_seconds: activation.speaker_idle_seconds.max(0),
        stt_flush_grace_seconds: activation.stt_flush_grace_seconds.max(0),
        max_window_seconds: activation.max_window_seconds.max(0),
        additive_preempt_seconds: activation.additive_preempt_seconds.max(0),
        independent_after_seconds: activation.independent_after_seconds.max(0),
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
    let job = runtime.timeline_store.create_job(job).await?;
    let _ = runtime
        .create_voice_playback_job_for_channel(
            &guild_id,
            &voice_channel_id,
            &job.requested_by_user_id,
            DiscordVoicePlaybackCue::Wake,
            "wake_detected",
            &job.id,
        )
        .await?;
    Ok(json!({
        "status": "scheduled",
        "job": job.to_value(),
    }))
}

pub async fn execute(
    runtime: &mut Runtime,
    job: &Job,
    payload: &WakeActivationPayload,
) -> Result<Value> {
    let original_wake_at = parse_instant(&payload.wake_started_at).unwrap_or_else(utc_now);
    let latest_wake_at = parse_instant(&payload.latest_wake_at).unwrap_or(original_wake_at);
    let window_start = original_wake_at - chrono::Duration::seconds(payload.lookback_seconds);
    let hard_cap = original_wake_at + chrono::Duration::seconds(payload.max_window_seconds);
    let now = utc_now();
    let window_end = if now < hard_cap { now } else { hard_cap };
    let events = runtime
        .timeline_store
        .load_events(
            &payload.guild_id,
            &payload.voice_channel_id,
            Some(window_start),
            Some(window_end + chrono::Duration::milliseconds(1)),
            None,
            None,
            false,
        )
        .await?;

    if let Some(closed_at) = activation_window_closed_at(payload, &events) {
        return dispatch_after_stt_settles(
            runtime,
            job,
            payload,
            window_start,
            latest_wake_at,
            closed_at,
            now,
        )
        .await;
    }

    let due_at = std::cmp::min(
        activation_due_at(payload, &events, latest_wake_at),
        hard_cap,
    );
    if now < due_at {
        let mut deferred = job.clone();
        deferred.state = JobState::Queued;
        deferred.next_run_at = Some(isoformat_z(Some(due_at)));
        runtime.timeline_store.update_job(&deferred).await?;
        return Ok(json!({
            "kind": "wake_activation",
            "status": "deferred",
            "next_run_at": deferred.next_run_at,
        }));
    }

    if let Some(hold) = activation_voice_capture_hold(runtime, payload, latest_wake_at, now).await?
        && now < hard_cap
    {
        let next_run_at = std::cmp::min(hold.next_run_at, hard_cap);
        let mut deferred = job.clone();
        deferred.state = JobState::Queued;
        deferred.next_run_at = Some(isoformat_z(Some(next_run_at)));
        runtime.timeline_store.update_job(&deferred).await?;
        return Ok(json!({
            "kind": "wake_activation",
            "status": "deferred",
            "reason": hold.reason,
            "next_run_at": deferred.next_run_at,
        }));
    }

    let closed_at = if now >= hard_cap { hard_cap } else { due_at };
    record_activation_window_closed(runtime, job, payload, closed_at).await?;

    dispatch_after_stt_settles(
        runtime,
        job,
        payload,
        window_start,
        latest_wake_at,
        closed_at,
        now,
    )
    .await
}

async fn dispatch_after_stt_settles(
    runtime: &mut Runtime,
    job: &Job,
    payload: &WakeActivationPayload,
    window_start: DateTime<Utc>,
    latest_wake_at: DateTime<Utc>,
    closed_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<Value> {
    let settle_deadline = closed_at
        + chrono::Duration::seconds(config::wake_activation_config().stt_settle_seconds.max(0));
    if has_pending_speaker_audio_segment(runtime, payload, latest_wake_at, closed_at).await? {
        if now < settle_deadline {
            let mut deferred = job.clone();
            deferred.state = JobState::Queued;
            deferred.next_run_at = Some(isoformat_z(Some(std::cmp::min(
                now + chrono::Duration::milliseconds(active_capture_poll_ms()),
                settle_deadline,
            ))));
            runtime.timeline_store.update_job(&deferred).await?;
            return Ok(json!({
                "kind": "wake_activation",
                "status": "deferred",
                "reason": "waiting_for_request_transcription",
                "request_audio_closed_at": isoformat_z(Some(closed_at)),
                "next_run_at": deferred.next_run_at,
            }));
        }
        record_activation_no_request(runtime, job, payload, closed_at, "stt_settlement_expired")
            .await?;
        return Ok(json!({
            "kind": "wake_activation",
            "status": "no_request_captured",
            "reason": "stt_settlement_expired",
            "request_audio_closed_at": isoformat_z(Some(closed_at)),
        }));
    }

    let request_events = runtime
        .timeline_store
        .load_events(
            &payload.guild_id,
            &payload.voice_channel_id,
            Some(window_start),
            Some(closed_at + chrono::Duration::milliseconds(1)),
            None,
            None,
            false,
        )
        .await?;
    let request = activation_request_text(payload, &request_events, closed_at);
    if request.trim().is_empty() {
        record_activation_no_request(runtime, job, payload, closed_at, "empty_request_text")
            .await?;
        return Ok(json!({
            "kind": "wake_activation",
            "status": "no_request_captured",
            "reason": "empty_request_text",
            "request_audio_closed_at": isoformat_z(Some(closed_at)),
        }));
    }

    let command = activation_agent_task_command(payload, &request_events, closed_at)?;
    let agent_job = runtime
        .agent_session_start_or_task_job(
            &payload.guild_id,
            &payload.voice_channel_id,
            &payload.speaker_user_id,
            command,
        )
        .await?;
    let created_job = runtime
        .timeline_store
        .create_child_job(job, agent_job)
        .await?;
    let created = json!({
        "kind": "agent_task_created",
        "job_ids": [created_job.id.clone()],
        "job": created_job.to_value(),
    });
    runtime
        .timeline_store
        .append_event(
            &payload.guild_id,
            &payload.voice_channel_id,
            json!({
                "event_kind": "wake_activation_dispatched",
                "kind": "wake_activation_dispatched",
                "job_id": job.id,
                "activation_id": payload.activation_id,
                "request_audio_closed_at": isoformat_z(Some(closed_at)),
                "created": created.clone(),
            }),
        )
        .await?;
    Ok(json!({
        "kind": "wake_activation",
        "status": "dispatched",
        "request_audio_closed_at": isoformat_z(Some(closed_at)),
        "created": created,
    }))
}

async fn record_activation_window_closed(
    runtime: &Runtime,
    job: &Job,
    payload: &WakeActivationPayload,
    closed_at: DateTime<Utc>,
) -> Result<()> {
    runtime
        .timeline_store
        .append_event(
            &payload.guild_id,
            &payload.voice_channel_id,
            json!({
                "event_kind": "wake_activation_window_closed",
                "kind": "wake_activation_window_closed",
                "job_id": job.id,
                "activation_id": payload.activation_id,
                "wake_event_id": payload.wake_event_id,
                "latest_wake_event_id": payload.latest_wake_event_id,
                "speaker_user_id": payload.speaker_user_id,
                "speaker_label": payload.speaker_label,
                "request_audio_closed_at": isoformat_z(Some(closed_at)),
                "startedAt": isoformat_z(Some(closed_at)),
                "endedAt": isoformat_z(Some(closed_at)),
            }),
        )
        .await?;
    let _ = runtime
        .create_voice_playback_job_for_channel(
            &payload.guild_id,
            &payload.voice_channel_id,
            &payload.speaker_user_id,
            DiscordVoicePlaybackCue::Ack,
            "wake_activation_window_closed",
            &job.id,
        )
        .await?;
    Ok(())
}

async fn record_activation_no_request(
    runtime: &Runtime,
    job: &Job,
    payload: &WakeActivationPayload,
    closed_at: DateTime<Utc>,
    reason: &str,
) -> Result<()> {
    runtime
        .timeline_store
        .append_event(
            &payload.guild_id,
            &payload.voice_channel_id,
            json!({
                "event_kind": "wake_activation_no_request",
                "kind": "wake_activation_no_request",
                "job_id": job.id,
                "activation_id": payload.activation_id,
                "reason": reason,
                "request_audio_closed_at": isoformat_z(Some(closed_at)),
            }),
        )
        .await?;
    Ok(())
}

async fn activation_followup_target(
    runtime: &Runtime,
    guild_id: &str,
    voice_channel_id: &str,
    wake_started_at: DateTime<Utc>,
) -> Result<Option<Job>> {
    let jobs = runtime
        .timeline_store
        .list_active_jobs_by_scope_kind(guild_id, voice_channel_id, JobKind::WakeActivation)
        .await?;
    let mut candidates = Vec::new();
    for job in jobs {
        let Some(payload) = job.wake_activation_payload() else {
            continue;
        };
        let latest_wake_at = payload.latest_wake_at.clone();
        let additive_preempt_seconds = payload.additive_preempt_seconds;
        let independent_after_seconds = payload.independent_after_seconds;
        let latest = parse_instant(&latest_wake_at).unwrap_or(wake_started_at);
        let seconds_since_latest = (wake_started_at - latest).num_seconds().abs();
        if seconds_since_latest <= additive_preempt_seconds {
            candidates.push(job);
            continue;
        }
        if seconds_since_latest <= independent_after_seconds
            && activation_can_be_rewritten(runtime, &job).await?
        {
            candidates.push(job);
        }
    }
    Ok(candidates
        .into_iter()
        .min_by(|left, right| right.created_at.cmp(&left.created_at)))
}

async fn activation_can_be_rewritten(runtime: &Runtime, activation: &Job) -> Result<bool> {
    if activation.state == JobState::Queued {
        return Ok(true);
    }
    let descendants = descendant_jobs(runtime, &activation.id).await?;
    Ok(descendants.is_empty())
}

async fn activation_for_wake_event(
    runtime: &Runtime,
    guild_id: &str,
    voice_channel_id: &str,
    wake_event_id: &str,
) -> Result<Option<Job>> {
    Ok(runtime
        .timeline_store
        .list_jobs_by_scope_kind(guild_id, voice_channel_id, JobKind::WakeActivation)
        .await?
        .into_iter()
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

async fn amend_activation_job(
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
    runtime.timeline_store.update_job(&activation).await?;
    runtime
        .timeline_store
        .append_event(
            &activation.guild_id,
            &activation.voice_channel_id,
            json!({
                "event_kind": "wake_activation_amended",
                "kind": "wake_activation_amended",
                "activation_id": activation_id,
                "job_id": activation.id.clone(),
                "wake_event_id": wake_event_id,
            }),
        )
        .await?;
    Ok(activation)
}

async fn replacement_activation_job(
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
    runtime.timeline_store.create_job(job).await
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

async fn cancel_job_tree(runtime: &Runtime, root: &Job) -> Result<Vec<String>> {
    let mut jobs = descendant_jobs(runtime, &root.id).await?;
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
        runtime.timeline_store.update_job(&job).await?;
    }
    Ok(cancelled)
}

async fn descendant_jobs(runtime: &Runtime, root_job_id: &str) -> Result<Vec<Job>> {
    let mut descendants = Vec::new();
    let children = runtime.timeline_store.list_child_jobs(root_job_id).await?;
    for child in children {
        descendants.extend(runtime.timeline_store.list_child_jobs(&child.id).await?);
        descendants.push(child);
    }
    Ok(descendants)
}

fn activation_due_at(
    payload: &WakeActivationPayload,
    events: &[Value],
    latest_wake_at: DateTime<Utc>,
) -> DateTime<Utc> {
    let latest_speaker_end =
        latest_speaker_event_end(payload, events, latest_wake_at).unwrap_or(latest_wake_at);
    let min_post_at = latest_wake_at + chrono::Duration::seconds(payload.min_post_seconds);
    let idle_at = latest_speaker_end + chrono::Duration::seconds(payload.speaker_idle_seconds);
    std::cmp::max(min_post_at, idle_at)
}

fn latest_speaker_event_end(
    payload: &WakeActivationPayload,
    events: &[Value],
    latest_wake_at: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    events
        .iter()
        .filter(|event| same_speaker(event, &payload.speaker_user_id))
        .filter(|event| event_overlaps_or_follows(event, latest_wake_at))
        .filter_map(event_end)
        .max()
}

fn activation_window_closed_at(
    payload: &WakeActivationPayload,
    events: &[Value],
) -> Option<DateTime<Utc>> {
    events
        .iter()
        .filter(|event| {
            first_value_string(event, &["event_kind", "kind"]) == "wake_activation_window_closed"
                && first_value_string(event, &["activation_id"]) == payload.activation_id
        })
        .filter_map(|event| {
            parse_instant(&first_value_string(event, &["request_audio_closed_at"]))
                .or_else(|| event_start(event))
        })
        .max()
}

async fn activation_voice_capture_hold(
    runtime: &Runtime,
    payload: &WakeActivationPayload,
    latest_wake_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<Option<CaptureHold>> {
    live_speaker_capture_hold(runtime, payload, latest_wake_at, now).await
}

async fn live_speaker_capture_hold(
    runtime: &Runtime,
    payload: &WakeActivationPayload,
    latest_wake_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<Option<CaptureHold>> {
    let session = runtime
        .active_session_for_channel(&payload.guild_id, &payload.voice_channel_id)
        .await?;
    let Some(session) = session else {
        return Ok(None);
    };
    let Some(speaker) = session.capture_stats.speakers.get(&payload.speaker_user_id) else {
        return Ok(None);
    };
    let last_pcm_at = parse_instant(&speaker.last_pcm_at);
    if last_pcm_at.is_some_and(|last_pcm_at| last_pcm_at < latest_wake_at) {
        return Ok(None);
    }
    let settled_at = last_pcm_at
        .map(|last_pcm_at| last_pcm_at + chrono::Duration::seconds(payload.speaker_idle_seconds))
        .unwrap_or(now + chrono::Duration::milliseconds(active_capture_poll_ms()));
    let has_live_audio =
        speaker.active || speaker.flush_in_flight || speaker.buffered_audio_bytes > 0;
    let waiting_for_idle = settled_at > now;
    if !has_live_audio && !waiting_for_idle {
        return Ok(None);
    }
    let next_run_at = if waiting_for_idle {
        settled_at
    } else {
        now + chrono::Duration::milliseconds(active_capture_poll_ms())
    };
    Ok(Some(CaptureHold {
        reason: "waiting_for_live_speaker_audio",
        next_run_at,
    }))
}

async fn has_pending_speaker_audio_segment(
    runtime: &Runtime,
    payload: &WakeActivationPayload,
    latest_wake_at: DateTime<Utc>,
    closed_at: DateTime<Utc>,
) -> Result<bool> {
    let jobs = runtime
        .timeline_store
        .list_active_jobs_by_scope_kind(
            &payload.guild_id,
            &payload.voice_channel_id,
            JobKind::AudioSegment,
        )
        .await?;
    Ok(jobs
        .iter()
        .filter_map(|job| job.audio_segment_payload())
        .any(|segment| {
            segment.speaker_user_id == payload.speaker_user_id
                && segment.segment_end_time >= latest_wake_at
                && segment.segment_start_time <= closed_at
        }))
}

fn activation_agent_task_command(
    payload: &WakeActivationPayload,
    events: &[Value],
    closed_at: DateTime<Utc>,
) -> Result<CommandRequest> {
    let request = activation_request_text(payload, events, closed_at);
    let source_event_ids = activation_source_event_ids(payload, events, closed_at);
    let activation = activation_summary(payload, &source_event_ids);
    CommandRequest::from_json(&json!({
        "action": "dispatch_now",
        "command_kind": "agent_task",
        "guild_id": payload.guild_id,
        "voice_channel_id": payload.voice_channel_id,
        "requested_by_user_id": payload.speaker_user_id,
        "requested_by_speaker_label": payload.speaker_label,
        "acknowledgement_text": "Working on that for you.",
        "requires_confirmation": false,
        "arguments": {
            "request": request,
            "instruction_text": request,
            "source_event_ids": source_event_ids,
            "activation": activation,
        },
    }))
}

fn activation_summary(payload: &WakeActivationPayload, source_event_ids: &[String]) -> Value {
    json!({
        "activation_id": payload.activation_id,
        "wake_event_id": payload.wake_event_id,
        "latest_wake_event_id": payload.latest_wake_event_id,
        "amended_wake_event_ids": payload.amended_wake_event_ids,
        "wake_started_at": payload.wake_started_at,
        "wake_ended_at": payload.wake_ended_at,
        "latest_wake_at": payload.latest_wake_at,
        "voice_channel_name": payload.voice_channel_name,
        "speaker_user_id": payload.speaker_user_id,
        "speaker_label": payload.speaker_label,
        "source_event_ids": source_event_ids,
    })
}

fn activation_request_text(
    payload: &WakeActivationPayload,
    events: &[Value],
    closed_at: DateTime<Utc>,
) -> String {
    let original_wake_at = parse_instant(&payload.wake_started_at).unwrap_or_else(utc_now);
    let latest_wake_at = parse_instant(&payload.latest_wake_at).unwrap_or(original_wake_at);
    let collapsed = collapse_ws(
        &events
            .iter()
            .filter(|event| same_speaker(event, &payload.speaker_user_id))
            .filter(|event| event_is_in_request_window(event, latest_wake_at, closed_at))
            .map(event_text)
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join(" "),
    );
    strip_leading_wake_phrase(&collapsed).to_string()
}

fn activation_source_event_ids(
    payload: &WakeActivationPayload,
    events: &[Value],
    closed_at: DateTime<Utc>,
) -> Vec<String> {
    let original_wake_at = parse_instant(&payload.wake_started_at).unwrap_or_else(utc_now);
    let latest_wake_at = parse_instant(&payload.latest_wake_at).unwrap_or(original_wake_at);
    let mut ids = Vec::new();
    for id in [
        payload.wake_event_id.as_str(),
        payload.latest_wake_event_id.as_str(),
    ]
    .into_iter()
    .chain(payload.amended_wake_event_ids.iter().map(String::as_str))
    {
        if !id.trim().is_empty() && !ids.iter().any(|existing| existing == id) {
            ids.push(id.to_string());
        }
    }
    for event in events
        .iter()
        .filter(|event| event_is_in_request_window(event, latest_wake_at, closed_at))
        .filter(|event| !event_text(event).is_empty() || event_has_wake(event))
    {
        let id = first_value_string(event, &["event_id", "eventId"]);
        if !id.is_empty() && !ids.iter().any(|existing| existing == &id) {
            ids.push(id);
        }
    }
    ids
}

fn collapse_ws(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn ready_at_string(
    started_at: DateTime<Utc>,
    ended_at: DateTime<Utc>,
    min_post_seconds: i64,
    idle_seconds: i64,
    _flush_grace_seconds: i64,
) -> String {
    let due_at = std::cmp::max(
        started_at + chrono::Duration::seconds(min_post_seconds),
        ended_at + chrono::Duration::seconds(idle_seconds),
    );
    due_at.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn same_speaker(event: &Value, speaker_user_id: &str) -> bool {
    first_value_string(event, &["speaker_user_id", "speakerId", "user_id"]) == speaker_user_id
}

fn event_overlaps_or_follows(event: &Value, instant: DateTime<Utc>) -> bool {
    event_end(event)
        .or_else(|| event_start(event))
        .is_some_and(|ended| ended >= instant)
}

fn event_is_in_request_window(
    event: &Value,
    latest_wake_at: DateTime<Utc>,
    closed_at: DateTime<Utc>,
) -> bool {
    event_end(event)
        .or_else(|| event_start(event))
        .is_some_and(|ended| ended >= latest_wake_at)
        && event_start(event)
            .or_else(|| event_end(event))
            .is_some_and(|started| started <= closed_at)
}

fn strip_leading_wake_phrase(text: &str) -> &str {
    let value = text.trim();
    let Some(after_hey) = strip_ascii_word(value, "hey") else {
        return strip_activation_separator(strip_ascii_word(value, "clanky").unwrap_or(value));
    };
    let after_hey = strip_activation_separator(after_hey);
    strip_activation_separator(strip_ascii_word(after_hey, "clanky").unwrap_or(value))
}

fn strip_ascii_word<'a>(value: &'a str, word: &str) -> Option<&'a str> {
    let trimmed = value.trim_start();
    if trimmed.len() < word.len() {
        return None;
    }
    let prefix = trimmed.get(..word.len())?;
    if !prefix.eq_ignore_ascii_case(word) {
        return None;
    }
    let rest = &trimmed[word.len()..];
    if rest
        .chars()
        .next()
        .is_some_and(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    {
        return None;
    }
    Some(rest)
}

fn strip_activation_separator(value: &str) -> &str {
    value
        .trim_start_matches(|ch: char| {
            ch.is_ascii_whitespace() || matches!(ch, ',' | '.' | ':' | ';' | '-' | '!' | '?')
        })
        .trim()
}

fn active_capture_poll_ms() -> i64 {
    config::wake_activation_config()
        .active_capture_poll_ms
        .max(1)
}
