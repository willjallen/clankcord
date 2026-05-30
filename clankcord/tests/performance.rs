use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

const ELEVENLABS_REALTIME_FACTOR: f64 = 0.3;
const ELEVENLABS_FIXED_OVERHEAD_SECONDS: f64 = 2.0;
const ELEVENLABS_PER_CHANNEL_OVERHEAD_SECONDS: f64 = 0.5;

const MUX_PROVIDER_STREAMS: usize = 2;
const MUX_BATCH_DELAY_SECONDS: f64 = 0.5;
const MUX_MAX_SLOTS: usize = 16;
const MUX_MAX_AUDIO_SECONDS: f64 = 30.0;
const MUX_GUARD_SECONDS: f64 = 0.150;
const MUX_NORMAL_LATENCY_BUDGET_SECONDS: f64 = 15.0;
const MUX_WAKE_LATENCY_BUDGET_SECONDS: f64 = 5.0;
const MUX_OVERFLOW_BACKLOG_SECONDS: f64 = 2.0;
const FLUSH_DELAY_SECONDS: f64 = 0.2;
const SEGMENT_SECONDS: f64 = 8.0;
const WALL_SECONDS: f64 = 600.0;
const WAKE_CLOSED_AT_SECONDS: f64 = 307.0;

#[derive(Debug, Clone)]
struct Slot {
    id: usize,
    room: &'static str,
    speaker: usize,
    segment_start_s: f64,
    arrival_s: f64,
    duration_s: f64,
    priority: i64,
}

#[derive(Debug, Clone)]
struct WakeCheck {
    label: &'static str,
    room: &'static str,
    closed_at_s: f64,
}

#[derive(Debug, Clone)]
struct Scenario {
    name: &'static str,
    slots: Vec<Slot>,
    wake_checks: Vec<WakeCheck>,
}

#[derive(Debug, Clone)]
struct SimulationReport {
    name: &'static str,
    slots: usize,
    mux_jobs: usize,
    input_wall_s: f64,
    total_speech_s: f64,
    provider_busy_s: f64,
    provider_cost_audio_s: f64,
    completed_at_s: f64,
    drain_after_input_s: f64,
    mean_slots_per_mux: f64,
    max_slots_per_mux: usize,
    mean_mux_audio_s: f64,
    max_mux_audio_s: f64,
    provider_utilization_until_drain: f64,
    wake_waits: Vec<(String, f64)>,
}

#[test]
fn elevenlabs_processing_time_formula_matches_documented_example() {
    let processing_time = elevenlabs_processing_seconds(60.0, 2);
    assert!((processing_time - 21.0).abs() < f64::EPSILON);
    assert!((provider_concurrency_cost_audio_seconds(60.0, 3) - 180.0).abs() < f64::EPSILON);
}

#[test]
#[ignore = "prints deterministic high-demand transcription capacity simulation"]
fn simulated_high_demand_elevenlabs_mux_capacity_report() {
    let scenarios = [
        conversational_three_room_scenario(),
        pathological_six_simultaneous_speakers_scenario(),
        pathological_ten_simultaneous_speakers_scenario(),
        pathological_ten_speakers_all_rooms_wake_scenario(),
    ];
    let reports = scenarios.iter().map(simulate).collect::<Vec<_>>();
    for report in &reports {
        print_report(report);
    }

    assert!(
        reports[0].drain_after_input_s < 30.0,
        "conversational three-room load should drain quickly"
    );
    assert!(
        reports[1].drain_after_input_s > 60.0,
        "continuous six-speaker overlap should show sustained provider backlog"
    );
    assert!(
        reports[2].drain_after_input_s > reports[1].drain_after_input_s,
        "ten-speaker overlap should be worse than six-speaker overlap"
    );
    assert!(
        reports[3]
            .wake_waits
            .iter()
            .map(|(_, wait_s)| *wait_s)
            .fold(0.0_f64, f64::max)
            > 300.0,
        "simultaneous wakes in all rooms should expose the true saturated backlog"
    );
}

fn elevenlabs_processing_seconds(duration_s: f64, channels: usize) -> f64 {
    duration_s * ELEVENLABS_REALTIME_FACTOR
        + ELEVENLABS_FIXED_OVERHEAD_SECONDS
        + channels as f64 * ELEVENLABS_PER_CHANNEL_OVERHEAD_SECONDS
}

