use std::path::PathBuf;

use clankcord::runtime::domain::interactions::{
    AgentTaskPromptContext, AgentThreadTitlePromptContext, agent_invocation_infrastructure_failure,
    agent_invocation_warning_event_kind, build_agent_task_message_from_template_dir,
    build_agent_thread_title_prompt_from_template_dir, sanitize_agent_thread_title,
};

#[test]
fn agent_task_prompt_is_compact_and_packet_free() {
    let prompt = build_agent_task_message_from_template_dir(
        &AgentTaskPromptContext {
            job_id: "job_1".to_string(),
            agent_session_id: "ags_1".to_string(),
            resumed_from_agent_session_id: String::new(),
            guild_id: "guild".to_string(),
            voice_channel_id: "voice".to_string(),
            requested_by_user_id: "user".to_string(),
            requested_by: "Will".to_string(),
            request: "summarize the floating point discussion".to_string(),
            workdir: "/clankcord/state/agent-workspaces/task/guild/voice".to_string(),
            previous_context: vec![
                "[2026-05-15T00:00:00Z] Will (user): we were talking about floats".to_string(),
            ],
            question: vec![
                "[2026-05-15T00:01:00Z] Will (user): hey clanky summarize this".to_string(),
            ],
        },
        true,
        &prompt_dir(),
    )
    .expect("build agent task prompt");

    assert!(prompt.contains("===== PREVIOUS CONTEXT ====="));
    assert!(prompt.contains("===== QUESTION / ACTIVATION ====="));
    assert!(prompt.contains("CLANKCORD_AGENT_WORKDIR"));
    assert!(prompt.contains("CLANKCORD_AGENT_JOB_ID"));
    assert!(prompt.contains("NO_RESPONSE_NEEDED"));
    assert!(prompt.contains("clankcord --help"));
    assert!(prompt.contains("command-group `--help`"));
    assert!(prompt.contains("clankcord responses dm"));
    assert!(prompt.contains("single-quoted heredoc"));
    assert!(prompt.contains("treat the request and the answer as private"));
    assert!(prompt.contains("speech-to-text transcription of live voice"));
    assert!(prompt.contains("interpret it charitably"));
    assert!(prompt.contains(
        "do not publish the topic, answer, summary, result, or confirmation to a public channel"
    ));
    assert!(prompt.contains("not the runtime HTTP endpoints"));
    assert!(!prompt.contains("JOB_PACKET_JSON"));
    assert!(!prompt.contains("packet.json"));
    assert!(!prompt.contains("\"schema\""));
    assert!(!prompt.contains("\"tools\""));
    assert!(!prompt.contains("\"manuals\""));
    assert!(!prompt.contains("\"policy\""));
}

#[test]
fn agent_task_prompt_can_render_from_custom_template_dir() {
    let tempdir = tempfile::tempdir().expect("create prompt template dir");
    std::fs::write(tempdir.path().join("master.md"), "CUSTOM MASTER")
        .expect("write master prompt template");
    std::fs::write(
        tempdir.path().join("agent-task.md"),
        "CUSTOM TASK\njob={{job_id}}\nrequest={{request}}\n{{question}}",
    )
    .expect("write agent task prompt template");

    let prompt = build_agent_task_message_from_template_dir(
        &AgentTaskPromptContext {
            job_id: "job_1".to_string(),
            agent_session_id: "ags_1".to_string(),
            resumed_from_agent_session_id: String::new(),
            guild_id: "guild".to_string(),
            voice_channel_id: "voice".to_string(),
            requested_by_user_id: "user".to_string(),
            requested_by: "Will".to_string(),
            request: "summarize the floating point discussion".to_string(),
            workdir: "/clankcord/state/agent-workspaces/task/guild/voice".to_string(),
            previous_context: vec![],
            question: vec![
                "[2026-05-15T00:01:00Z] Will (user): hey clanky summarize this".to_string(),
            ],
        },
        true,
        tempdir.path(),
    )
    .expect("render custom prompt templates");

    assert!(prompt.contains("CUSTOM MASTER"));
    assert!(prompt.contains("CUSTOM TASK"));
    assert!(prompt.contains("job=job_1"));
    assert!(prompt.contains("request=summarize the floating point discussion"));
    assert!(prompt.contains("hey clanky summarize this"));
}

#[test]
fn agent_thread_title_prompt_uses_its_own_template() {
    let prompt = build_agent_thread_title_prompt_from_template_dir(
        &AgentThreadTitlePromptContext {
            agent_session_id: "ags_1".to_string(),
            current_thread_title: "agent code ags_1".to_string(),
            voice_channel_name: "Code Lounge".to_string(),
            response_count: 2,
            responses: vec![
                "response 1:\nrequest: explain grpc\nresponse: grpc is a request protocol"
                    .to_string(),
                "response 2:\nrequest: compare wasm\nresponse: wasm runs portable modules"
                    .to_string(),
            ],
        },
        &prompt_dir(),
    )
    .expect("render agent thread title prompt");

    assert!(prompt.contains("THREAD_TITLE_TASK"));
    assert!(prompt.contains("current_thread_title: agent code ags_1"));
    assert!(prompt.contains("voice_channel_name: Code Lounge"));
    assert!(prompt.contains("response_count: 2"));
    assert!(prompt.contains("compare wasm"));
    assert!(!prompt.contains("===== PREVIOUS CONTEXT ====="));
    assert!(!prompt.contains("NO_RESPONSE_NEEDED"));
}

#[test]
fn agent_thread_title_sanitizer_keeps_one_compact_title() {
    let title = sanitize_agent_thread_title("  `gRPC and WASM comparison`  \nextra")
        .expect("sanitize thread title");

    assert_eq!(title, "gRPC and WASM comparison");
    assert!(sanitize_agent_thread_title("").is_err());
    assert_eq!(
        sanitize_agent_thread_title(&"a".repeat(120))
            .expect("truncate long title")
            .chars()
            .count(),
        80
    );
}

#[test]
fn codex_auth_failure_is_infrastructure_failure_but_mcp_token_warning_is_not() {
    assert!(agent_invocation_infrastructure_failure(
        "Auth(TokenRefreshFailed(\"invalid_grant\"))"
    ));
    assert!(!agent_invocation_infrastructure_failure(
        "MCP server docs failed: Auth(TokenRefreshFailed(\"invalid_grant\"))"
    ));
    assert_eq!(
        agent_invocation_warning_event_kind(
            "MCP server docs failed: Auth(TokenRefreshFailed(\"invalid_grant\"))"
        ),
        Some("agent_mcp_token_warning")
    );
    assert!(!agent_invocation_infrastructure_failure(
        "codex command timed out after 240 seconds"
    ));
}

fn prompt_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("res/prompts")
}
