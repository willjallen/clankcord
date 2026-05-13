use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::runtime::timeline::{
    event_start, event_text, isoformat_z, new_id, parse_duration, utc_now,
};

pub const ROUTER_LOOKBACK_SECONDS: i64 = 30;
pub const ROUTER_FOLLOWUP_IDLE_SECONDS: i64 = 30;
pub const ROUTER_MAX_FOLLOWUP_SECONDS: i64 = 5 * 60;
pub const WAKE_CONTEXT_SECONDS: i64 = ROUTER_LOOKBACK_SECONDS;

static WAKE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?i)\b(?:hey|hay)\s*,?\s+(?:clanky|klanky|clankey|clankie|clanki|planky|plankey|plankie|blanky|blankey|blankie|manky|mankey|mankie)\b",
    )
    .unwrap()
});
static COMMAND_HINT_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?i)\b(join (?:the )?(?:room|voice|channel|lounge|call|chat|vc|[a-z0-9 -]+lounge)|come (?:here|in|into|to (?:the )?[a-z0-9 -]+lounge)|get in(?: here| the room| the channel| the call)?|start (?:a )?(?:live )?transcript|(?:pull up|show|save) (?:a |the |this |that |live |draft )?transcript|make (?:this|that|it|the conversation)? ?permanent|materialize(?: (?:a |the |this |that )?transcript)?|forget (?:that|this|the last)|pause|resume|stop listening|deafen|deaf(?:en)? yourself|undefin|undeafen|on deafen|come off (?:of )?mute|get (?:your ass )?(?:the fuck )?out(?: of (?:here|the room))?|fuck off|leave)\b",
    )
    .unwrap()
});
static ADDRESS_OVERRIDE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)\b(?:actually\s+do\s+this|actually\s+(?:do|run|answer|make|start|stop|cancel|tell|find|summarize|materialize|leave|deafen)\b)").unwrap()
});
static LEAVE_COMMAND_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)\b(?:leave|fuck off|go away|get (?:your ass )?(?:the fuck )?out(?: of (?:here|the room))?)\b").unwrap()
});
static JOIN_COMMAND_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)\b(?:join (?:the )?(?:room|voice|channel|lounge|call|chat|vc|[a-z0-9 -]+lounge)|come (?:here|in|into|to (?:the )?[a-z0-9 -]+lounge)|get in(?: here| the room| the channel| the call)?)\b").unwrap()
});
static TIME_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)\b(?:from|for|last|past)\s+(?P<count>\d+)\s*(?P<unit>seconds?|minutes?|mins?|hours?|hrs?|days?)\b").unwrap()
});
static ROOM_TARGET_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)\b(?:to|in|into|from|leave|join|deafen|pause|resume)\s+(?:the\s+)?(?P<room>[a-z0-9][a-z0-9 -]{0,40}\s+(?:lounge|longue|launch|lunge))\b").unwrap()
});

pub const COMMAND_KINDS: &[&str] = &[
    "agent_task",
    "start_live_transcript",
    "start_draft_transcript",
    "materialize_transcript",
    "make_permanent",
    "pause_listening",
    "deafen_listening",
    "resume_listening",
    "forget_window",
    "leave_room",
    "join_room",
];
pub const ROUTER_ACTIONS: &[&str] = &[
    "dispatch_now",
    "wait_for_more",
    "ignore",
    "cancel_job",
    "amend_job",
    "replace_job",
];

pub fn wake_or_command_candidate(text: &str) -> bool {
    !text.trim().is_empty() && has_wake_phrase(text)
}

pub fn has_wake_phrase(text: &str) -> bool {
    WAKE_RE.is_match(text)
}

pub fn has_command_hint(text: &str) -> bool {
    COMMAND_HINT_RE.is_match(text)
}

pub fn router_action(result: &Value) -> String {
    let action = string_field(result, "action");
    if !action.is_empty() {
        action
    } else if result.get("is_command").and_then(Value::as_bool) == Some(true) {
        "dispatch_now".to_string()
    } else {
        "ignore".to_string()
    }
}

pub fn event_speaker_id(event: &Value) -> String {
    first_value_string(event, &["speaker_user_id", "speakerId", "user_id"])
}

pub fn event_id(event: &Value) -> String {
    first_value_string(event, &["event_id", "eventId"])
}