fn provider_concurrency_cost_audio_seconds(duration_s: f64, channels: usize) -> f64 {
    duration_s * channels as f64
}

fn conversational_three_room_scenario() -> Scenario {
    let mut slots = Vec::new();
    add_round_robin_room(&mut slots, "code", 4, WALL_SECONDS);
    add_round_robin_room(&mut slots, "environment", 3, WALL_SECONDS);
    add_round_robin_room(&mut slots, "art", 3, WALL_SECONDS);
    apply_wake_room_priority(&mut slots, "code", WAKE_CLOSED_AT_SECONDS);
    Scenario {
        name: "3 rooms, one active speaker per room",
        slots,
        wake_checks: vec![WakeCheck {
            label: "code wake",
            room: "code",
            closed_at_s: WAKE_CLOSED_AT_SECONDS,
        }],
    }
}

fn pathological_six_simultaneous_speakers_scenario() -> Scenario {
    let mut slots = Vec::new();
    add_continuous_speakers(&mut slots, "code", 2, WALL_SECONDS);
    add_continuous_speakers(&mut slots, "environment", 2, WALL_SECONDS);
    add_continuous_speakers(&mut slots, "art", 2, WALL_SECONDS);
    apply_wake_room_priority(&mut slots, "code", WAKE_CLOSED_AT_SECONDS);
    Scenario {
        name: "3 rooms, 6 overlapping continuous speakers",
        slots,
        wake_checks: vec![WakeCheck {
            label: "code wake",
            room: "code",
            closed_at_s: WAKE_CLOSED_AT_SECONDS,
        }],
    }
}

fn pathological_ten_simultaneous_speakers_scenario() -> Scenario {
    let mut slots = Vec::new();
    add_continuous_speakers(&mut slots, "code", 4, WALL_SECONDS);
    add_continuous_speakers(&mut slots, "environment", 3, WALL_SECONDS);
    add_continuous_speakers(&mut slots, "art", 3, WALL_SECONDS);
    apply_wake_room_priority(&mut slots, "code", WAKE_CLOSED_AT_SECONDS);
    Scenario {
        name: "3 rooms, 10 overlapping continuous speakers",
        slots,
        wake_checks: vec![WakeCheck {
            label: "code wake",
            room: "code",
            closed_at_s: WAKE_CLOSED_AT_SECONDS,
        }],
    }
}

fn pathological_ten_speakers_all_rooms_wake_scenario() -> Scenario {
    let mut slots = Vec::new();
    add_continuous_speakers(&mut slots, "code", 4, WALL_SECONDS);
    add_continuous_speakers(&mut slots, "environment", 3, WALL_SECONDS);
    add_continuous_speakers(&mut slots, "art", 3, WALL_SECONDS);
    apply_wake_room_priority(&mut slots, "code", WAKE_CLOSED_AT_SECONDS);
    apply_wake_room_priority(&mut slots, "environment", WAKE_CLOSED_AT_SECONDS);
    apply_wake_room_priority(&mut slots, "art", WAKE_CLOSED_AT_SECONDS);
    Scenario {
        name: "3 rooms, 10 overlapping speakers, simultaneous wakes",
        slots,
        wake_checks: vec![
            WakeCheck {
                label: "code wake",
                room: "code",
                closed_at_s: WAKE_CLOSED_AT_SECONDS,
            },
            WakeCheck {
                label: "environment wake",
                room: "environment",
                closed_at_s: WAKE_CLOSED_AT_SECONDS,
            },
            WakeCheck {
                label: "art wake",
                room: "art",
                closed_at_s: WAKE_CLOSED_AT_SECONDS,
            },
        ],
    }
}

fn add_round_robin_room(
    slots: &mut Vec<Slot>,
    room: &'static str,
    speaker_count: usize,
    wall_seconds: f64,
) {
    let mut segment_start_s = 0.0;
    let mut index = 0usize;
    while segment_start_s < wall_seconds {
        let duration_s = SEGMENT_SECONDS.min(wall_seconds - segment_start_s);
        slots.push(Slot {
            id: slots.len(),
            room,
            speaker: index % speaker_count,
            segment_start_s,
            arrival_s: segment_start_s + duration_s + FLUSH_DELAY_SECONDS,
            duration_s,
            priority: 0,
        });
        segment_start_s += duration_s;
        index += 1;
    }
}

