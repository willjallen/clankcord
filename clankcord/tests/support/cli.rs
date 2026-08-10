//! CLI invocation fixtures shared across category binaries.

use clankcord::model::job::BinaryPayload;
use clankcord::model::job::DiscordSlashCommandPayload;
use std::process::Command;

pub fn clankcord(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_clankcord"))
        .args(args)
        .output()
        .expect("clankcord binary runs")
}

pub fn stdout(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
}

pub fn stderr(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).to_string()
}

pub fn slash_payload(
    interaction_id: &str,
    command_name: &str,
    channel_id: &str,
    voice_channel_id: &str,
    options: serde_json::Value,
) -> DiscordSlashCommandPayload {
    DiscordSlashCommandPayload {
        interaction_id: interaction_id.to_string(),
        interaction_token: format!("token-{interaction_id}"),
        application_id: "app-1".to_string(),
        guild_id: "guild".to_string(),
        channel_id: channel_id.to_string(),
        voice_channel_id: voice_channel_id.to_string(),
        user_id: "user-a".to_string(),
        username: "will".to_string(),
        command_name: command_name.to_string(),
        options: BinaryPayload::from_json(&options).unwrap(),
        created_at: "2026-05-15T10:00:00.000Z".to_string(),
        response_visibility: "ephemeral".to_string(),
    }
}