pub fn activation_context_events(
    candidate_event: &Value,
    recent_events: &[Value],
    max_seconds: i64,
) -> Vec<Value> {
    let text = event_text(candidate_event);
    let source_id = event_id(candidate_event);
    let candidate_start = event_start(candidate_event).unwrap_or_else(utc_now);
    let activation_granted = candidate_event
        .get("router_activation_granted")
        .and_then(Value::as_bool)
        == Some(true);
    if !has_wake_phrase(&text) && !activation_granted {
        return Vec::new();
    }
    let mut ordered: Vec<Value> = recent_events
        .iter()
        .filter(|event| !event_text(event).is_empty() || event_id(event) == source_id)
        .cloned()
        .collect();
    ordered.sort_by_key(|event| event_start(event).unwrap_or(candidate_start));
    if !ordered.iter().any(|event| event_id(event) == source_id) {
        ordered.push(candidate_event.clone());
    }
    let window_start = candidate_start - chrono::Duration::seconds(max_seconds);
    let mut selected = Vec::new();
    for event in ordered {
        if event_start(&event).is_some_and(|started| started < window_start) {
            continue;
        }
        selected.push(event);
    }
    if !selected.iter().any(|event| event_id(event) == source_id) {
        selected.push(candidate_event.clone());
    }
    selected.sort_by_key(|event| event_start(event).unwrap_or(candidate_start));
    selected
}

pub fn has_activation_context(candidate_event: &Value, recent_events: &[Value]) -> bool {
    !activation_context_events(candidate_event, recent_events, WAKE_CONTEXT_SECONDS).is_empty()
}

pub fn activated_text(
    candidate_event: &Value,
    recent_events: &[Value],
) -> (String, Vec<String>, bool) {
    let context = activation_context_events(candidate_event, recent_events, WAKE_CONTEXT_SECONDS);
    if context.is_empty() {
        return (
            event_text(candidate_event),
            non_empty_vec(vec![event_id(candidate_event)]),
            false,
        );
    }
    let texts = context
        .iter()
        .map(event_text)
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let ids = non_empty_vec(context.iter().map(event_id).collect());
    let current_has_wake = has_wake_phrase(&event_text(candidate_event));
    (texts.trim().to_string(), ids, current_has_wake)
}

pub fn instruction_events(candidate_event: &Value, recent_events: &[Value]) -> Vec<Value> {
    let context = activation_context_events(candidate_event, recent_events, WAKE_CONTEXT_SECONDS);
    if context.is_empty() {
        return Vec::new();
    }
    let source_id = event_id(candidate_event);
    let candidate_start = event_start(candidate_event).unwrap_or_else(utc_now);
    let requester_id = event_speaker_id(candidate_event);
    let mut selected = Vec::new();
    for event in context {
        let current_id = event_id(&event);
        if current_id == source_id {
            selected.push(event);
            continue;
        }
        if event_start(&event).is_some_and(|started| started < candidate_start) {
            continue;
        }
        let speaker_id = event_speaker_id(&event);
        if !requester_id.is_empty() && !speaker_id.is_empty() && speaker_id != requester_id {
            continue;
        }
        selected.push(event);
    }
    selected.sort_by_key(|event| event_start(event).unwrap_or(candidate_start));
    selected
}

pub fn text_from_wake_onward(text: &str) -> String {
    WAKE_RE
        .find(text)
        .map(|mat| text[mat.start()..].trim().to_string())
        .unwrap_or_else(|| text.to_string())
}

pub fn activated_instruction_text(
    candidate_event: &Value,
    recent_events: &[Value],
) -> (String, Vec<String>, bool) {
    let events = instruction_events(candidate_event, recent_events);
    if events.is_empty() {
        return (
            event_text(candidate_event),
            non_empty_vec(vec![event_id(candidate_event)]),
            false,
        );
    }
    let source_id = event_id(candidate_event);
    let mut texts = Vec::new();
    let mut ids = Vec::new();
    for event in events {
        let mut text = event_text(&event);
        if event_id(&event) == source_id {
            text = text_from_wake_onward(&text);
        }
        if !text.is_empty() {
            texts.push(text);
        }
        let id = event_id(&event);
        if !id.is_empty() {
            ids.push(id);
        }
    }
    let current_has_wake = has_wake_phrase(&event_text(candidate_event));
    (texts.join(" ").trim().to_string(), ids, current_has_wake)
}

pub fn matched_text(pattern: &Regex, text: &str) -> String {
    pattern
        .find(text)
        .map(|mat| mat.as_str().to_string())
        .unwrap_or_default()
}

pub fn snippet(text: &str, limit: usize) -> String {
    let cleaned = Regex::new(r"\s+")
        .unwrap()
        .replace_all(text, " ")
        .trim()
        .to_string();
    if cleaned.len() <= limit {
        cleaned
    } else {
        format!("{}...", cleaned[..limit].trim_end())
    }
}

pub fn normalize_relative_time(text: &str, default: &str) -> String {
    let Some(captures) = TIME_RE.captures(text) else {
        let lowered = text.to_lowercase();
        if lowered.contains("hour ago") {
            return "-1h".to_string();
        }
        if lowered.contains("ten minutes") {
            return "-10m".to_string();
        }
        if lowered.contains("twenty minutes") {
            return "-20m".to_string();
        }
        return default.to_string();
    };
    let count = captures.name("count").map(|m| m.as_str()).unwrap_or("10");
    let unit = captures
        .name("unit")
        .map(|m| m.as_str().to_lowercase())
        .unwrap_or_default();
    let suffix = if unit.starts_with("second") {
        "s"
    } else if unit.starts_with("hour") || unit.starts_with("hr") {
        "h"
    } else if unit.starts_with("day") {
        "d"
    } else {
        "m"
    };
    format!("-{count}{suffix}")
}

