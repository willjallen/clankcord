use serde_json::json;

use crate::Result;
use crate::adapters::discord::api::{discord_request, string_field};

pub fn register_slash_commands() -> Result<()> {
    let application = discord_request("GET", "/oauth2/applications/@me", None, None, None, 30)?;
    let application_id = string_field(&application, "id");
    if application_id.trim().is_empty() {
        anyhow::bail!("Discord application id was not returned");
    }
    let commands = json!([
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
    ]);
    discord_request(
        "PUT",
        &format!("/applications/{application_id}/commands"),
        Some(&commands),
        None,
        None,
        30,
    )?;
    Ok(())
}
