use std::fs;

use reqwest::blocking::multipart::{Form, Part};

use crate::Result;
use crate::adapters::discord::api::{
    create_dm_channel, discord_multipart_request, discord_request,
};
use crate::runtime::jobs::{DiscordPostMetadata, DiscordPostedMessageMetadata};
use crate::runtime::message_chunks::{MESSAGE_CHUNK_LIMIT, split_message_chunks};
use crate::runtime::util::string_field;
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
    let discord_post = if !payload.attachments.is_empty() {
        post_message_with_attachments(&channel_id, &payload)?
    } else if payload.components.is_empty() && payload.allowed_mentions.is_empty() {
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
        messages.push(post_text_chunk(channel_id, &chunk, None)?);
    }
    Ok(DiscordPostMetadata {
        channel_id: channel_id.to_string(),
        messages,
    })
}

fn post_text_chunk(
    channel_id: &str,
    content: &str,
    allowed_mentions: Option<&crate::runtime::BinaryPayload>,
) -> Result<DiscordPostedMessageMetadata> {
    let mut body = serde_json::Map::new();
    body.insert(
        "content".to_string(),
        serde_json::Value::String(content.to_string()),
    );
    if let Some(allowed_mentions) = allowed_mentions
        && !allowed_mentions.is_empty()
    {
        body.insert("allowed_mentions".to_string(), allowed_mentions.to_json());
    }
    let payload = discord_request(
        "POST",
        &format!("/channels/{channel_id}/messages"),
        Some(&serde_json::Value::Object(body)),
        None,
        None,
        30,
    )?;
    Ok(DiscordPostedMessageMetadata {
        channel_id: channel_id.to_string(),
        message_id: string_field(&payload, "id"),
    })
}

fn post_single_message(
    channel_id: &str,
    payload: &DiscordTextSendPayload,
) -> Result<DiscordPostMetadata> {
    let plan = message_send_plan(payload);
    let mut messages = Vec::new();
    for chunk in &plan.leading_text_chunks {
        messages.push(post_text_chunk(
            channel_id,
            chunk,
            Some(&payload.allowed_mentions),
        )?);
    }
    let mut body = serde_json::Map::new();
    body.insert(
        "content".to_string(),
        serde_json::Value::String(plan.final_content),
    );
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
    messages.push(DiscordPostedMessageMetadata {
        channel_id: channel_id.to_string(),
        message_id: string_field(&response, "id"),
    });
    Ok(DiscordPostMetadata {
        channel_id: channel_id.to_string(),
        messages,
    })
}

fn post_message_with_attachments(
    channel_id: &str,
    payload: &DiscordTextSendPayload,
) -> Result<DiscordPostMetadata> {
    let plan = message_send_plan(payload);
    let mut messages = Vec::new();
    for chunk in &plan.leading_text_chunks {
        messages.push(post_text_chunk(
            channel_id,
            chunk,
            Some(&payload.allowed_mentions),
        )?);
    }
    let mut body = serde_json::Map::new();
    body.insert(
        "content".to_string(),
        serde_json::Value::String(plan.final_content),
    );
    if !payload.allowed_mentions.is_empty() {
        body.insert(
            "allowed_mentions".to_string(),
            payload.allowed_mentions.to_json(),
        );
    }
    if !payload.components.is_empty() {
        body.insert("components".to_string(), payload.components.to_json());
    }
    body.insert(
        "attachments".to_string(),
        serde_json::Value::Array(
            payload
                .attachments
                .iter()
                .enumerate()
                .map(|(index, attachment)| {
                    serde_json::json!({
                        "id": index,
                        "filename": attachment.filename.clone(),
                    })
                })
                .collect(),
        ),
    );

    let mut form = Form::new().text("payload_json", serde_json::Value::Object(body).to_string());
    for (index, attachment) in payload.attachments.iter().enumerate() {
        let filename = attachment.filename.trim();
        if filename.is_empty() {
            anyhow::bail!("discord text send attachment has no filename");
        }
        let bytes = fs::read(&attachment.path)?;
        let part = Part::bytes(bytes)
            .file_name(filename.to_string())
            .mime_str("application/zip")?;
        form = form.part(format!("files[{index}]"), part);
    }

    let response = discord_multipart_request(
        "POST",
        &format!("/channels/{channel_id}/messages"),
        form,
        None,
        None,
        60,
    )?;
    messages.push(DiscordPostedMessageMetadata {
        channel_id: channel_id.to_string(),
        message_id: string_field(&response, "id"),
    });
    Ok(DiscordPostMetadata {
        channel_id: channel_id.to_string(),
        messages,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MessageSendPlan {
    leading_text_chunks: Vec<String>,
    final_content: String,
}

fn message_send_plan(payload: &DiscordTextSendPayload) -> MessageSendPlan {
    let content = render_text_content(payload);
    let mut chunks = split_message_chunks(&content, MESSAGE_CHUNK_LIMIT);
    if chunks.is_empty() {
        chunks.push(String::new());
    }
    let final_content = chunks.pop().unwrap_or_default();
    MessageSendPlan {
        leading_text_chunks: chunks,
        final_content,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{
        BinaryPayload, TextAttachmentPayload, TextDeliveryKind, TextTarget, TextTargetKind,
    };

    #[test]
    fn message_send_plan_splits_long_content_before_final_payload_message() {
        let payload = DiscordTextSendPayload {
            intent: TextDeliveryKind::Message,
            target: TextTarget {
                kind: TextTargetKind::Channel,
                channel_id: "channel-1".to_string(),
                user_id: String::new(),
            },
            content: "longword ".repeat(600),
            source_job_id: "job-source".to_string(),
            requested_by_user_id: "user-a".to_string(),
            allowed_mentions: BinaryPayload::empty(),
            components: BinaryPayload::empty(),
            attachments: vec![TextAttachmentPayload {
                path: "/tmp/artifact.zip".to_string(),
                filename: "artifact.zip".to_string(),
                size_bytes: 128,
                sha256: "sha256:abc123".to_string(),
            }],
        };

        let plan = message_send_plan(&payload);

        assert!(!plan.leading_text_chunks.is_empty());
        assert!(
            plan.leading_text_chunks
                .iter()
                .all(|chunk| chunk.len() <= MESSAGE_CHUNK_LIMIT)
        );
        assert!(plan.final_content.len() <= MESSAGE_CHUNK_LIMIT);
    }
}