pub fn target_room_for_text(text: &str) -> String {
    let Some(captures) = ROOM_TARGET_RE.captures(text) else {
        return String::new();
    };
    let room = captures.name("room").map(|m| m.as_str()).unwrap_or("");
    let collapsed = Regex::new(r"\s+").unwrap().replace_all(room, " ");
    Regex::new(r"(?i)^the\s+")
        .unwrap()
        .replace(
            collapsed.trim_matches(&[',', '.', ';', ':', '-', ' '][..]),
            "",
        )
        .to_string()
}

pub fn command_kind_for_text(text: &str) -> String {
    let lowered = text.to_lowercase();
    if lowered.contains("resume")
        || lowered.contains("undeafen")
        || lowered.contains("undefin")
        || lowered.contains("on deafen")
        || lowered.contains("come off mute")
        || lowered.contains("come off of mute")
    {
        return "resume_listening".to_string();
    }
    if LEAVE_COMMAND_RE.is_match(text) {
        return "leave_room".to_string();
    }
    if JOIN_COMMAND_RE.is_match(text) {
        return "join_room".to_string();
    }
    if lowered.contains("pause") || lowered.contains("stop listening") || lowered.contains("deafen")
    {
        return "deafen_listening".to_string();
    }
    if lowered.contains("live transcript") {
        return "start_live_transcript".to_string();
    }
    if lowered.contains("start") && lowered.contains("transcript") {
        return "start_draft_transcript".to_string();
    }
    if lowered.contains("make") && lowered.contains("permanent") {
        return "make_permanent".to_string();
    }
    if lowered.contains("materialize")
        || (lowered.contains("transcript")
            && (lowered.contains("pull up")
                || lowered.contains("show")
                || lowered.contains("save")))
    {
        return "materialize_transcript".to_string();
    }
    if lowered.contains("forget") {
        return "forget_window".to_string();
    }
    String::new()
}

pub fn clean_question_text(text: &str) -> String {
    let mut cleaned = WAKE_RE.replace_all(text, "").to_string();
    for pattern in [
        r"(?i)\b(?:please|can you|could you|would you)\b",
        r"(?i)\b(?:tell me|answer|look up|give me)\b",
        r"(?i)\b(?:in|to) (?:the )?agent chat(?: channel)?\b",
    ] {
        cleaned = Regex::new(pattern)
            .unwrap()
            .replace_all(&cleaned, " ")
            .to_string();
    }
    Regex::new(r"\s+")
        .unwrap()
        .replace_all(&cleaned, " ")
        .trim_matches(&[',', '.', ';', ':', '-', ' '][..])
        .to_string()
}

pub fn requires_confirmation(command_kind: &str) -> bool {
    command_kind == "forget_window"
}

pub fn dedupe_hash(
    guild_id: &str,
    voice_channel_id: &str,
    candidate_event_id: &str,
    command_kind: &str,
    arguments: &Value,
) -> String {
    let raw = serde_json::to_string(&serde_json::json!({
        "arguments": arguments,
        "candidate_event_id": candidate_event_id,
        "command_kind": command_kind,
        "guild_id": guild_id,
        "voice_channel_id": voice_channel_id
    }))
    .unwrap_or_default();
    format!("{:x}", Sha256::digest(raw.as_bytes()))
}

