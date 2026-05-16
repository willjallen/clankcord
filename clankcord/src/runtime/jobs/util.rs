use serde_json::{Map, Number, Value};

pub(crate) fn insert_optional_string(
    object: &mut Map<String, Value>,
    key: &str,
    value: &Option<String>,
) {
    if let Some(value) = value.as_ref().filter(|value| !value.trim().is_empty()) {
        object.insert(key.to_string(), Value::String(value.clone()));
    }
}

pub(crate) fn insert_non_empty(object: &mut Map<String, Value>, key: &str, value: &str) {
    if !value.trim().is_empty() {
        object.insert(key.to_string(), Value::String(value.to_string()));
    }
}

pub(crate) fn insert_i64_if_nonzero(object: &mut Map<String, Value>, key: &str, value: i64) {
    if value != 0 {
        object.insert(key.to_string(), Value::Number(Number::from(value)));
    }
}

pub(crate) fn truthy(value: Option<&Value>, default: bool) -> bool {
    match value {
        Some(Value::Bool(value)) => *value,
        Some(Value::Number(value)) => value.as_i64().unwrap_or(0) != 0,
        Some(Value::String(value)) => match value.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => true,
            "0" | "false" | "no" | "off" => false,
            "" => default,
            _ => default,
        },
        _ => default,
    }
}
