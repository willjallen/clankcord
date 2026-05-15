use clankcord::runtime::domain::interactions::{
    AgentTaskPromptContext, agent_invocation_infrastructure_failure, build_agent_task_message,
};

#[test]
fn agent_task_prompt_is_compact_and_packet_free() {
    let prompt = build_agent_task_message(&AgentTaskPromptContext {
        job_id: "job_1".to_string(),
        guild_id: "guild".to_string(),
        voice_channel_id: "voice".to_string(),
        requested_by_user_id: "user".to_string(),
        requested_by: "Will".to_string(),
        request: "summarize the floating point discussion".to_string(),
        workdir: "/clankcord/state/agent-workspaces/task/guild/voice".to_string(),
        previous_context: vec![
            "[2026-05-15T00:00:00Z] Will (user): we were talking about floats".to_string(),
        ],
        question: vec!["[2026-05-15T00:01:00Z] Will (user): hey clanky summarize this".to_string()],
    });

    assert!(prompt.contains("===== PREVIOUS CONTEXT ====="));
    assert!(prompt.contains("===== QUESTION / ACTIVATION ====="));
    assert!(prompt.contains("CLANKCORD_AGENT_WORKDIR"));
    assert!(prompt.contains("CLANKCORD_AGENT_JOB_ID"));
    assert!(prompt.contains("NO_RESPONSE_NEEDED"));
    assert!(prompt.contains("clankcord --help"));
    assert!(prompt.contains("clankcord responses --help"));
    assert!(prompt.contains("clankcord responses dm"));
    assert!(prompt.contains("treat the request and the answer as private"));
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
fn codex_auth_failure_is_infrastructure_failure_but_watchdog_text_is_not() {
    assert!(agent_invocation_infrastructure_failure(
        "Auth(TokenRefreshFailed(\"invalid_grant\"))"
    ));
    assert!(!agent_invocation_infrastructure_failure(
        "codex command timed out after 240 seconds"
    ));
}
