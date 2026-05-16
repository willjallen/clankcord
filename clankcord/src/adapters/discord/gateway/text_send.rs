use crate::Result;
use crate::adapters::discord::api::{create_dm_channel, discord_request};
use crate::runtime::jobs::{DiscordPostMetadata, DiscordPostedMessageMetadata};
use crate::runtime::util::{MESSAGE_CHUNK_LIMIT, split_message_chunks, string_field};
use crate::runtime::{
    DiscordTextSendOutput, DiscordTextSendPayload, TextDeliveryKind, TextTargetKind,
};

pub async fn send(payload: DiscordTextSendPayload) -> Result<DiscordTextSendOutput> {
    tokio::task::spawn_blocking(move || send_blocking(payload)).await?
}

fn send_blocking(payload: DiscordTextSendPayload) -> Result<DiscordTextSendOutput> {
    let channel_id = match payload.target.kind {
        TextTargetKind::Channel => {
            let channel_id = payload.target.channel_id.trim();
            if channel_id.is_empty() {
                anyhow::bail!("discord text send has no target channel");
            }
            channel_id.to_string()
        }
        TextTargetKind::Dm => {
            let user_id = payload.target.user_id.trim();
            if user_id.is_empty() {
                anyhow::bail!("discord text send has no target DM user");
            }
            let channel = create_dm_channel(user_id)?;
            let channel_id = string_field(&channel, "id");
            if channel_id.is_empty() {
                anyhow::bail!("Discord did not return a DM channel id for {user_id}");
            }
            channel_id
        }
        kind => anyhow::bail!(
            "discord text send requires a concrete Discord target, got {}",
            kind.as_str()
        ),
    };
    let discord_post = if payload.components.is_empty() && payload.allowed_mentions.is_empty() {
        post_chunks(&channel_id, &render_text_content(&payload))?
    } else {
        post_single_message(&channel_id, &payload)?
    };
    Ok(DiscordTextSendOutput {
        target: payload.target,
        source_job_id: payload.source_job_id,
        discord_post,
    })
}

fn render_text_content(payload: &DiscordTextSendPayload) -> String {
    let content = payload.content.trim();
    let mention = payload.requested_by_user_id.trim();
    let prefix = match payload.intent {
        TextDeliveryKind::Message => "",
        TextDeliveryKind::Question => "Question: ",
    };
    if mention.is_empty() {
        format!("{prefix}{content}")
    } else {
        format!("<@{mention}> {prefix}{content}")
    }
}

fn post_chunks(channel_id: &str, content: &str) -> Result<DiscordPostMetadata> {
    let mut messages = Vec::new();
    for chunk in split_message_chunks(content, MESSAGE_CHUNK_LIMIT) {
        let payload = discord_request(
            "POST",
            &format!("/channels/{channel_id}/messages"),
            Some(&serde_json::json!({"content": chunk})),
            None,
            None,
            30,
        )?;
        messages.push(DiscordPostedMessageMetadata {
            channel_id: channel_id.to_string(),
            message_id: string_field(&payload, "id"),
        });
    }
    Ok(DiscordPostMetadata {
        channel_id: channel_id.to_string(),
        messages,
    })
}

fn post_single_message(
    channel_id: &str,
    payload: &DiscordTextSendPayload,
) -> Result<DiscordPostMetadata> {
    let content = render_text_content(payload);
    if content.len() > MESSAGE_CHUNK_LIMIT {
        anyhow::bail!("discord text send with components exceeds message limit");
    }
    let mut body = serde_json::Map::new();
    body.insert("content".to_string(), serde_json::Value::String(content));
    if !payload.allowed_mentions.is_empty() {
        body.insert(
            "allowed_mentions".to_string(),
            payload.allowed_mentions.to_json(),
        );
    }
    if !payload.components.is_empty() {
        body.insert("components".to_string(), payload.components.to_json());
    }
    let response = discord_request(
        "POST",
        &format!("/channels/{channel_id}/messages"),
        Some(&serde_json::Value::Object(body)),
        None,
        None,
        30,
    )?;
    Ok(DiscordPostMetadata {
        channel_id: channel_id.to_string(),
        messages: vec![DiscordPostedMessageMetadata {
            channel_id: channel_id.to_string(),
            message_id: string_field(&response, "id"),
        }],
    })
}
