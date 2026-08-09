//! Transcription mux planning.
//!
//! Decides when queued transcription slots become mux jobs: fairness across
//! speaker flows, provider stream capacity, and latency budgets. The store
//! provides locked rows and SQL mechanics via `MuxPlanning`; every decision
//! is made here.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde_json::Value;

use crate::Result;
use crate::config;
use crate::runtime::timeline::store::{ActiveMuxStreamRow, TranscriptionSlotRecord};
use crate::runtime::timeline::{instant_ms_dt, utc_now};
use crate::runtime::{Ctx, Job, JobState};

/// A provider stream and when it is expected to be free again.
#[derive(Debug, Clone, Copy)]
struct ActiveMuxStream {
    available_at_ms: i64,
}

pub async fn ensure_transcription_mux_plan_job(
    ctx: &Ctx,
    source_id: &str,
    delay_ms: i64,
) -> Result<Option<Job>> {
    let source_id = source_id.trim();
    if source_id.is_empty()
        || !ctx
            .store
            .has_queued_transcription_slots(source_id)
            .await?
    {
        return Ok(None);
    }
    let ordering_key = transcription_mux_plan_ordering_key(source_id);
    if let Some(mut existing) = ctx
        .store
        .active_transcription_mux_plan_job(&ordering_key)
        .await?
    {
        if delay_ms <= 0 && existing.state == JobState::Queued && existing.next_run_at.is_some() {
            existing.next_run_at = None;
            ctx.store.update_job(&existing).await?;
        }
        return Ok(Some(existing));
    }
    ctx.store
        .create_job(Job::transcription_mux_plan(source_id, delay_ms.max(0)))
        .await
        .map(Some)
}

pub async fn ensure_transcription_mux_plan_jobs_for_queued_slots(
    ctx: &Ctx,
    delay_ms: i64,
) -> Result<Vec<Job>> {
    let mut jobs = Vec::new();
    for source_id in ctx.store.queued_transcription_source_ids().await? {
        if let Some(job) = ensure_transcription_mux_plan_job(ctx, &source_id, delay_ms).await? {
            jobs.push(job);
        }
    }
    Ok(jobs)
}

pub async fn plan_transcription_mux_jobs(ctx: &Ctx, source_id: &str) -> Result<Value> {
    let source_id = source_id.trim();
    let max_streams = config::transcription_mux_provider_streams();
    let max_slots = config::transcription_mux_max_slots();
    let max_audio_ms = config::transcription_mux_max_audio_ms();
    let guard_ms = config::transcription_mux_guard_ms();
    let normal_budget_ms = config::transcription_mux_normal_latency_budget_ms();
    let wake_budget_ms = config::transcription_mux_wake_latency_budget_ms();
    let overflow_backlog_ms = config::transcription_mux_overflow_backlog_ms();
    let now_ms = instant_ms_dt(utc_now());
    let candidate_limit = (max_slots * max_streams * 64).clamp(max_slots, 2048) as i64;
    let mut planning = ctx
        .store
        .begin_mux_planning(source_id, candidate_limit, now_ms)
        .await?;
    let mut active_streams = planning
        .active
        .iter()
        .map(|row| stream_from_row(row, now_ms, guard_ms))
        .collect::<Vec<_>>();
    active_streams.sort_by_key(|stream| stream.available_at_ms);
    let mut queued = std::mem::take(&mut planning.queued);
    if queued.is_empty() {
        planning.finish().await?;
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
        planning.finish().await?;
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
        planning
            .commit_batch(&slot_ids, &mux_job, source_id, now_ms)
            .await?;
        let selected_ids = slot_ids.into_iter().collect::<BTreeSet<_>>();
        let batch_audio_ms = mux_audio_ms_for_slots(&batch, guard_ms);
        queued.retain(|slot| !selected_ids.contains(&slot.slot_id));
        active_streams.push(ActiveMuxStream {
            available_at_ms: now_ms
                .saturating_add(estimated_transcription_provider_processing_ms(batch_audio_ms)),
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
    planning.finish().await?;
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

fn stream_from_row(row: &ActiveMuxStreamRow, now_ms: i64, guard_ms: i64) -> ActiveMuxStream {
    let mux_audio_ms = mux_audio_ms_for_counts(row.speech_ms, row.slot_count as usize, guard_ms);
    ActiveMuxStream {
        available_at_ms: now_ms.max(
            row.started_at_ms
                .saturating_add(estimated_transcription_provider_processing_ms(mux_audio_ms)),
        ),
    }
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
                    return single_slot_compatible_batch(selected);
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
    single_slot_compatible_batch(selected)
}


fn single_slot_compatible_batch(
    mut selected: Vec<TranscriptionSlotRecord>,
) -> Vec<TranscriptionSlotRecord> {
    let Some(index) = selected.iter().position(|slot| slot.requires_single_slot) else {
        return selected;
    };
    if index == 0 {
        selected.truncate(1);
    } else {
        selected.truncate(index);
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
    format!(
        "transcription:mux_plan:{}",
        crate::runtime::jobs::spec::normalize_key_part(source_id)
    )
}
