use serenity::async_trait;
use serenity::client::{Client, Context, EventHandler};
use serenity::model::application::Interaction;
use serenity::model::channel::Message;
use serenity::model::gateway::GatewayIntents;

use crate::Result;
use crate::adapters::discord::api::load_discord_bot_token;
use crate::adapters::discord::gateway::{components, registration, slash};
use crate::runtime::{DiscordTextMessagePayload, Job, RuntimeJobSink, log};

#[derive(Clone)]
pub struct DiscordTextAdapter {
    job_sink: RuntimeJobSink,
}

impl DiscordTextAdapter {
    pub fn new(job_sink: RuntimeJobSink) -> Self {
        Self { job_sink }
    }

    pub fn spawn(self) {
        tokio::spawn(async move {
            if let Err(error) = self.run().await {
                log(&format!("discord text adapter stopped: {error}"));
            }
        });
    }

    async fn run(self) -> Result<()> {
        let token = match load_discord_bot_token() {
            Ok(token) => token,
            Err(error) => {
                log(&format!("discord text adapter disabled: {error}"));
                return Ok(());
            }
        };
        if let Err(error) = registration::register_slash_commands() {
            log(&format!(
                "discord slash command registration failed: {error}"
            ));
        }
        let intents = GatewayIntents::GUILDS
            | GatewayIntents::GUILD_MESSAGES
            | GatewayIntents::DIRECT_MESSAGES
            | GatewayIntents::MESSAGE_CONTENT;
        let handler = DiscordTextGatewayHandler {
            job_sink: self.job_sink,
        };
        let mut client = Client::builder(&token, intents)
            .event_handler(handler)
            .await?;
        client.start_autosharded().await?;
        Ok(())
    }
}

struct DiscordTextGatewayHandler {
    job_sink: RuntimeJobSink,
}

#[async_trait]
impl EventHandler for DiscordTextGatewayHandler {
    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        match interaction {
            Interaction::Command(command) => {
                slash::handle_slash_command(self.job_sink.clone(), ctx, command).await;
            }
            Interaction::Component(component) => {
                components::handle_component_interaction(self.job_sink.clone(), ctx, component)
                    .await;
            }
            _ => {}
        }
    }

    async fn message(&self, _ctx: Context, message: Message) {
        if message.author.bot {
            return;
        }
        let payload = DiscordTextMessagePayload {
            guild_id: message
                .guild_id
                .map(|guild_id| guild_id.get().to_string())
                .unwrap_or_default(),
            channel_id: message.channel_id.get().to_string(),
            message_id: message.id.get().to_string(),
            author_user_id: message.author.id.get().to_string(),
            author_username: message.author.name.clone(),
            author_display_name: message.author.global_name.clone().unwrap_or_default(),
            content: message.content.clone(),
            created_at: message.timestamp.to_rfc3339().unwrap_or_default(),
            referenced_message_id: message
                .referenced_message
                .as_ref()
                .map(|referenced| referenced.id.get().to_string())
                .unwrap_or_default(),
        };
        self.job_sink
            .submit_detached(Job::discord_text_message(payload));
    }
}
