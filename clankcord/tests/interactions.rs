use std::path::PathBuf;

use clankcord::runtime::domain::interactions::{
    AgentPromptRequestOrigin, AgentTaskPromptContext, build_agent_task_message_from_template_dir,
};
use clankcord::runtime::{AgentSessionRouteKind, TextTargetKind};

#[test]
fn agent_task_message_uses_compact_invocation_context() {
    let context = prompt_context();
    let message = build_agent_task_message_from_template_dir(&context, true, &prompt_dir())
        .expect("build agent task message");

    assert!(message.contains("You are Clanky, a helpful and rigorous Discord server assistant"));
    assert!(message.contains("clankcord --help"));
    assert!(message.contains("command-group `--help`"));
    assert!(message.contains("clankcord responses send"));
    assert!(message.contains("clankcord responses dm"));
    assert!(message.contains("single-quoted heredoc"));
    assert!(message.contains("RESPONSE_SUBMITTED"));
    assert!(message.contains("NO_RESPONSE_NEEDED"));
    assert!(message.contains("Codex final text is a control signal"));
    assert!(message.contains("INVOCATION_RESPONSE_CONTRACT"));
    assert!(message.contains("After successful private delivery"));
    assert!(message.contains("You may search the web"));
    assert!(message.contains("clankcord coding spec"));
    assert!(message.contains("clankcord responses send --attachment"));
    assert!(message.contains("Do not be sycophantic"));
    assert!(message.contains("VOICE_REQUEST_CONTEXT"));
    assert!(message.contains("begin with one short sentence summarizing what you understood"));
    assert!(
        message
            .contains("CLANKCORD_AGENT_WORKDIR=/clankcord/state/agent-workspaces/task/guild/code")
    );
    assert!(message.contains("===== RECENT SCOPE EVENTS ====="));
    assert!(message.contains("===== CURRENT REQUEST EVENTS ====="));
    assert!(message.contains("vince: prior context"));
    assert!(message.contains("will: summarize this"));
    assert!(!message.contains("JOB_PACKET_JSON"));
    assert!(!message.contains("\"schema\""));
    assert!(!message.contains("\"tools\""));
    assert!(!message.contains("\"manuals\""));
    assert!(!message.contains("\"policy\""));
}

#[test]
fn resumed_agent_task_message_omits_large_session_instructions() {
    let context = prompt_context();
    let message = build_agent_task_message_from_template_dir(&context, false, &prompt_dir())
        .expect("build resumed agent task message");

    assert!(message.contains("JOB:"));
    assert!(!message.contains("SESSION_INSTRUCTIONS:"));
    assert!(message.contains("INVOCATION_RESPONSE_CONTRACT"));
    assert!(message.contains("After successful private delivery"));
    assert!(message.contains("VOICE_REQUEST_CONTEXT"));
    assert!(message.contains("===== CURRENT REQUEST EVENTS ====="));
    assert!(!message.contains("JOB_PACKET_JSON"));
}

fn prompt_context() -> AgentTaskPromptContext {
    AgentTaskPromptContext {
        job_id: "job_test".to_string(),
        agent_session_id: "ags_test".to_string(),
        resumed_from_agent_session_id: String::new(),
        route_kind: AgentSessionRouteKind::Voice,
        request_origin: AgentPromptRequestOrigin::Voice,
        response_surface: TextTargetKind::Channel,
        guild_id: "guild".to_string(),
        scope_id: "code".to_string(),
        requested_by_user_id: "user-1".to_string(),
        requested_by: "will".to_string(),
        request: "summarize this".to_string(),
        workdir: "/clankcord/state/agent-workspaces/task/guild/code".to_string(),
        recent_scope_events: vec!["[2026-05-15T00:00:00Z] vince: prior context".to_string()],
        source_request_events: vec!["[2026-05-15T00:01:00Z] will: summarize this".to_string()],
    }
}

fn prompt_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("res/prompts")
}
