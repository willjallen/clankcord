use clankcord::runtime::domain::interactions::{
    AgentTaskPromptContext, build_agent_task_message, build_agent_task_message_for_session,
};

#[test]
fn agent_task_message_uses_compact_transcript_context() {
    let context = prompt_context();
    let message = build_agent_task_message(&context);

    assert!(message.contains("You are Clanky, a helpful and rigorous Discord server assistant"));
    assert!(message.contains("clankcord --help"));
    assert!(message.contains("clankcord responses --help"));
    assert!(message.contains("clankcord responses send"));
    assert!(message.contains("clankcord responses dm"));
    assert!(message.contains("RESPONSE_SUBMITTED"));
    assert!(message.contains("NO_RESPONSE_NEEDED"));
    assert!(message.contains("Final text is not a publication path"));
    assert!(message.contains("You may search the web"));
    assert!(message.contains("Do not be sycophantic"));
    assert!(
        message
            .contains("CLANKCORD_AGENT_WORKDIR=/clankcord/state/agent-workspaces/task/guild/code")
    );
    assert!(message.contains("===== PREVIOUS CONTEXT ====="));
    assert!(message.contains("===== QUESTION / ACTIVATION ====="));
    assert!(message.contains("vince (user-2): prior context"));
    assert!(message.contains("will (user-1): summarize this"));
    assert!(!message.contains("JOB_PACKET_JSON"));
    assert!(!message.contains("\"schema\""));
    assert!(!message.contains("\"tools\""));
    assert!(!message.contains("\"manuals\""));
    assert!(!message.contains("\"policy\""));
}

#[test]
fn resumed_agent_task_message_omits_large_session_instructions() {
    let context = prompt_context();
    let message = build_agent_task_message_for_session(&context, false);

    assert!(message.contains("JOB:"));
    assert!(!message.contains("SESSION_INSTRUCTIONS:"));
    assert!(message.contains("===== QUESTION / ACTIVATION ====="));
    assert!(!message.contains("JOB_PACKET_JSON"));
}

fn prompt_context() -> AgentTaskPromptContext {
    AgentTaskPromptContext {
        job_id: "job_test".to_string(),
        guild_id: "guild".to_string(),
        voice_channel_id: "code".to_string(),
        requested_by_user_id: "user-1".to_string(),
        requested_by: "will".to_string(),
        request: "summarize this".to_string(),
        workdir: "/clankcord/state/agent-workspaces/task/guild/code".to_string(),
        previous_context: vec!["[2026-05-15T00:00:00Z] vince (user-2): prior context".to_string()],
        question: vec!["[2026-05-15T00:01:00Z] will (user-1): summarize this".to_string()],
    }
}