fn add_continuous_speakers(
    slots: &mut Vec<Slot>,
    room: &'static str,
    speaker_count: usize,
    wall_seconds: f64,
) {
    for speaker in 0..speaker_count {
        let mut segment_start_s = 0.0;
        while segment_start_s < wall_seconds {
            let duration_s = SEGMENT_SECONDS.min(wall_seconds - segment_start_s);
            slots.push(Slot {
                id: slots.len(),
                room,
                speaker,
                segment_start_s,
                arrival_s: segment_start_s + duration_s + FLUSH_DELAY_SECONDS,
                duration_s,
                priority: 0,
            });
            segment_start_s += duration_s;
        }
    }
}

fn apply_wake_room_priority(slots: &mut [Slot], room: &str, closed_at_s: f64) {
    for slot in slots {
        if slot.room == room && slot.segment_start_s <= closed_at_s {
            slot.priority = 1000;
        }
    }
}

fn simulate(scenario: &Scenario) -> SimulationReport {
    let mut slots = scenario.slots.clone();
    slots.sort_by(compare_arrival_then_id);
    for (id, slot) in slots.iter_mut().enumerate() {
        slot.id = id;
    }

    let mut next_arrival = 0usize;
    let mut pending = Vec::<usize>::new();
    let mut completions = vec![0.0; slots.len()];
    let mut active_streams = Vec::<f64>::new();
    let mut scheduled_slots = 0usize;
    let mut mux_jobs = 0usize;
    let mut provider_busy_s = 0.0_f64;
    let mut provider_cost_audio_s = 0.0_f64;
    let mut total_selected_slots = 0usize;
    let mut max_slots_per_mux = 0usize;
    let mut total_mux_audio_s = 0.0_f64;
    let mut max_mux_audio_s = 0.0_f64;

    let mut planner_s = 0.0_f64;
    while scheduled_slots < slots.len() {
        active_streams.retain(|available_at| *available_at > planner_s);
        while next_arrival < slots.len() && slots[next_arrival].arrival_s <= planner_s {
            pending.push(next_arrival);
            next_arrival += 1;
        }
        if pending.is_empty() {
            let next_arrival_planner_s = slots
                .get(next_arrival)
                .map(|slot| slot.arrival_s + MUX_BATCH_DELAY_SECONDS)
                .unwrap_or(f64::INFINITY);
            let next_stream_s = active_streams.iter().copied().fold(f64::INFINITY, f64::min);
            let next_s = next_arrival_planner_s.min(next_stream_s);
            if !next_s.is_finite() {
                break;
            }
            planner_s = planner_s.max(next_s);
            continue;
        }

        let mut planned_this_pass = false;
        while active_streams.len() < MUX_PROVIDER_STREAMS && !pending.is_empty() {
            let should_start = active_streams.is_empty()
                || predicted_lateness_seconds(&pending, &active_streams, &slots, planner_s)
                    > MUX_OVERFLOW_BACKLOG_SECONDS;
            if !should_start {
                break;
            }
            let selected = select_mux_slots(&pending, &slots);
            if selected.is_empty() {
                break;
            }
            let selected_set = selected.iter().copied().collect::<BTreeSet<_>>();
            pending.retain(|slot_index| !selected_set.contains(slot_index));
            let mux_audio_s = mux_audio_seconds_for_slots(&selected, &slots);
            let provider_s = elevenlabs_processing_seconds(mux_audio_s, 1);
            let mux_complete_s = planner_s + provider_s;
            active_streams.push(mux_complete_s);

            mux_jobs += 1;
            provider_busy_s += provider_s;
            provider_cost_audio_s += provider_concurrency_cost_audio_seconds(mux_audio_s, 1);
            total_selected_slots += selected.len();
            max_slots_per_mux = max_slots_per_mux.max(selected.len());
            total_mux_audio_s += mux_audio_s;
            max_mux_audio_s = max_mux_audio_s.max(mux_audio_s);
            for slot_index in selected {
                completions[slot_index] = mux_complete_s;
                scheduled_slots += 1;
            }
            planned_this_pass = true;
        }
        if planned_this_pass {
            continue;
        }
        let next_arrival_planner_s = slots
            .get(next_arrival)
            .map(|slot| slot.arrival_s + MUX_BATCH_DELAY_SECONDS)
            .unwrap_or(f64::INFINITY);
        let next_stream_s = active_streams.iter().copied().fold(f64::INFINITY, f64::min);
        let next_s = next_arrival_planner_s.min(next_stream_s);
        if !next_s.is_finite() {
            break;
        }
        planner_s = planner_s.max(next_s);
    }

    let input_wall_s = slots
        .iter()
        .map(|slot| slot.segment_start_s + slot.duration_s)
        .fold(0.0, f64::max);
    let total_speech_s = slots.iter().map(|slot| slot.duration_s).sum::<f64>();
    let completed_at_s = completions.iter().copied().fold(0.0, f64::max);
    let wake_waits = scenario
        .wake_checks
        .iter()
        .map(|wake| {
            let ready_at_s = slots
                .iter()
                .enumerate()
                .filter(|(_, slot)| {
                    slot.room == wake.room && slot.segment_start_s <= wake.closed_at_s
                })
                .map(|(index, _)| completions[index])
                .fold(wake.closed_at_s, f64::max);
            (
                wake.label.to_string(),
                (ready_at_s - wake.closed_at_s).max(0.0),
            )
        })
        .collect::<Vec<_>>();

    SimulationReport {
        name: scenario.name,
        slots: slots.len(),
        mux_jobs,
        input_wall_s,
        total_speech_s,
        provider_busy_s,
        provider_cost_audio_s,
        completed_at_s,
        drain_after_input_s: (completed_at_s - input_wall_s).max(0.0),
        mean_slots_per_mux: total_selected_slots as f64 / mux_jobs.max(1) as f64,
        max_slots_per_mux,
        mean_mux_audio_s: total_mux_audio_s / mux_jobs.max(1) as f64,
        max_mux_audio_s,
        provider_utilization_until_drain: provider_busy_s
            / (MUX_PROVIDER_STREAMS as f64 * completed_at_s.max(1.0)),
        wake_waits,
    }
}

