use serde_json::json;

use crate::Result;
use crate::adapters::discord::api::discord_request;
use crate::config;
use crate::runtime::util::string_field;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlashCommandRegistration {
    pub application_id: String,
    pub application_name: String,
    pub guild_ids: Vec<String>,
    pub command_count: usize,
    pub cleared_global_commands: bool,
}

pub fn register_slash_commands(token: &str) -> Result<SlashCommandRegistration> {
    let application = discord_request(
        "GET",
        "/oauth2/applications/@me",
        None,
        None,
        Some(token),
        30,
    )?;
    let application_id = string_field(&application, "id");
    if application_id.trim().is_empty() {
        anyhow::bail!("Discord application id was not returned");
    }
    let commands = slash_commands();
    let guild_ids = config::configured_guild_ids();
    if guild_ids.is_empty() {
        anyhow::bail!("no Discord guilds are configured for slash command registration");
    }

    discord_request(
        "PUT",
        &format!("/applications/{application_id}/commands"),
        Some(&json!([])),
        None,
        Some(token),
        30,
    )?;
    for guild_id in &guild_ids {
        discord_request(
            "PUT",
            &format!("/applications/{application_id}/guilds/{guild_id}/commands"),
            Some(&commands),
            None,
            Some(token),
            30,
        )?;
    }
    Ok(SlashCommandRegistration {
        application_id,
        application_name: string_field(&application, "name"),
        guild_ids,
        command_count: commands.as_array().map(Vec::len).unwrap_or(0),
        cleared_global_commands: true,
    })
}

fn slash_commands() -> serde_json::Value {
    json!([
        {
            "name": "join",
            "description": "Ask Clanky to join a voice room.",
            "type": 1,
            "options": [{
                "name": "room",
                "description": "Voice room id, channel id, or configured room name.",
                "type": 3,
                "required": false
            }]
        },
        {
            "name": "leave",
            "description": "Ask Clanky to leave a voice room.",
            "type": 1,
            "options": [{
                "name": "room",
                "description": "Voice room id, channel id, or configured room name.",
                "type": 3,
                "required": false
            }]
        },
        {
            "name": "feedback",
            "description": "Send feedback.",
            "type": 1,
            "options": [{
                "name": "message",
                "description": "Feedback text.",
                "type": 3,
                "required": true
            }]
        }
    ])
}
