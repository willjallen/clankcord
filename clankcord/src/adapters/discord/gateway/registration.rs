use std::collections::BTreeSet;

use serde_json::json;

use crate::Result;
use crate::adapters::discord::api::discord_request;
use crate::config::{config_path, load_control_config, load_rooms_payload, read_json};
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
    let guild_ids = configured_guild_ids();
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

fn configured_guild_ids() -> Vec<String> {
    let mut guild_ids = BTreeSet::new();
    let control = load_control_config();
    insert_guild_id(&mut guild_ids, string_field(&control, "guildId"));
    let runtime = read_json(&config_path(), json!({}));
    for guild in runtime
        .get("guilds")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
    {
        insert_guild_id(&mut guild_ids, string_field(guild, "guildId"));
    }
    let rooms = load_rooms_payload();
    for room in rooms
        .get("rooms")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
    {
        insert_guild_id(&mut guild_ids, string_field(room, "guildId"));
    }
    guild_ids.into_iter().collect()
}

fn insert_guild_id(guild_ids: &mut BTreeSet<String>, guild_id: String) {
    let guild_id = guild_id.trim();
    if !guild_id.is_empty() {
        guild_ids.insert(guild_id.to_string());
    }
}
