use serde_json::json;

use clankcord::runtime::{BinaryPayload, DiscordSlashCommandPayload, Job, JobKind};

#[test]
fn discord_slash_command_job_round_trips() {
    let job = Job::discord_slash_command(DiscordSlashCommandPayload {
        interaction_id: "interaction-1".to_string(),
        interaction_token: "token-1".to_string(),
        application_id: "app-1".to_string(),
        guild_id: "guild".to_string(),
        channel_id: "code".to_string(),
        user_id: "user-a".to_string(),
        username: "will".to_string(),
        command_name: "join".to_string(),
        options: BinaryPayload::from_json(&json!([{"name": "room", "value": "code"}])).unwrap(),
        created_at: "2026-05-15T10:00:00.000Z".to_string(),
        response_visibility: "ephemeral".to_string(),
    });

    let decoded = Job::decode(&job.encode().unwrap()).unwrap();
    assert_eq!(decoded.kind, JobKind::DiscordSlashCommand);
    assert_eq!(decoded.requested_by_user_id, "user-a");
    assert_eq!(decoded.payload.to_json()["command_name"], "join");
    assert_eq!(decoded.payload.to_json()["options"][0]["value"], "code");
}