pub fn evaluate_router_candidate(
    candidate_event: &Value,
    recent_events: &[Value],
    room_status: &Value,
    last_commands: Option<&[Value]>,
) -> Value {
    let raw_text = event_text(candidate_event);
    let (context_text, mut context_source_event_ids, current_has_wake) =
        activated_text(candidate_event, recent_events);
    let (instruction_text, mut instruction_source_event_ids, _) =
        activated_instruction_text(candidate_event, recent_events);
    let source_event_id = event_id(candidate_event);
    let guild_id = first_non_empty([
        first_value_string(candidate_event, &["guild_id", "guildId"]),
        string_field(room_status, "guild_id"),
    ]);
    let channel_id = first_non_empty([
        first_value_string(candidate_event, &["voice_channel_id", "channelId"]),
        string_field(room_status, "voice_channel_id"),
    ]);
    let capture_run_id = first_non_empty([
        first_value_string(candidate_event, &["capture_run_id", "captureRunId"]),
        string_field(room_status, "capture_run_id"),
    ]);
    let voice_bot_id = first_non_empty([
        first_value_string(candidate_event, &["voice_bot_id", "botId"]),
        string_field(room_status, "voice_bot_id"),
    ]);
    let speaker_id = first_value_string(candidate_event, &["speaker_user_id", "speakerId"]);
    let speaker_label = first_non_empty([
        first_value_string(candidate_event, &["speaker_label", "speakerLabel"]),
        speaker_id.clone(),
    ]);

    if context_source_event_ids.is_empty() && !source_event_id.is_empty() {
        context_source_event_ids.push(source_event_id.clone());
    }
    if instruction_source_event_ids.is_empty() && !source_event_id.is_empty() {
        instruction_source_event_ids.push(source_event_id.clone());
    }
    if !has_activation_context(candidate_event, recent_events) {
        let agent_reason = format!(
            "I ignored this because the candidate event itself did not contain a Hey Clanky wake phrase or supported STT variant. Commands are only evaluated from explicit Hey Clanky wake events. Candidate text: {:?}.",
            snippet(&raw_text, 220)
        );
        return serde_json::json!({
            "action": "ignore",
            "is_command": false,
            "confidence": 0.0,
            "wake_phrase_detected": false,
            "reason": agent_reason,
            "agent_reason": agent_reason,
            "candidate_text": raw_text,
            "source_event_ids": instruction_source_event_ids,
            "context_source_event_ids": context_source_event_ids
        });
    }

    let command_kind = command_kind_for_text(&instruction_text);
    if command_kind.is_empty() {
        let wake_match = matched_text(
            &WAKE_RE,
            if current_has_wake {
                &raw_text
            } else {
                &instruction_text
            },
        );
        let activation_granted = candidate_event
            .get("router_activation_granted")
            .and_then(Value::as_bool)
            == Some(true);
        let agent_reason = if activation_granted && !current_has_wake {
            format!(
                "I treated this as a granted follow-up after a prior Clanky clarification, but the follow-up text did not contain a direct built-in Clawcord control. The router model should decide whether it is a general agent request. Instruction text: {:?}.",
                snippet(&instruction_text, 220)
            )
        } else {
            format!(
                "I heard the wake phrase variant {wake_match:?}, but the post-activation instruction text did not contain a direct built-in Clawcord control. The router model should decide whether it is a general agent request. Instruction text: {:?}.",
                snippet(&instruction_text, 220)
            )
        };
        return serde_json::json!({
            "action": "ignore",
            "is_command": false,
            "confidence": 0.25,
            "wake_phrase_detected": true,
            "activation_granted_by_prior_wait": activation_granted,
            "wake_phrase_on_candidate": current_has_wake,
            "reason": agent_reason,
            "agent_reason": agent_reason,
            "source_event_ids": instruction_source_event_ids,
            "context_source_event_ids": context_source_event_ids,
            "candidate_text": raw_text,
            "activated_text": context_text,
            "instruction_text": instruction_text
        });
    }

    let relative_start = normalize_relative_time(&instruction_text, "-10m");
    let mut arguments = Map::new();
    if [
        "agent_task",
        "start_live_transcript",
        "start_draft_transcript",
        "materialize_transcript",
        "forget_window",
    ]
    .contains(&command_kind.as_str())
    {
        arguments.insert("relative_start".to_string(), Value::String(relative_start));
        arguments.insert("relative_end".to_string(), Value::String("now".to_string()));
        let lowered = instruction_text.to_lowercase();
        if Regex::new(r"\btoday\b").unwrap().is_match(&lowered) {
            arguments.insert(
                "date_reference".to_string(),
                Value::String("today".to_string()),
            );
        } else if Regex::new(r"\byesterday\b").unwrap().is_match(&lowered) {
            arguments.insert(
                "date_reference".to_string(),
                Value::String("yesterday".to_string()),
            );
        }
        if let Some(date_match) = Regex::new(r"\b\d{4}-\d{2}-\d{2}\b").unwrap().find(&lowered) {
            arguments.insert(
                "date_reference".to_string(),
                Value::String(date_match.as_str().to_string()),
            );
        }
        if lowered.contains("work-related") || lowered.contains("work related") {
            arguments.insert("work_related".to_string(), Value::Bool(true));
        }
    }
    if command_kind == "start_live_transcript" {
        arguments.insert("live".to_string(), Value::Bool(true));
        arguments.insert("refine".to_string(), Value::Bool(false));
    }
    if command_kind == "make_permanent" {
        arguments.insert(
            "context_reference".to_string(),
            Value::String("this conversation".to_string()),
        );
        arguments.insert("refine".to_string(), Value::Bool(true));
    }
    if command_kind == "agent_task" {
        arguments.insert(
            "request".to_string(),
            Value::String(clean_question_text(&instruction_text)),
        );
        arguments.insert("raw_text".to_string(), Value::String(raw_text.clone()));
        arguments.insert(
            "activated_text".to_string(),
            Value::String(context_text.clone()),
        );
        arguments.insert(
            "instruction_text".to_string(),
            Value::String(instruction_text.clone()),
        );
        arguments.insert(
            "respond_in".to_string(),
            Value::String("agent_chat".to_string()),
        );
    }
    if [
        "join_room",
        "leave_room",
        "deafen_listening",
        "pause_listening",
        "resume_listening",
    ]
    .contains(&command_kind.as_str())
    {
        let target_room = target_room_for_text(&instruction_text);
        if !target_room.is_empty() {
            arguments.insert("target_room".to_string(), Value::String(target_room));
        }
    }
    let arguments_value = Value::Object(arguments);
    let source_event_ids = if instruction_source_event_ids.is_empty() {
        context_source_event_ids.clone()
    } else {
        instruction_source_event_ids.clone()
    };
    let dedupe = dedupe_hash(
        &guild_id,
        &channel_id,
        &if source_event_ids.is_empty() {
            source_event_id.clone()
        } else {
            source_event_ids.join("|")
        },
        &command_kind,
        &arguments_value,
    );
    for command in last_commands.unwrap_or(&[]) {
        if string_field(command, "dedupe_hash") == dedupe {
            let agent_reason = format!(
                "I recognized {command_kind}, but this normalized command already matched a recently dispatched command for source events {}.",
                if source_event_ids.is_empty() {
                    source_event_id.clone()
                } else {
                    source_event_ids.join(", ")
                }
            );
            return serde_json::json!({
                "action": "ignore",
                "is_command": false,
                "confidence": 0.1,
                "reason": agent_reason,
                "agent_reason": agent_reason,
                "source_event_ids": source_event_ids,
                "context_source_event_ids": context_source_event_ids,
                "candidate_text": raw_text,
                "activated_text": context_text,
                "instruction_text": instruction_text
            });
        }
    }
    let activation_granted = candidate_event
        .get("router_activation_granted")
        .and_then(Value::as_bool)
        == Some(true);
    let confidence = if current_has_wake { 0.93 } else { 0.82 };
    let wake_match = matched_text(
        &WAKE_RE,
        if current_has_wake {
            &raw_text
        } else {
            &instruction_text
        },
    );
    let command_match = matched_text(&COMMAND_HINT_RE, &instruction_text);
    let recognized_as = if command_kind == "agent_task" {
        "a request for an agent task"
    } else {
        "a built-in voice control"
    };
    let agent_reason = if activation_granted && !current_has_wake {
        format!(
            "I treated this as a granted follow-up after a prior Clanky clarification, reviewed the follow-up instruction text, recognized action phrase {command_match:?} as {recognized_as}, and used source event(s) {}.",
            source_event_ids.join(", ")
        )
    } else {
        format!(
            "I activated on wake variant {wake_match:?}, reviewed the post-activation instruction text, recognized action phrase {command_match:?} as {recognized_as}, and used source event(s) {}.",
            source_event_ids.join(", ")
        )
    };
    serde_json::json!({
        "action": "dispatch_now",
        "is_command": true,
        "confidence": confidence,
        "wake_phrase_detected": true,
        "activation_granted_by_prior_wait": activation_granted,
        "wake_phrase_on_candidate": current_has_wake,
        "command_kind": command_kind,
        "requested_by_user_id": speaker_id,
        "requested_by_speaker_label": speaker_label,
        "guild_id": guild_id,
        "voice_channel_id": channel_id,
        "capture_run_id": capture_run_id,
        "voice_bot_id": voice_bot_id,
        "arguments": arguments_value,
        "requires_confirmation": requires_confirmation(&command_kind),
        "reason": agent_reason,
        "agent_reason": agent_reason,
        "acknowledgement_text": acknowledgement_text_for_command(&command_kind),
        "candidate_text": raw_text,
        "matched_wake_phrase": wake_match,
        "matched_command_phrase": command_match,
        "source_event_ids": source_event_ids,
        "context_source_event_ids": context_source_event_ids,
        "activated_text": context_text,
        "instruction_text": instruction_text,
        "dedupe_hash": dedupe
    })
}

