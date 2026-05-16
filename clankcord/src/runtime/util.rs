use serde_json::Value;

use crate::Result;
use crate::config::string_value;
use crate::runtime::{Job, JobKind};

pub fn log(message: &str) {
    eprintln!("[clankcord-voice] {message}");
}

pub(crate) fn first_non_empty<const N: usize>(values: [String; N]) -> String {
    values
        .into_iter()
        .find(|value| !value.trim().is_empty())
        .map(|value| value.trim().to_string())
        .unwrap_or_default()
}

pub(crate) fn string_field(value: &Value, key: &str) -> String {
    string_value(value.get(key))
}

pub(crate) fn first_value_string(value: &Value, keys: &[&str]) -> String {
    keys.iter()
        .map(|key| string_field(value, key))
        .find(|value| !value.is_empty())
        .unwrap_or_default()
}

pub(crate) fn string_array(value: &Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|value| match value {
            Value::String(text) => Some(text.trim().to_string()),
            Value::Number(number) => Some(number.to_string()),
            _ => None,
        })
        .filter(|value| !value.is_empty())
        .collect()
}

pub(crate) fn finite_number(value: Option<&Value>) -> Option<f64> {
    match value {
        Some(Value::Number(number)) => number.as_f64().filter(|number| number.is_finite()),
        Some(Value::String(text)) => text.parse::<f64>().ok().filter(|number| number.is_finite()),
        _ => None,
    }
}

pub(crate) fn number_or_null(value: Option<f64>) -> Value {
    value
        .and_then(serde_json::Number::from_f64)
        .map(Value::Number)
        .unwrap_or(Value::Null)
}

pub(crate) fn non_empty(value: String, default: String) -> String {
    if value.trim().is_empty() {
        default
    } else {
        value.trim().to_string()
    }
}

pub(crate) fn preview(value: &str, limit: usize) -> String {
    value.trim().chars().take(limit).collect()
}

pub(crate) fn single_child_of_kind(children: &[Job], kind: JobKind) -> Result<&Job> {
    let matches = children
        .iter()
        .filter(|child| child.kind == kind)
        .collect::<Vec<_>>();
    if matches.len() != 1 {
        anyhow::bail!("expected exactly one {kind} child, found {}", matches.len());
    }
    Ok(matches[0])
}
