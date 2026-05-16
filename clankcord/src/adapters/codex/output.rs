use serde_json::{Value, json};

pub fn parse_codex_stdout_payload(stdout: &str) -> Value {
    let raw = stdout.trim();
    if raw.is_empty() {
        return json!({});
    }
    if let Ok(payload) = serde_json::from_str::<Value>(raw) {
        return payload;
    }
    let Some(start) = raw.find('{') else {
        return Value::String(raw.to_string());
    };
    let Some(end) = raw.rfind('}') else {
        return Value::String(raw.to_string());
    };
    if end <= start {
        return Value::String(raw.to_string());
    }
    serde_json::from_str::<Value>(&raw[start..=end])
        .unwrap_or_else(|_| Value::String(raw.to_string()))
}

pub fn codex_response_text(stdout: &str, last_message: &str) -> String {
    let last_message = last_message.trim();
    if !last_message.is_empty() {
        return last_message.to_string();
    }
    let values = json_values_from_stdout(stdout);
    if let Some(text) = values
        .iter()
        .filter_map(codex_assistant_message_text)
        .last()
    {
        return text;
    }
    let payload = parse_codex_stdout_payload(stdout);
    if let Some(payloads) = payload.get("payloads").and_then(Value::as_array) {
        let parts = payloads
            .iter()
            .filter_map(|entry| entry.get("text").and_then(Value::as_str))
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        if !parts.is_empty() {
            return parts.join("\n\n").trim().to_string();
        }
    }
    if let Some(meta) = payload.get("meta").and_then(Value::as_object) {
        for key in ["finalAssistantVisibleText", "finalAssistantRawText"] {
            if let Some(text) = meta.get(key).and_then(Value::as_str).map(str::trim)
                && !text.is_empty()
            {
                return text.to_string();
            }
        }
    }
    if let Some(text) = payload.as_str().map(str::trim)
        && !text.is_empty()
    {
        return text.to_string();
    }
    stdout.trim().to_string()
}

fn codex_assistant_message_text(value: &Value) -> Option<String> {
    match value.get("type").and_then(Value::as_str).unwrap_or("") {
        "item.completed" | "item.started" => current_item_message_text(value),
        "response_item" => response_item_message_text(value),
        "event_msg" => event_msg_message_text(value),
        _ => None,
    }
}

fn current_item_message_text(value: &Value) -> Option<String> {
    let item = value.get("item")?;
    match item.get("type").and_then(Value::as_str).unwrap_or("") {
        "agent_message" => string_field(item, "text"),
        "message" if item.get("role").and_then(Value::as_str) == Some("assistant") => {
            current_message_item_text(item)
        }
        _ => None,
    }
}

fn current_message_item_text(item: &Value) -> Option<String> {
    if let Some(text) = string_field(item, "text") {
        return Some(text);
    }
    let text = item
        .get("content")
        .and_then(Value::as_array)?
        .iter()
        .filter_map(|part| {
            part.get("text")
                .or_else(|| part.get("content"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
        })
        .collect::<Vec<_>>()
        .join("\n");
    (!text.is_empty()).then_some(text)
}

fn response_item_message_text(value: &Value) -> Option<String> {
    let payload = value.get("payload")?;
    if payload.get("type").and_then(Value::as_str) != Some("message")
        || payload.get("role").and_then(Value::as_str) != Some("assistant")
    {
        return None;
    }
    let text = payload
        .get("content")
        .and_then(Value::as_array)?
        .iter()
        .filter_map(|part| part.get("text").and_then(Value::as_str).map(str::trim))
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    (!text.is_empty()).then_some(text)
}

fn event_msg_message_text(value: &Value) -> Option<String> {
    let payload = value.get("payload")?;
    if payload.get("type").and_then(Value::as_str) != Some("agent_message") {
        return None;
    }
    string_field(payload, "message")
}

pub fn extract_codex_session_id(stdout: &str) -> String {
    find_string_field(
        &json_values_from_stdout(stdout),
        &[
            "session_id",
            "sessionId",
            "conversation_id",
            "conversationId",
            "thread_id",
            "threadId",
        ],
    )
}

pub fn extract_codex_model(stdout: &str) -> String {
    find_string_field(
        &json_values_from_stdout(stdout),
        &["model", "model_id", "modelId", "model_slug", "modelSlug"],
    )
}

pub fn extract_codex_usage(stdout: &str) -> Value {
    let latest = json_values_from_stdout(stdout)
        .into_iter()
        .filter_map(codex_usage_payload)
        .last()
        .unwrap_or_else(|| json!({}));
    if latest.is_object() {
        latest
    } else {
        json!({})
    }
}

pub fn parse_codex_jsonl(stdout: &str) -> Vec<Value> {
    json_values_from_stdout(stdout)
}

pub fn codex_usage_payload(value: Value) -> Option<Value> {
    legacy_token_count_payload(&value).or_else(|| turn_completed_usage_payload(&value))
}

fn json_values_from_stdout(stdout: &str) -> Vec<Value> {
    let raw = stdout.trim();
    if raw.is_empty() {
        return Vec::new();
    }
    if let Ok(value) = serde_json::from_str::<Value>(raw) {
        return vec![value];
    }
    raw.lines()
        .filter_map(|line| serde_json::from_str::<Value>(line.trim()).ok())
        .collect()
}

fn legacy_token_count_payload(value: &Value) -> Option<Value> {
    let object = value.as_object()?;
    if object.get("type").and_then(Value::as_str) != Some("event_msg") {
        return None;
    }
    let payload = object.get("payload")?.as_object()?;
    if payload.get("type").and_then(Value::as_str) != Some("token_count") {
        return None;
    }
    Some(Value::Object(payload.clone()))
}

fn turn_completed_usage_payload(value: &Value) -> Option<Value> {
    let object = value.as_object()?;
    if object.get("type").and_then(Value::as_str) != Some("turn.completed") {
        return None;
    }
    let usage = object.get("usage")?.as_object()?;
    let usage = Value::Object(usage.clone());
    Some(json!({
        "last_token_usage": usage,
        "total_token_usage": usage,
        "raw_usage": usage,
    }))
}

fn find_string_field(values: &[Value], keys: &[&str]) -> String {
    values
        .iter()
        .find_map(|value| find_string_field_in_value(value, keys))
        .unwrap_or_default()
}

fn find_string_field_in_value(value: &Value, keys: &[&str]) -> Option<String> {
    match value {
        Value::Object(object) => {
            for key in keys {
                if let Some(text) = object.get(*key).and_then(Value::as_str).map(str::trim)
                    && !text.is_empty()
                {
                    return Some(text.to_string());
                }
            }
            object
                .values()
                .find_map(|child| find_string_field_in_value(child, keys))
        }
        Value::Array(values) => values
            .iter()
            .find_map(|child| find_string_field_in_value(child, keys)),
        _ => None,
    }
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(ToOwned::to_owned)
}
