use std::env;

use serde_json::Value;

use crate::Result;
use crate::config::string_value;
use crate::errors::discord_tool_error;
use crate::runtime::Job;
use crate::runtime::timeline::parse_duration;

pub fn log(message: &str) {
    eprintln!("[clawcord-voice] {message}");
}

pub fn parse_duration_seconds(value: Option<&Value>, fallback: i64) -> i64 {
    let raw = string_value(value).trim().to_string();
    if raw.is_empty() {
        return fallback;
    }
    if let Some(duration) = parse_duration(&raw) {
        return duration.num_seconds().abs();
    }
    raw.parse::<i64>()
        .ok()
        .map(|value| value.max(0))
        .unwrap_or(fallback)
}

pub fn duration_to_seconds(raw: &str) -> i64 {
    let value = raw.trim().to_lowercase();
    if value.ends_with("ms") {
        return value[..value.len() - 2]
            .parse::<f64>()
            .map(|number| (number / 1000.0).max(0.0) as i64)
            .unwrap_or(0);
    }
    let (number, multiplier) = if let Some(stripped) = value.strip_suffix('s') {
        (stripped, 1.0)
    } else if let Some(stripped) = value.strip_suffix('m') {
        (stripped, 60.0)
    } else if let Some(stripped) = value.strip_suffix('h') {
        (stripped, 3600.0)
    } else if let Some(stripped) = value.strip_suffix('d') {
        (stripped, 86400.0)
    } else {
        (value.as_str(), 1.0)
    };
    number
        .parse::<f64>()
        .map(|number| (number * multiplier).max(0.0) as i64)
        .unwrap_or(0)
}

pub fn voice_worker_agent_timeout_seconds() -> u64 {
    env::var("VOICE_POOL_WORKER_AGENT_TIMEOUT_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(240)
        .max(30)
}

pub fn first_non_empty<const N: usize>(values: [String; N]) -> String {
    values
        .into_iter()
        .find(|value| !value.trim().is_empty())
        .map(|value| value.trim().to_string())
        .unwrap_or_default()
}

pub(crate) fn update_object_fields<const N: usize>(
    value: &mut Value,
    fields: [(&str, Value); N],
) -> Result<()> {
    let map = value
        .as_object_mut()
        .ok_or_else(|| discord_tool_error("payload is not an object"))?;
    for (key, field_value) in fields {
        map.insert(key.to_string(), field_value);
    }
    Ok(())
}

pub(crate) fn set_if_blank(target: &mut Value, key: &str, value: Value) {
    let Some(map) = target.as_object_mut() else {
        return;
    };
    if map.get(key).is_none_or(value_is_blank) {
        map.insert(key.to_string(), value);
    }
}

fn value_is_blank(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(text) => text.trim().is_empty(),
        Value::Array(items) => items.is_empty(),
        Value::Object(map) => map.is_empty(),
        _ => false,
    }
}

pub(crate) fn job_cancel_requested(job: &Job) -> bool {
    job.cancel_requested()
}

pub(crate) fn require_confirmation_actor(job: &Job, actor_user_id: &str) -> Result<()> {
    let expected = job.requested_by_user_id.trim();
    if !expected.is_empty() && actor_user_id.trim() != expected {
        return Err(discord_tool_error(
            "only the requesting user can approve or cancel this confirmation",
        ));
    }
    Ok(())
}

pub(crate) fn preview(value: &str, limit: usize) -> String {
    value.trim().chars().take(limit).collect()
}

pub(crate) fn preview_tail(value: &str, limit: usize) -> String {
    let trimmed = value.trim();
    let count = trimmed.chars().count();
    if count <= limit {
        return trimmed.to_string();
    }
    trimmed.chars().skip(count - limit).collect()
}
