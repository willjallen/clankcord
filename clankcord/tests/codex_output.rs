use clankcord::adapters::codex::{codex_response_text, extract_codex_usage};
use serde_json::Value;

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

#[test]
fn extracts_response_text_from_current_codex_jsonl() {
    let stdout = r#"
{"type":"thread.started","thread_id":"thread-1"}
{"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"I am checking the room."}}
{"type":"item.completed","item":{"id":"item_1","type":"agent_message","text":"NO_RESPONSE_NEEDED"}}
{"type":"turn.completed","usage":{"input_tokens":10,"output_tokens":2}}
"#;

    assert_eq!(codex_response_text(stdout, ""), "NO_RESPONSE_NEEDED");
}
