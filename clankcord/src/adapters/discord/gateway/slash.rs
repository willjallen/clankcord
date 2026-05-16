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
        let content = format!("/{command_name} requires you to be in a voice channel.");
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
    let result = job_sink.submit(Job::discord_slash_command(payload)).await;
    let content = match result {
        Ok(value) => format!(
            "/{command_name} queued as `{}`.",
            value
                .get("job")
                .and_then(|job| job.get("job_id"))
                .and_then(Value::as_str)
                .unwrap_or("job")
        ),
        Err(error) => format!("/{command_name} failed to queue: {error}"),
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
    matches!(command_name, "wake" | "deafen" | "undeafen")
}

fn clipped_text(content: &str, limit: usize) -> String {
    let mut clipped = content.chars().take(limit).collect::<String>();
    if clipped.len() < content.len() {
        clipped.push_str("...");
    }
    clipped
}