fn select_mux_slots(pending: &[usize], slots: &[Slot]) -> Vec<usize> {
    let mut selected = Vec::new();
    let mut priorities = pending
        .iter()
        .map(|slot_index| slots[*slot_index].priority)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    priorities.sort_by(|left, right| right.cmp(left));
    for priority in priorities {
        let mut flows = fair_slot_flows(pending, slots, priority);
        loop {
            let mut added = false;
            for (_flow_key, queue) in &mut flows {
                let Some(slot_index) = queue.pop_front() else {
                    continue;
                };
                if selected.len() >= MUX_MAX_SLOTS {
                    return selected;
                }
                let mut candidate = selected.clone();
                candidate.push(slot_index);
                if !selected.is_empty()
                    && mux_audio_seconds_for_slots(&candidate, slots) > MUX_MAX_AUDIO_SECONDS
                {
                    continue;
                }
                selected.push(slot_index);
                added = true;
            }
            if !added
                || selected.len() >= MUX_MAX_SLOTS
                || flows.iter().all(|(_, queue)| queue.is_empty())
            {
                break;
            }
        }
        if selected.len() >= MUX_MAX_SLOTS
            || mux_audio_seconds_for_slots(&selected, slots) >= MUX_MAX_AUDIO_SECONDS
        {
            break;
        }
    }
    selected
}

fn fair_slot_flows(
    pending: &[usize],
    slots: &[Slot],
    priority: i64,
) -> Vec<(String, VecDeque<usize>)> {
    let mut grouped = BTreeMap::<String, Vec<usize>>::new();
    for slot_index in pending
        .iter()
        .copied()
        .filter(|slot_index| slots[*slot_index].priority == priority)
    {
        let slot = &slots[slot_index];
        grouped
            .entry(format!("{}:{}", slot.room, slot.speaker))
            .or_default()
            .push(slot_index);
    }
    let mut flows = grouped
        .into_iter()
        .map(|(flow_key, mut slot_indexes)| {
            slot_indexes.sort_by(|left, right| compare_claim_order(&slots[*left], &slots[*right]));
            let first_arrival = slot_indexes
                .first()
                .map(|slot_index| slots[*slot_index].arrival_s)
                .unwrap_or(0.0);
            (first_arrival, flow_key, VecDeque::from(slot_indexes))
        })
        .collect::<Vec<_>>();
    flows.sort_by(|left, right| {
        left.0
            .partial_cmp(&right.0)
            .unwrap_or(Ordering::Equal)
            .then_with(|| left.1.cmp(&right.1))
    });
    flows
        .into_iter()
        .map(|(_, flow_key, queue)| (flow_key, queue))
        .collect()
}