pub fn acknowledgement_text_for_command(command_kind: &str) -> String {
    match command_kind {
        "join_room" => "Joining the room now.",
        "leave_room" => "Leaving the room now.",
        "deafen_listening" => "Deafening now.",
        "resume_listening" => "Listening again.",
        "start_live_transcript"
        | "start_draft_transcript"
        | "materialize_transcript"
        | "make_permanent" => "Working on that transcript for you.",
        _ => "Working on that for you.",
    }
    .to_string()
}

pub fn validate_router_result(result: &Value) -> (bool, String) {
    if !result.is_object() {
        return (false, "router result is not an object".to_string());
    }
    let action = router_action(result);
    if !ROUTER_ACTIONS.contains(&action.as_str()) {
        return (
            false,
            format!("unsupported action {:?}", result.get("action")),
        );
    }
    if ["ignore", "wait_for_more"].contains(&action.as_str()) {
        return (false, non_empty(string_field(result, "reason"), action));
    }
    if ["cancel_job", "amend_job", "replace_job"].contains(&action.as_str())
        && string_field(result, "target_job_id").is_empty()
        && result.get("target_job_ids").is_none_or(Value::is_null)
    {
        return (false, "router result missing target_job_id".to_string());
    }
    if action == "cancel_job" {
        return (true, "ok".to_string());
    }
    for field in ["guild_id", "voice_channel_id", "source_event_ids"] {
        if result.get(field).is_none_or(value_is_empty) {
            return (false, format!("router result missing {field}"));
        }
    }
    let command_kind = string_field(result, "command_kind");
    if !command_kind.is_empty() && !COMMAND_KINDS.contains(&command_kind.as_str()) {
        return (
            false,
            format!("unsupported command_kind {:?}", result.get("command_kind")),
        );
    }
    (true, "ok".to_string())
}

