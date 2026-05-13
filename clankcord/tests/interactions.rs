use serde_json::json;

use clankcord::runtime::domain::interactions::build_agent_task_message;

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

    assert!(message.contains("You are handling a Clawcord agent task."));
    assert!(message.contains(&packet_path.display().to_string()));
    assert!(message.contains("\"job_id\": \"job_test\""));
    assert!(message.contains("JOB_PACKET_JSON:"));
}
