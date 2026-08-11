//! Interprets a persisted codex CLI JSONL trace into the structured view
//! the operator surfaces render: session identity, message/tool timelines,
//! and token/rate-limit accounting. All knowledge of codex event shapes
//! stays here.

use serde_json::{Map, Value, json};

use crate::adapters::codex::output::{codex_usage_payload, parse_codex_jsonl};
use crate::util::{non_empty, string_field};

pub fn parse_codex_trace(raw: &str) -> Value {
    let events = parse_codex_jsonl(raw);
    let mut session_id = String::new();
    let mut model = String::new();
    let mut cli_version = String::new();
    let mut messages = Vec::new();
    let mut tool_calls = Vec::new();
    let mut timeline = Vec::new();
    let mut token_usage = Value::Object(Map::new());
    let mut rate_limits = Value::Null;
    let mut context_window = 0_i64;
    for event in &events {
        if let Some(usage) = codex_usage_payload(event.clone()) {
            token_usage = usage_payload_info(&usage);
            if let Some(reported) = usage.get("rate_limits") {
                rate_limits = reported.clone();
            }
            context_window = context_window_from_usage(&token_usage).unwrap_or(context_window);
        }
        match event.get("type").and_then(Value::as_str).unwrap_or("") {
            "session_meta" => {
                let payload = event.get("payload").unwrap_or(&Value::Null);
                session_id = non_empty(session_id, string_field(payload, "id"));
                model = non_empty(model, string_field(payload, "model"));
                cli_version = non_empty(cli_version, string_field(payload, "cli_version"));
            }
            "thread.started" => {
                session_id = non_empty(session_id, string_field(event, "thread_id"));
            }
            "item.started" | "item.completed" => {
                collect_current_item(event, &mut messages, &mut tool_calls, &mut timeline);
            }
            "response_item" => {
                collect_response_item(event, &mut messages, &mut tool_calls, &mut timeline)
            }
            "event_msg" => {
                let payload = event.get("payload").unwrap_or(&Value::Null);
                if payload.get("type").and_then(Value::as_str) == Some("agent_message") {
                    push_message(
                        &mut messages,
                        &mut timeline,
                        json!({
                            "role": "assistant",
                            "phase": string_field(payload, "phase"),
                            "text": string_field(payload, "message"),
                            "timestamp": string_field(event, "timestamp"),
                        }),
                    );
                }
            }
            _ => {}
        }
    }
    let total_input = token_usage_input_tokens(&token_usage);
    let context_used_percent = if context_window > 0 {
        (total_input as f64 / context_window as f64) * 100.0
    } else {
        0.0
    };
    json!({
        "sessionId": session_id,
        "model": model,
        "cliVersion": cli_version,
        "eventCount": events.len(),
        "messages": messages,
        "toolCalls": tool_calls,
        "timeline": timeline,
        "tokenUsage": token_usage,
        "rateLimits": rate_limits,
        "contextUsedTokens": total_input,
        "modelContextWindow": context_window,
        "contextUsedPercent": context_used_percent,
    })
}

pub fn usage_payload_info(usage: &Value) -> Value {
    usage.get("info").cloned().unwrap_or_else(|| usage.clone())
}

fn context_window_from_usage(usage: &Value) -> Option<i64> {
    usage
        .get("model_context_window")
        .and_then(Value::as_i64)
        .or_else(|| usage.get("modelContextWindow").and_then(Value::as_i64))
}

fn token_usage_input_tokens(usage: &Value) -> i64 {
    usage
        .get("total_token_usage")
        .and_then(|value| value.get("input_tokens"))
        .and_then(Value::as_i64)
        .or_else(|| {
            usage
                .get("last_token_usage")
                .and_then(|value| value.get("input_tokens"))
                .and_then(Value::as_i64)
        })
        .unwrap_or(0)
}