pub fn command_to_job_kind(command_kind: &str) -> String {
    BTreeMap::from([
        ("agent_task", "agent_task"),
        ("start_live_transcript", "materialize_transcript"),
        ("start_draft_transcript", "materialize_transcript"),
        ("materialize_transcript", "materialize_transcript"),
        ("make_permanent", "make_permanent"),
        ("pause_listening", "pause_listening"),
        ("deafen_listening", "deafen_listening"),
        ("resume_listening", "resume_listening"),
        ("forget_window", "forget_window"),
        ("leave_room", "leave_room"),
        ("join_room", "join_room"),
    ])
    .get(command_kind)
    .copied()
    .unwrap_or(command_kind)
    .to_string()
}

pub fn command_window_times(
    result: &Value,
    now: Option<DateTime<Utc>>,
) -> (DateTime<Utc>, DateTime<Utc>) {
    let current = now.unwrap_or_else(utc_now);
    let args = result.get("arguments").and_then(Value::as_object);
    let relative_start = args
        .and_then(|map| map.get("relative_start"))
        .and_then(Value::as_str)
        .unwrap_or("-10m");
    let delta = parse_duration(relative_start).unwrap_or_else(|| chrono::Duration::minutes(-10));
    (current + delta, current)
}

pub const ROUTER_SYSTEM_PROMPT: &str = r#"You are clanky-voice-router.

Your job is only to decide whether a collected Discord voice wake window from one voice channel contains an actionable request addressed to Clanky.

Activation has already been gated deterministically. Usually the candidate event contains "Hey Clanky" or a plausible STT variant. In follow-up mode, Clawcord has already asked the user for missing detail after a prior wake-gated wait_for_more result, so the candidate event may be a non-wake continuation. You must inspect the entire collected interaction window, especially window_events, to decide whether someone is clearly talking to Clanky. Your main failure mode to avoid is routing when people are only discussing Clanky in third person, quoting an example, joking about Clanky, or mentioning what Clanky did.

Return JSON only. Do not answer the user. The transcript context is from exactly one voice channel. Always include your own concise reason for executing or not executing a command in the "reason" field.

When the user names another voice room, such as "art lounge", preserve that phrase in arguments.target_room or arguments.voice_channel_name rather than silently using the current channel. When the user gives a date phrase such as "today", "yesterday", or YYYY-MM-DD, preserve it in arguments.date_reference. When the user asks for "work-related" material, set arguments.work_related=true.

Use action="wait_for_more" only when the collected settle window clearly indicates the user intentionally addressed Clanky but has not provided enough actionable detail. Use action="ignore" when the wake phrase was accidental, third-person, quoted, joking, or has no actionable information need. Use action="dispatch_now" for a clear actionable request. Do not require imperative grammar: direct questions, requests for opinions, corrections, complaints about a prior Clanky answer, and statements like "what I want to know is ..." are actionable when they are addressed to Clanky. Use action="cancel_job", "amend_job", or "replace_job" only when the interaction context makes the target prior job clear and the job is still cancellable; for corrections to completed work, dispatch a new agent_task. If interaction_context.turn_history or interaction_context.recent_jobs contains a prior Clanky job, interpret the current window as a possible answer, correction, or replacement. When routing a request that references prior Clanky work, include arguments.previous_job_id from interaction_context.recent_jobs when identifiable. If you route any action that Clawcord should acknowledge, include a short "acknowledgement_text" that Clawcord can post before work starts; do not include a Discord mention because Clawcord adds it.

For requests that need reasoning, retrieval, summarization, research, planning, or external tool use, set command_kind="agent_task" or omit command_kind. The agent task selects the workflow after dispatch. Use a specific command_kind only for built-in Clawcord voice controls that the router should dispatch directly: join_room, leave_room, deafen_listening, resume_listening, pause_listening, start_live_transcript, start_draft_transcript, materialize_transcript, make_permanent, or forget_window."#;

pub fn packet_event_view(event: &Value) -> Value {
    serde_json::json!({
        "event_id": event_id(event),
        "kind": first_value_string(event, &["event_kind", "kind"]),
        "speaker_user_id": event_speaker_id(event),
        "speaker_label": first_value_string(event, &["speaker_label", "speakerLabel"]),
        "started_at": first_value_string(event, &["segment_start_time", "startedAt", "timestamp"]),
        "ended_at": first_value_string(event, &["segment_end_time", "endedAt"]),
        "text": event_text(event)
    })
}

