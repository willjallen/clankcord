use serde_json::json;

use clankcord::runtime::Runtime;

#[test]
fn worker_agent_message_embeds_packet_source_of_truth() {
    let raw = tempfile::tempdir().unwrap();
    let packet_path = raw.path().join("job.packet.json");
    let packet = json!({
        "job_id": "job_test",
        "guild_id": "guild",
        "voice_channel_id": "code",
        "payload": {"command": {"command_kind": "voice_agent_task"}}
    });
    let message = Runtime::build_worker_agent_message(&packet_path, &packet);

    assert!(message.contains("You are handling a Clawcord job."));
    assert!(message.contains(&packet_path.display().to_string()));
    assert!(message.contains("\"job_id\": \"job_test\""));
    assert!(message.contains("JOB_PACKET_JSON:"));
}