fn collect_current_item(
    event: &Value,
    messages: &mut Vec<Value>,
    tool_calls: &mut Vec<Value>,
    timeline: &mut Vec<Value>,
) {
    let item = event.get("item").unwrap_or(&Value::Null);
    let item_type = item.get("type").and_then(Value::as_str).unwrap_or("");
    let event_type = event.get("type").and_then(Value::as_str).unwrap_or("");
    let status = if event_type.ends_with(".started") {
        "started"
    } else {
        "completed"
    };
    match item_type {
        "agent_message" => {
            let text = string_field(item, "text");
            if !text.trim().is_empty() {
                push_message(
                    messages,
                    timeline,
                    json!({
                        "role": "assistant",
                        "phase": status,
                        "text": text,
                        "timestamp": string_field(event, "timestamp"),
                    }),
                );
            }
        }
        "message" => {
            let text = text_from_current_message_item(item);
            if !text.trim().is_empty() {
                push_message(
                    messages,
                    timeline,
                    json!({
                        "role": string_field(item, "role"),
                        "phase": status,
                        "text": text,
                        "timestamp": string_field(event, "timestamp"),
                    }),
                );
            }
        }
        "command_execution" => push_tool_call(
            tool_calls,
            timeline,
            json!({
                "name": "command_execution",
                "arguments": string_field(item, "command"),
                "output": item.get("aggregated_output").cloned().unwrap_or_else(|| json!("")),
                "status": item.get("status").and_then(Value::as_str).unwrap_or(status),
                "exitCode": item.get("exit_code").cloned().unwrap_or(Value::Null),
                "callId": string_field(item, "id"),
                "timestamp": string_field(event, "timestamp"),
            }),
        ),
        _ if item_type.contains("tool") || item_type.contains("function") => {
            push_tool_call(
                tool_calls,
                timeline,
                json!({
                    "name": non_empty(string_field(item, "name"), item_type.to_string()),
                    "arguments": item.get("arguments")
                        .or_else(|| item.get("input"))
                        .cloned()
                        .unwrap_or_else(|| json!("")),
                    "output": item.get("output")
                        .or_else(|| item.get("result"))
                        .cloned()
                        .unwrap_or_else(|| json!("")),
                    "status": item.get("status").and_then(Value::as_str).unwrap_or(status),
                    "callId": string_field(item, "id"),
                    "timestamp": string_field(event, "timestamp"),
                }),
            );
        }
        _ => {}
    }
}

fn text_from_current_message_item(item: &Value) -> String {
    if let Some(text) = item.get("text").and_then(Value::as_str) {
        return text.to_string();
    }
    item.get("content")
        .and_then(Value::as_array)
        .map(|content| {
            content
                .iter()
                .filter_map(|part| {
                    part.get("text")
                        .or_else(|| part.get("content"))
                        .and_then(Value::as_str)
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

fn collect_response_item(
    event: &Value,
    messages: &mut Vec<Value>,
    tool_calls: &mut Vec<Value>,
    timeline: &mut Vec<Value>,
) {
    let payload = event.get("payload").unwrap_or(&Value::Null);
    match payload.get("type").and_then(Value::as_str).unwrap_or("") {
        "message" => {
            let text = payload
                .get("content")
                .and_then(Value::as_array)
                .map(|content| {
                    content
                        .iter()
                        .filter_map(|part| part.get("text").and_then(Value::as_str))
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();
            if !text.trim().is_empty() {
                push_message(
                    messages,
                    timeline,
                    json!({
                        "role": string_field(payload, "role"),
                        "phase": string_field(payload, "phase"),
                        "text": text,
                        "timestamp": string_field(event, "timestamp"),
                    }),
                );
            }
        }
        "function_call" => push_tool_call(
            tool_calls,
            timeline,
            json!({
                "name": string_field(payload, "name"),
                "arguments": payload.get("arguments").cloned().unwrap_or_else(|| json!("")),
                "callId": string_field(payload, "call_id"),
                "timestamp": string_field(event, "timestamp"),
            }),
        ),
        "function_call_output" => push_tool_call(
            tool_calls,
            timeline,
            json!({
                "output": payload.get("output").cloned().unwrap_or_else(|| json!("")),
                "callId": string_field(payload, "call_id"),
                "timestamp": string_field(event, "timestamp"),
            }),
        ),
        _ => {}
    }
}

fn push_message(messages: &mut Vec<Value>, timeline: &mut Vec<Value>, mut message: Value) {
    if let Value::Object(object) = &mut message {
        object.insert("kind".to_string(), json!("message"));
    }
    messages.push(message.clone());
    timeline.push(message);
}

fn push_tool_call(tool_calls: &mut Vec<Value>, timeline: &mut Vec<Value>, mut tool_call: Value) {
    if let Value::Object(object) = &mut tool_call {
        object.insert("kind".to_string(), json!("tool_call"));
    }
    tool_calls.push(tool_call.clone());
    timeline.push(tool_call);
}
