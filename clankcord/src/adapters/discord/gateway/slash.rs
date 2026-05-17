use chrono::SecondsFormat;
use serde_json::{Value, json};
use serenity::builder::EditInteractionResponse;
use serenity::client::Context;
use serenity::model::application::CommandInteraction;

use crate::runtime::{BinaryPayload, DiscordSlashCommandPayload, Job, RuntimeJobSink, log};

pub async fn handle_slash_command(
    job_sink: RuntimeJobSink,
    ctx: Context,
    command: CommandInteraction,
) {
    if !is_supported_slash_command(&command.data.name) {
        return;
    }
    if let Err(error) = command.defer_ephemeral(&ctx.http).await {
        log(&format!("slash command defer failed: {error}"));
    }
    let payload = slash_payload(&ctx, &command);
    let command_name = payload.command_name.clone();
    if slash_command_requires_voice_channel(&command_name) && payload.voice_channel_id.is_empty() {
        let content = slash_missing_voice_channel_response_content();
        if let Err(error) = command
            .edit_response(
                &ctx.http,
                EditInteractionResponse::new().content(clipped_text(&content, 1900)),
            )
            .await
        {
            log(&format!("slash command edit response failed: {error}"));
        }
        return;
    }
    let success_content = slash_success_response_content(&payload);
    let result = job_sink.submit(Job::discord_slash_command(payload)).await;
    let content = match result {
        Ok(_) => success_content,
        Err(error) => format!("I couldn't start /{command_name}: {error}"),
    };
    if let Err(error) = command
        .edit_response(
            &ctx.http,
            EditInteractionResponse::new().content(clipped_text(&content, 1900)),
        )
        .await
    {
        log(&format!("slash command edit response failed: {error}"));
    }
}

pub fn slash_success_response_content(payload: &DiscordSlashCommandPayload) -> String {
    match payload.command_name.as_str() {
        "join" => format!(
            "Connecting Clanky to {}.",
            slash_voice_channel_label(payload)
        ),
        "leave" => format!(
            "Disconnecting Clanky from {}.",
            slash_voice_channel_label(payload)
        ),
        "wake" => format!("Waking Clanky in {}.", slash_voice_channel_label(payload)),
        "deafen" => format!(
            "Deafening Clanky in {}.",
            slash_voice_channel_label(payload)
        ),
        "undeafen" => format!(
            "Undeafening Clanky in {}.",
            slash_voice_channel_label(payload)
        ),
        "feedback" => {
            let message = slash_option_string(payload, &["message"]);
            if message.trim().is_empty() {
                "Feedback sent.".to_string()
            } else {
                format!("Feedback sent: {message}")
            }
        }
        command => format!("/{command} received."),
    }
}

pub fn slash_missing_voice_channel_response_content() -> &'static str {
    "You are not in a voice channel."
}

fn slash_payload(ctx: &Context, command: &CommandInteraction) -> DiscordSlashCommandPayload {
    let options = serde_json::to_value(&command.data.options).unwrap_or_else(|_| json!([]));
    DiscordSlashCommandPayload {
        interaction_id: command.id.get().to_string(),
        interaction_token: command.token.clone(),
        application_id: command.application_id.get().to_string(),
        guild_id: command
            .guild_id
            .map(|guild_id| guild_id.get().to_string())
            .unwrap_or_default(),
        channel_id: command.channel_id.get().to_string(),
        voice_channel_id: invoker_voice_channel_id(ctx, command),
        user_id: command.user.id.get().to_string(),
        username: command.user.name.clone(),
        command_name: command.data.name.clone(),
        options: BinaryPayload::from_json(&options).unwrap_or_else(|_| BinaryPayload::empty()),
        created_at: chrono::Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
        response_visibility: "ephemeral".to_string(),
    }
}

fn invoker_voice_channel_id(ctx: &Context, command: &CommandInteraction) -> String {
    let Some(guild_id) = command.guild_id else {
        return String::new();
    };
    ctx.cache
        .guild(guild_id)
        .and_then(|guild| {
            guild
                .voice_states
                .get(&command.user.id)
                .and_then(|voice_state| voice_state.channel_id)
        })
        .map(|channel_id| channel_id.get().to_string())
        .unwrap_or_default()
}

fn is_supported_slash_command(command_name: &str) -> bool {
    matches!(
        command_name,
        "join" | "leave" | "feedback" | "wake" | "deafen" | "undeafen"
    )
}

fn slash_command_requires_voice_channel(command_name: &str) -> bool {
    matches!(
        command_name,
        "join" | "leave" | "wake" | "deafen" | "undeafen"
    )
}

fn slash_voice_channel_label(payload: &DiscordSlashCommandPayload) -> String {
    let voice_channel_id = payload.voice_channel_id.trim();
    if voice_channel_id.is_empty() {
        "your voice channel".to_string()
    } else {
        format!("<#{voice_channel_id}>")
    }
}

fn slash_option_string(payload: &DiscordSlashCommandPayload, names: &[&str]) -> String {
    let options = payload.options_json();
    if let Some(object) = options.as_object() {
        for name in names {
            if let Some(value) = object.get(*name).and_then(option_value_string) {
                return value;
            }
        }
    }
    if let Some(values) = options.as_array() {
        for wanted in names {
            for option in values {
                if option.get("name").and_then(Value::as_str) == Some(*wanted)
                    && let Some(value) = option.get("value").and_then(option_value_string)
                {
                    return value;
                }
            }
        }
    }
    String::new()
}

fn option_value_string(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(ToOwned::to_owned)
        .or_else(|| value.as_i64().map(|value| value.to_string()))
        .or_else(|| value.as_u64().map(|value| value.to_string()))
        .or_else(|| {
            value
                .as_object()
                .and_then(|object| object.get("String").or_else(|| object.get("value")))
                .and_then(option_value_string)
        })
}

fn clipped_text(content: &str, limit: usize) -> String {
    let mut clipped = content.chars().take(limit).collect::<String>();
    if clipped.len() < content.len() {
        clipped.push_str("...");
    }
    clipped
}