pub fn compact_router_job_view(job: &Value) -> Value {
    let payload = job.get("payload").unwrap_or(&Value::Null);
    let command = payload.get("command").unwrap_or(&Value::Null);
    let arguments = command.get("arguments").unwrap_or(&Value::Null);
    let state = string_field(job, "state");
    serde_json::json!({
        "job_id": string_field(job, "job_id"),
        "kind": string_field(job, "kind"),
        "state": state.clone(),
        "guild_id": string_field(job, "guild_id"),
        "voice_channel_id": string_field(job, "voice_channel_id"),
        "requested_by_user_id": string_field(job, "requested_by_user_id"),
        "command_kind": string_field(command, "command_kind"),
        "question": first_non_empty([
            string_field(arguments, "question"),
            string_field(arguments, "request"),
        ]),
        "created_at": string_field(job, "created_at"),
        "updated_at": string_field(job, "updated_at"),
        "started_at": string_field(job, "started_at"),
        "cancellable": matches!(
            state.as_str(),
            "queued" | "running" | "waiting" | "cancel_requested" | "confirmation_pending"
        ),
        "cancel_requested": job.get("cancel_requested").and_then(Value::as_bool) == Some(true)
            || state == "cancel_requested",
    })
}

pub fn compact_room_status_for_router(room_status: &Value) -> Value {
    let Some(status) = room_status.as_object() else {
        return room_status.clone();
    };
    let mut compact = status.clone();
    if let Some(active_jobs) = status.get("activeJobs").and_then(Value::as_array) {
        compact.insert(
            "activeJobs".to_string(),
            Value::Array(
                active_jobs
                    .iter()
                    .filter(|job| job.is_object())
                    .map(compact_router_job_view)
                    .collect(),
            ),
        );
    }
    Value::Object(compact)
}

