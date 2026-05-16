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
    if !matches!(command.data.name.as_str(), "join" | "leave" | "feedback") {
        return;
    }
    if let Err(error) = command.defer(&ctx.http).await {
        log(&format!("slash command defer failed: {error}"));
    }
    let payload = slash_payload(&command);
    let command_name = payload.command_name.clone();
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

fn slash_payload(command: &CommandInteraction) -> DiscordSlashCommandPayload {
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
        user_id: command.user.id.get().to_string(),
        username: command.user.name.clone(),
        command_name: command.data.name.clone(),
        options: BinaryPayload::from_json(&options).unwrap_or_else(|_| BinaryPayload::empty()),
        created_at: chrono::Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
        response_visibility: "ephemeral".to_string(),
    }
}

fn clipped_text(content: &str, limit: usize) -> String {
    let mut clipped = content.chars().take(limit).collect::<String>();
    if clipped.len() < content.len() {
        clipped.push_str("...");
    }
    clipped
}
