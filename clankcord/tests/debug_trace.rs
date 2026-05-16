use clankcord::runtime::views::parse_codex_trace;
use serde_json::Value;

#[test]
fn current_codex_jsonl_populates_agent_debug_trace() {
    let raw = r#"
{"type":"thread.started","thread_id":"019e270d-878f-70c0-855e-456d2225d85c"}
{"type":"turn.started"}
{"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"I will answer and publish through Clankcord."}}
{"type":"item.started","item":{"id":"item_1","type":"command_execution","command":"clankcord responses submit --job job_1 <<'EOF'\nAnswer text\nEOF","aggregated_output":"","exit_code":null,"status":"in_progress"}}
{"type":"item.completed","item":{"id":"item_1","type":"command_execution","command":"clankcord responses submit --job job_1 <<'EOF'\nAnswer text\nEOF","aggregated_output":"{\"job_ids\":[\"job_response\"]}\n","exit_code":0,"status":"completed"}}
{"type":"item.completed","item":{"id":"item_2","type":"agent_message","text":"RESPONSE_SUBMITTED"}}
{"type":"turn.completed","usage":{"input_tokens":60770,"cached_input_tokens":36096,"output_tokens":2492,"reasoning_output_tokens":1876}}
"#;

    let trace = parse_codex_trace(raw);

    assert_eq!(
        trace.get("sessionId").and_then(Value::as_str),
        Some("019e270d-878f-70c0-855e-456d2225d85c")
    );
    assert_eq!(trace.get("eventCount").and_then(Value::as_u64), Some(7));
    assert_eq!(
        trace
            .get("messages")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(2)
    );
    assert_eq!(
        trace
            .get("toolCalls")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(2)
    );
    assert_eq!(
        trace.get("contextUsedTokens").and_then(Value::as_i64),
        Some(60770)
    );
    assert_eq!(
        trace
            .get("tokenUsage")
            .and_then(|value| value.get("total_token_usage"))
            .and_then(|value| value.get("cached_input_tokens"))
            .and_then(Value::as_i64),
        Some(36096)
    );
}
