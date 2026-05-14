use serde_json::json;

use clankcord::runtime::domain::interactions::{
    build_agent_task_message, build_agent_task_message_for_session,
};

#[test]
fn agent_task_message_embeds_packet_source_of_truth() {
    let raw = tempfile::tempdir().unwrap();
    let packet_path = raw.path().join("job.packet.json");
    let packet = json!({
        "job_id": "job_test",
        "guild_id": "guild",
        "voice_channel_id": "code",
        "payload": {"command": {"command_kind": "agent_task"}}
    });
    let message = build_agent_task_message(&packet_path, &packet);

    assert!(message.contains("You are Clanky, a helpful and rigorous Discord server assistant"));
    assert!(message.contains("clankcord responses submit"));
    assert!(message.contains("RESPONSE_SUBMITTED"));
    assert!(message.contains("Do not use final text as a publication channel"));
    assert!(!message.contains("response command is unavailable"));
    assert!(message.contains("You may search the web"));
    assert!(message.contains("Do not be sycophantic"));
    assert!(message.contains(&packet_path.display().to_string()));
    assert!(message.contains("\"job_id\": \"job_test\""));
    assert!(message.contains("JOB_PACKET_JSON:"));
}

#[test]
fn resumed_agent_task_message_omits_large_session_instructions() {
    let raw = tempfile::tempdir().unwrap();
    let packet_path = raw.path().join("job.packet.json");
    let packet = json!({
        "job_id": "job_test",
        "guild_id": "guild",
        "voice_channel_id": "code",
        "payload": {"command": {"command_kind": "agent_task"}}
    });
    let message = build_agent_task_message_for_session(&packet_path, &packet, false);

    assert!(message.contains("JOB_CONTEXT:"));
    assert!(!message.contains("SESSION_INSTRUCTIONS:"));
    assert!(message.contains("clankcord responses submit"));
    assert!(message.contains("JOB_PACKET_JSON:"));
}
