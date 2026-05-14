use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::runtime::timeline::{event_start, event_text, utc_now};

pub const VOICE_ACTIVATION_LOOKBACK_SECONDS: i64 = 30;

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
static WAKE_CLEANUP_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)\b(?:hey|hay)\s*,?\s+clanky\b").unwrap());

const COMMAND_KINDS: &[&str] = &[
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

const COMMAND_ACTIONS: &[&str] = &["dispatch_now", "ignore"];

pub fn voice_command_action(result: &Value) -> String {
    let action = string_field(result, "action");
    if !action.is_empty() {
        action
    } else if result.get("is_command").and_then(Value::as_bool) == Some(true) {
        "dispatch_now".to_string()
    } else {
        "ignore".to_string()
    }
}

fn event_speaker_id(event: &Value) -> String {
    first_value_string(event, &["speaker_user_id", "speakerId", "user_id"])
}

fn event_id(event: &Value) -> String {
    first_value_string(event, &["event_id", "eventId"])
}

fn wake_detected(event: &Value) -> bool {
    event
        .get("wake")
        .and_then(|wake| wake.get("wake"))
        .and_then(Value::as_bool)
        == Some(true)
        || event.get("wake_detected").and_then(Value::as_bool) == Some(true)
}

fn wake_score(event: &Value) -> Option<f64> {
    finite_number(event.get("wake").and_then(|wake| wake.get("score")))
        .or_else(|| finite_number(event.get("wake_score")))
}

pub fn activation_context_events(candidate_event: &Value, recent_events: &[Value]) -> Vec<Value> {
    let source_id = event_id(candidate_event);
    let candidate_start = event_start(candidate_event).unwrap_or_else(utc_now);
    let window_start =
        candidate_start - chrono::Duration::seconds(VOICE_ACTIVATION_LOOKBACK_SECONDS);
    let speaker_id = event_speaker_id(candidate_event);
    let guild_id = first_value_string(candidate_event, &["guild_id", "guildId"]);
    let channel_id = first_value_string(candidate_event, &["voice_channel_id", "channelId"]);

    let mut ordered = recent_events
        .iter()
        .filter(|event| {
            if !source_id.is_empty() && event_id(event) == source_id {
                return true;
            }
            if event_start(event).is_some_and(|started| started < window_start) {
                return false;
            }
            if !speaker_id.is_empty() && event_speaker_id(event) != speaker_id {
                return false;
            }
            let event_guild = first_value_string(event, &["guild_id", "guildId"]);
            if !guild_id.is_empty() && !event_guild.is_empty() && event_guild != guild_id {
                return false;
            }
            let event_channel = first_value_string(event, &["voice_channel_id", "channelId"]);
            if !channel_id.is_empty() && !event_channel.is_empty() && event_channel != channel_id {
                return false;
            }
            !event_text(event).is_empty() || wake_detected(event)
        })
        .cloned()
        .collect::<Vec<_>>();
    if !ordered.iter().any(|event| event_id(event) == source_id) {
        ordered.push(candidate_event.clone());
    }
    ordered.sort_by_key(|event| event_start(event).unwrap_or(candidate_start));

    let Some(wake_index) = ordered.iter().rposition(wake_detected) else {
        return Vec::new();
    };
    ordered.into_iter().skip(wake_index).collect()
}

pub fn evaluate_voice_command(
    candidate_event: &Value,
    recent_events: &[Value],
    room_status: &Value,
) -> Value {
    let raw_text = event_text(candidate_event);
    let activation_context = activation_context_events(candidate_event, recent_events);
    let (instruction_text, source_event_ids, wake_on_candidate) =
        activated_instruction_text_from_context(candidate_event, &activation_context);
    if activation_context.is_empty() {
        return json!({
            "action": "ignore",
            "is_command": false,
            "confidence": 0.0,
            "wake_detected": false,
            "reason": "No wake-word activation was present for this speaker stream.",
            "candidate_text": raw_text,
            "source_event_ids": source_event_ids,
        });
    }

    let mut command_kind = command_kind_for_text(&instruction_text);
    let request = clean_question_text(&instruction_text);
    if command_kind.is_empty() {
        if request.split_whitespace().count() < 2 && !request.contains('?') {
            return json!({
                "action": "ignore",
                "is_command": false,
                "confidence": 0.0,
                "wake_detected": true,
                "wake_on_candidate": wake_on_candidate,
                "reason": "Wake word was detected, but no actionable request followed it.",
                "candidate_text": raw_text,
                "instruction_text": instruction_text,
                "source_event_ids": source_event_ids,
            });
        }
        command_kind = "agent_task".to_string();
    }

    let guild_id = first_non_empty([
        first_value_string(candidate_event, &["guild_id", "guildId"]),
        string_field(room_status, "guild_id"),
        string_field(room_status, "guildId"),
    ]);
    let channel_id = first_non_empty([
        first_value_string(candidate_event, &["voice_channel_id", "channelId"]),
        string_field(room_status, "voice_channel_id"),
        string_field(room_status, "channelId"),
    ]);
    let capture_run_id = first_non_empty([
        first_value_string(candidate_event, &["capture_run_id", "captureRunId"]),
        string_field(room_status, "capture_run_id"),
        string_field(room_status, "captureRunId"),
    ]);
    let voice_bot_id = first_non_empty([
        first_value_string(candidate_event, &["voice_bot_id", "botId"]),
        string_field(room_status, "voice_bot_id"),
        string_field(room_status, "botId"),
    ]);
    let speaker_id = event_speaker_id(candidate_event);
    let speaker_label = first_non_empty([
        first_value_string(candidate_event, &["speaker_label", "speakerLabel"]),
        speaker_id.clone(),
    ]);
    let arguments_value = command_arguments_for_text(&command_kind, &instruction_text, &raw_text);
    let dedupe = dedupe_hash(
        &guild_id,
        &channel_id,
        &source_event_ids.join("|"),
        &command_kind,
        &arguments_value,
    );
    let score = wake_score(candidate_event).unwrap_or(if wake_on_candidate { 1.0 } else { 0.75 });
    json!({
        "action": "dispatch_now",
        "is_command": true,
        "confidence": score,
        "wake_detected": true,
        "wake_on_candidate": wake_on_candidate,
        "command_kind": command_kind.clone(),
        "requested_by_user_id": speaker_id,
        "requested_by_speaker_label": speaker_label,
        "guild_id": guild_id,
        "voice_channel_id": channel_id,
        "capture_run_id": capture_run_id,
        "voice_bot_id": voice_bot_id,
        "arguments": arguments_value,
        "requires_confirmation": requires_confirmation(&command_kind),
        "reason": "Wake word model activated this speaker stream; command was built deterministically.",
        "acknowledgement_text": acknowledgement_text_for_command(&command_kind),
        "candidate_text": raw_text,
        "activated_text": instruction_text.clone(),
        "instruction_text": instruction_text,
        "source_event_ids": source_event_ids,
        "wake": candidate_event.get("wake").cloned().unwrap_or_else(|| json!({})),
        "dedupe_hash": dedupe,
    })
}

fn command_kind_for_text(text: &str) -> String {
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

fn clean_question_text(text: &str) -> String {
    let mut cleaned = WAKE_CLEANUP_RE.replace_all(text, " ").to_string();
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
    collapse_ws(&cleaned)
        .trim_matches(&[',', '.', ';', ':', '-', ' '][..])
        .to_string()
}

pub fn validate_voice_command_result(result: &Value) -> (bool, String) {
    if !result.is_object() {
        return (false, "voice command result is not an object".to_string());
    }
    let action = voice_command_action(result);
    if !COMMAND_ACTIONS.contains(&action.as_str()) {
        return (
            false,
            format!("unsupported action {:?}", result.get("action")),
        );
    }
    if action == "ignore" {
        return (false, non_empty(string_field(result, "reason"), action));
    }
    for field in ["guild_id", "voice_channel_id", "source_event_ids"] {
        if result.get(field).is_none_or(value_is_empty) {
            return (false, format!("voice command result missing {field}"));
        }
    }
    let command_kind = string_field(result, "command_kind");
    if command_kind.is_empty() || !COMMAND_KINDS.contains(&command_kind.as_str()) {
        return (
            false,
            format!("unsupported command_kind {:?}", result.get("command_kind")),
        );
    }
    (true, "ok".to_string())
}

fn activated_instruction_text_from_context(
    candidate_event: &Value,
    context: &[Value],
) -> (String, Vec<String>, bool) {
    if context.is_empty() {
        return (
            event_text(candidate_event),
            non_empty_vec(vec![event_id(candidate_event)]),
            false,
        );
    }
    let current_wake = wake_detected(candidate_event);
    let text = context
        .iter()
        .map(event_text)
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let ids = non_empty_vec(context.iter().map(event_id).collect());
    (collapse_ws(&text), ids, current_wake)
}

fn acknowledgement_text_for_command(command_kind: &str) -> String {
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

pub fn requires_confirmation(command_kind: &str) -> bool {
    command_kind == "forget_window"
}

fn dedupe_hash(
    guild_id: &str,
    voice_channel_id: &str,
    candidate_event_id: &str,
    command_kind: &str,
    arguments: &Value,
) -> String {
    let raw = serde_json::to_string(&json!({
        "arguments": arguments,
        "candidate_event_id": candidate_event_id,
        "command_kind": command_kind,
        "guild_id": guild_id,
        "voice_channel_id": voice_channel_id
    }))
    .unwrap_or_default();
    format!("{:x}", Sha256::digest(raw.as_bytes()))
}

fn command_arguments_for_text(command_kind: &str, instruction_text: &str, raw_text: &str) -> Value {
    let mut arguments = Map::new();
    if [
        "agent_task",
        "start_live_transcript",
        "start_draft_transcript",
        "materialize_transcript",
        "forget_window",
    ]
    .contains(&command_kind)
    {
        arguments.insert(
            "relative_start".to_string(),
            Value::String(normalize_relative_time(instruction_text, "-10m")),
        );
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
            Value::String(clean_question_text(instruction_text)),
        );
        arguments.insert("raw_text".to_string(), Value::String(raw_text.to_string()));
        arguments.insert(
            "activated_text".to_string(),
            Value::String(instruction_text.to_string()),
        );
        arguments.insert(
            "instruction_text".to_string(),
            Value::String(instruction_text.to_string()),
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
    .contains(&command_kind)
    {
        let target_room = target_room_for_text(instruction_text);
        if !target_room.is_empty() {
            arguments.insert("target_room".to_string(), Value::String(target_room));
        }
    }
    Value::Object(arguments)
}

fn normalize_relative_time(text: &str, default: &str) -> String {
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

fn target_room_for_text(text: &str) -> String {
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

fn collapse_ws(text: &str) -> String {
    Regex::new(r"\s+")
        .unwrap()
        .replace_all(text, " ")
        .trim()
        .to_string()
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

fn first_non_empty<const N: usize>(values: [String; N]) -> String {
    values
        .into_iter()
        .find(|value| !value.trim().is_empty())
        .map(|value| value.trim().to_string())
        .unwrap_or_default()
}

fn non_empty(value: String, default: String) -> String {
    if value.trim().is_empty() {
        default
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

fn value_is_empty(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(text) => text.is_empty(),
        Value::Array(values) => values.is_empty(),
        Value::Object(map) => map.is_empty(),
        _ => false,
    }
}

fn finite_number(value: Option<&Value>) -> Option<f64> {
    match value {
        Some(Value::Number(number)) => number.as_f64().filter(|number| number.is_finite()),
        Some(Value::String(text)) => text.parse::<f64>().ok().filter(|number| number.is_finite()),
        _ => None,
    }
}
