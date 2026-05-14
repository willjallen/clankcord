use serde_json::{Value, json};

pub(crate) fn parse_codex_stdout_payload(stdout: &str) -> Value {
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

pub(crate) fn codex_response_text(stdout: &str, last_message: &str) -> String {
    let last_message = last_message.trim();
    if !last_message.is_empty() {
        return last_message.to_string();
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

pub(crate) fn extract_codex_session_id(stdout: &str) -> String {
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

pub(crate) fn extract_codex_model(stdout: &str) -> String {
    find_string_field(
        &json_values_from_stdout(stdout),
        &["model", "model_id", "modelId", "model_slug", "modelSlug"],
    )
}

pub(crate) fn extract_codex_usage(stdout: &str) -> Value {
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

pub(crate) fn parse_codex_jsonl(stdout: &str) -> Vec<Value> {
    json_values_from_stdout(stdout)
}

pub(crate) fn codex_usage_payload(value: Value) -> Option<Value> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_usage_from_current_codex_turn_completed_event() {
        let stdout = r#"
{"type":"thread.started","thread_id":"thread-1"}
{"type":"turn.completed","usage":{"input_tokens":60770,"cached_input_tokens":36096,"output_tokens":2492,"reasoning_output_tokens":1876}}
"#;

        let usage = extract_codex_usage(stdout);
        assert_eq!(
            usage
                .get("total_token_usage")
                .and_then(|value| value.get("input_tokens"))
                .and_then(Value::as_i64),
            Some(60770)
        );
        assert_eq!(
            usage
                .get("last_token_usage")
                .and_then(|value| value.get("cached_input_tokens"))
                .and_then(Value::as_i64),
            Some(36096)
        );
    }

    #[test]
    fn keeps_extracting_legacy_token_count_payloads() {
        let stdout = r#"
{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":10},"model_context_window":100}}}
"#;

        let usage = extract_codex_usage(stdout);
        assert_eq!(
            usage
                .get("info")
                .and_then(|value| value.get("total_token_usage"))
                .and_then(|value| value.get("input_tokens"))
                .and_then(Value::as_i64),
            Some(10)
        );
    }
}