fn mux_audio_seconds_for_slots(selected: &[usize], slots: &[Slot]) -> f64 {
    let speech_duration_s = selected
        .iter()
        .map(|slot_index| slots[*slot_index].duration_s)
        .sum::<f64>();
    mux_audio_seconds(speech_duration_s, selected.len())
}

fn mux_audio_seconds(speech_duration_s: f64, slot_count: usize) -> f64 {
    if slot_count == 0 {
        return 0.0;
    }
    let guard_count = slot_count.saturating_mul(2).saturating_sub(1);
    speech_duration_s + guard_count as f64 * MUX_GUARD_SECONDS
}

fn compare_arrival_then_id(left: &Slot, right: &Slot) -> Ordering {
    left.arrival_s
        .partial_cmp(&right.arrival_s)
        .unwrap_or(Ordering::Equal)
        .then_with(|| left.id.cmp(&right.id))
}

fn predicted_lateness_seconds(
    pending: &[usize],
    active_streams: &[f64],
    slots: &[Slot],
    planner_s: f64,
) -> f64 {
    if pending.is_empty() || active_streams.is_empty() {
        return 0.0;
    }
    let mut remaining = pending.to_vec();
    let mut stream_available = active_streams.to_vec();
    let mut max_lateness = 0.0_f64;
    while !remaining.is_empty() {
        stream_available.sort_by(|left, right| left.partial_cmp(right).unwrap_or(Ordering::Equal));
        let selected = select_mux_slots(&remaining, slots);
        if selected.is_empty() {
            break;
        }
        let mux_audio_s = mux_audio_seconds_for_slots(&selected, slots);
        let start_s = planner_s.max(stream_available[0]);
        let finish_s = start_s + elevenlabs_processing_seconds(mux_audio_s, 1);
        for slot_index in &selected {
            let slot = &slots[*slot_index];
            let budget_s = if slot.priority >= 1000 {
                MUX_WAKE_LATENCY_BUDGET_SECONDS
            } else {
                MUX_NORMAL_LATENCY_BUDGET_SECONDS
            };
            let deadline_s = slot.segment_start_s + slot.duration_s + budget_s;
            max_lateness = max_lateness.max((finish_s - deadline_s).max(0.0));
        }
        stream_available[0] = finish_s;
        let selected_set = selected.into_iter().collect::<BTreeSet<_>>();
        remaining.retain(|slot_index| !selected_set.contains(slot_index));
    }
    max_lateness
}

fn compare_claim_order(left: &Slot, right: &Slot) -> Ordering {
    right
        .priority
        .cmp(&left.priority)
        .then_with(|| {
            left.arrival_s
                .partial_cmp(&right.arrival_s)
                .unwrap_or(Ordering::Equal)
        })
        .then_with(|| left.speaker.cmp(&right.speaker))
        .then_with(|| left.id.cmp(&right.id))
}

fn print_report(report: &SimulationReport) {
    println!();
    println!("scenario: {}", report.name);
    println!("  input wall: {:.1}s", report.input_wall_s);
    println!(
        "  slots: {}, mux jobs: {}, mean/max slots per mux: {:.2}/{}",
        report.slots, report.mux_jobs, report.mean_slots_per_mux, report.max_slots_per_mux
    );
    println!(
        "  total speech: {:.1}s, provider cost: {:.1} mono audio-seconds",
        report.total_speech_s, report.provider_cost_audio_s
    );
    println!(
        "  mean/max mux audio: {:.2}s/{:.2}s",
        report.mean_mux_audio_s, report.max_mux_audio_s
    );
    println!(
        "  completed at: {:.1}s, drain after input: {:.1}s",
        report.completed_at_s, report.drain_after_input_s
    );
    println!(
        "  provider busy: {:.1}s, utilization until drain: {:.1}%",
        report.provider_busy_s,
        report.provider_utilization_until_drain * 100.0
    );
    for (label, wait_s) in &report.wake_waits {
        println!("  agent wait after {label} close: {wait_s:.1}s");
    }
}