pub fn router_candidate_packet(
    candidate_event: &Value,
    recent_events: &[Value],
    room_status: &Value,
    router_window: Option<&Value>,
    heuristic_result: Option<&Value>,
    interaction_context: Option<&Value>,
) -> Value {
    let window = router_window
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let context = activation_context_events(candidate_event, recent_events, WAKE_CONTEXT_SECONDS);
    let instruction = instruction_events(candidate_event, recent_events);
    let (instruction_text, instruction_source_event_ids, _) =
        activated_instruction_text(candidate_event, recent_events);
    let compact_recent = recent_events
        .iter()
        .rev()
        .take(30)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(packet_event_view)
        .collect::<Vec<_>>();
    let activation_mode = non_empty(string_field(&window, "activation_mode"), "wake".to_string());
    serde_json::json!({
        "packet_id": new_id("routerpkt"),
        "created_at": isoformat_z(None),
        "agent": "clanky-voice-router",
        "activation_mode": activation_mode,
        "system_prompt": ROUTER_SYSTEM_PROMPT,
        "instructions": [
            "Decide whether this entire wake window contains an actionable request addressed to Clanky.",
            "The candidate_event is normally the deterministic wake event. In activation_mode=followup, it is the first non-wake answer after Clawcord asked for missing detail.",
            "Do not accept non-wake events as fresh activation unless activation_mode=followup or interaction_context shows an active wait_for_more continuation.",
            "Use window_events as the full collected interaction: 30 seconds before the candidate plus the idle-closed post-candidate window.",
            "Do not decide from candidate_event alone. Candidate text is only the anchor; the command may be explained across later window events.",
            "The deterministic heuristic only examines instruction_text for direct built-in controls. It intentionally ignores pre-wake context and does not classify general questions or agent work.",
            "Do not require imperative grammar: direct questions, requests for opinions, corrections, complaints about a prior Clanky answer, and statements like 'what I want to know is ...' are actionable when addressed to Clanky.",
            "Reject casual discussion about Clanky, quoted examples, jokes, third-person mentions, and wake windows with no actionable information need.",
            "Address override: if the wake-gated request says 'actually do this' or 'actually <verb>', treat it as intentionally addressed to Clanky even if the speaker frames it as an example.",
            "When the address override is present, do not reject solely because of third-person, hypothetical, quoted, or example framing; normalize the concrete action from surrounding words.",
            "Route when it is pretty clear someone is talking to Clanky and giving Clanky something to answer, redo, investigate, summarize, or operate.",
            "For requests that need reasoning, retrieval, summarization, research, planning, or external tool use, use command_kind=agent_task or omit command_kind.",
            "Use specific command_kind values only for built-in voice controls and transcript materialization commands that Clawcord should execute directly.",
            "Return wait_for_more only if the idle-closed settle window shows an intentional but incomplete Clanky request.",
            "Use interaction_context to understand previous acknowledgements, active jobs, recent completed jobs, and whether this is a correction or cancellation.",
            "Use interaction_context.turn_history to compose current follow-up text with prior wait_for_more, dispatch, or acknowledgement state.",
            "Use interaction_context.recent_jobs to resolve references like 'your last response', 'my last question', or 'the thing you just said'.",
            "When routing a request that references prior Clanky work, include arguments.previous_job_id from interaction_context.recent_jobs when identifiable.",
            "For both command and non-command outcomes, write the agent's own reason in the response.reason field.",
            "When routing, include acknowledgement_text: a short status phrase Clawcord can post before dispatch.",
            "If the user names another voice room, preserve it in arguments.target_room or arguments.voice_channel_name.",
            "If the user says today, yesterday, or an explicit YYYY-MM-DD date, preserve it in arguments.date_reference.",
            "If the user asks for work-related material, set arguments.work_related=true.",
        ],
        "supported_wake_variants": WAKE_RE.as_str(),
        "command_hint_pattern": COMMAND_HINT_RE.as_str(),
        "address_override_pattern": ADDRESS_OVERRIDE_RE.as_str(),
        "recognized_command_kinds": COMMAND_KINDS,
        "recognized_actions": ROUTER_ACTIONS,
        "wake_lookback_seconds": window.get("lookback_seconds").cloned().unwrap_or(Value::from(ROUTER_LOOKBACK_SECONDS)),
        "idle_seconds": window.get("idle_seconds").or_else(|| window.get("settle_seconds")).cloned().unwrap_or(Value::from(ROUTER_FOLLOWUP_IDLE_SECONDS)),
        "followup_idle_seconds": window.get("idle_seconds").or_else(|| window.get("settle_seconds")).cloned().unwrap_or(Value::from(ROUTER_FOLLOWUP_IDLE_SECONDS)),
        "max_followup_seconds": window.get("max_followup_seconds").cloned().unwrap_or(Value::from(ROUTER_MAX_FOLLOWUP_SECONDS)),
        "router_window": window,
        "interaction_context": interaction_context.cloned().unwrap_or_else(|| serde_json::json!({})),
        "heuristic_result": heuristic_result.cloned().unwrap_or_else(|| serde_json::json!({})),
        "heuristic_scope": "direct built-in controls only, using instruction_text",
        "instruction_text": instruction_text,
        "instruction_source_event_ids": instruction_source_event_ids,
        "candidate_event": packet_event_view(candidate_event),
        "instruction_events": instruction.iter().map(packet_event_view).collect::<Vec<_>>(),
        "window_events": context.iter().map(packet_event_view).collect::<Vec<_>>(),
        "activation_context_events": context.iter().map(packet_event_view).collect::<Vec<_>>(),
        "recent_events": compact_recent,
        "room_status": compact_room_status_for_router(room_status),
        "response_schema": {
            "action": "one of dispatch_now, wait_for_more, ignore, cancel_job, amend_job, replace_job",
            "is_command": "boolean",
            "confidence": "optional diagnostic number from 0 to 1; never use this to decide whether to route",
            "wake_phrase_detected": "boolean",
            "command_kind": "agent_task for normal agent work, a built-in voice control kind for direct controls, or omitted when action alone is sufficient",
            "arguments": "object with normalized command arguments",
            "arguments.target_room": "room name/slug/id when the request names a voice room",
            "arguments.date_reference": "today, yesterday, or YYYY-MM-DD when the request names a date",
            "arguments.work_related": "boolean when the request asks for work-related filtering",
            "arguments.previous_job_id": "prior Clanky job id when the request refers to a previous Clanky answer or question",
            "target_job_id": "job id when action cancels/amends/replaces an existing job",
            "source_event_ids": "array of event ids used as evidence",
            "reason": "the router agent's own concise reason for executing or not executing"
        },
        "limits": {"max_input_tokens": 2500, "max_output_tokens": 300, "temperature": 0}
    })
}

fn string_field(value: &Value, key: &str) -> String {
    match value.get(key) {
        Some(Value::String(text)) => text.trim().to_string(),
        Some(Value::Number(number)) => number.to_string(),
        Some(Value::Bool(boolean)) => boolean.to_string(),
        _ => String::new(),
    }
}

fn first_value_string(value: &Value, keys: &[&str]) -> String {
    keys.iter()
        .map(|key| string_field(value, key))
        .find(|value| !value.is_empty())
        .unwrap_or_default()
}

fn non_empty(value: String, fallback: String) -> String {
    if value.trim().is_empty() {
        fallback
    } else {
        value
    }
}

fn non_empty_vec(values: Vec<String>) -> Vec<String> {
    values
        .into_iter()
        .filter(|value| !value.is_empty())
        .collect()
}

fn first_non_empty<const N: usize>(values: [String; N]) -> String {
    values
        .into_iter()
        .find(|value| !value.is_empty())
        .unwrap_or_default()
}

fn value_is_empty(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(text) => text.is_empty(),
        Value::Array(values) => values.is_empty(),
        Value::Object(map) => map.is_empty(),
        _ => false,
    }
}
