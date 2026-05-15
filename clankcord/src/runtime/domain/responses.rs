use serde_json::{Value, json};

use crate::Result;
use crate::adapters::discord::api::{create_dm_channel, send_message};
use crate::config::{MESSAGE_CHUNK_LIMIT, split_message_chunks, string_field};
use crate::runtime::jobs::{DiscordPostMetadata, DiscordPostedMessageMetadata};
use crate::runtime::util::first_non_empty;
use crate::runtime::{
    Job, JobKind, JobOutput, ResponseKind, ResponseOutput, ResponsePayload, ResponseSinkKind,
    Runtime,
};

impl Runtime {
    pub(crate) async fn response_job_from_value(&self, value: &Value) -> Result<Job> {
        let mut payload = ResponsePayload::from_json(value)?;
        let source = if payload.source_job_id.trim().is_empty() {
            None
        } else {
            Some(self.timeline_store.get_job(&payload.source_job_id).await?)
        };
        let guild_id = first_non_empty([
            crate::config::string_field(value, "guild_id"),
            crate::config::string_field(value, "guildId"),
            source
                .as_ref()
                .map(|job| job.guild_id.clone())
                .unwrap_or_default(),
        ]);
        let voice_channel_id = first_non_empty([
            crate::config::string_field(value, "voice_channel_id"),
            crate::config::string_field(value, "channelId"),
            source
                .as_ref()
                .map(|job| job.voice_channel_id.clone())
                .unwrap_or_default(),
        ]);
        if guild_id.trim().is_empty() || voice_channel_id.trim().is_empty() {
            anyhow::bail!("response is missing guild/channel scope");
        }
        if payload.requested_by_user_id.trim().is_empty() {
            payload.requested_by_user_id = source
                .as_ref()
                .map(|job| job.requested_by_user_id.clone())
                .unwrap_or_default();
        }
        if let Some(source_job) = source.as_ref().filter(|job| job.kind == JobKind::AgentTask) {
            let existing = self
                .timeline_store
                .list_response_jobs_for_source(&source_job.id)
                .await?;
            if !existing.is_empty() {
                anyhow::bail!("agent task {} already has a response job", source_job.id);
            }
        }
        Ok(Job::response(
            guild_id,
            voice_channel_id,
            payload.requested_by_user_id.clone(),
            payload,
        ))
    }
}

pub(crate) async fn execute(
    runtime: &Runtime,
    job: &Job,
    payload: &ResponsePayload,
) -> Result<JobOutput> {
    if payload.content.trim().is_empty() {
        anyhow::bail!("response job {} has empty content", job.id);
    }
    let post = match payload.sink.kind {
        ResponseSinkKind::AgentChat => post_to_agent_chat(runtime, job, payload)?,
        ResponseSinkKind::Channel => post_to_channel(job, payload, &payload.sink.channel_id)?,
        ResponseSinkKind::Dm => post_to_dm(job, payload)?,
        ResponseSinkKind::Stdout => {
            return Ok(JobOutput::Response(ResponseOutput {
                response_kind: payload.response_kind.as_str().to_string(),
                sink: payload.sink.clone(),
                source_job_id: payload.source_job_id.clone(),
                content: payload.content.clone(),
                discord_post: None,
            }));
        }
    };
    runtime
        .timeline_store
        .append_event(
            &job.guild_id,
            &job.voice_channel_id,
            json!({
                "event_kind": "response_published",
                "kind": "response_published",
                "job_id": job.id.clone(),
                "source_job_id": payload.source_job_id.clone(),
                "response_kind": payload.response_kind.as_str(),
                "sink": payload.sink.to_json(),
                "discord_post": post.to_json(),
            }),
        )
        .await?;
    Ok(JobOutput::Response(ResponseOutput {
        response_kind: payload.response_kind.as_str().to_string(),
        sink: payload.sink.clone(),
        source_job_id: payload.source_job_id.clone(),
        content: String::new(),
        discord_post: Some(post),
    }))
}

fn post_to_agent_chat(
    runtime: &Runtime,
    job: &Job,
    payload: &ResponsePayload,
) -> Result<DiscordPostMetadata> {
    let channel_id = runtime.control_config.bots_channel_id.clone();
    if channel_id.trim().is_empty() {
        anyhow::bail!("botsChannelId is not configured");
    }
    post_to_channel(job, payload, &channel_id)
}

fn post_to_channel(
    job: &Job,
    payload: &ResponsePayload,
    channel_id: &str,
) -> Result<DiscordPostMetadata> {
    let channel_id = channel_id.trim();
    if channel_id.is_empty() {
        anyhow::bail!("response job {} has no target channel", job.id);
    }
    post_chunks(channel_id, &render_response_content(payload))
}

fn post_to_dm(job: &Job, payload: &ResponsePayload) -> Result<DiscordPostMetadata> {
    let user_id = payload.sink.user_id.trim();
    if user_id.is_empty() {
        anyhow::bail!("response job {} has no DM user id", job.id);
    }
    let channel = create_dm_channel(user_id)?;
    let channel_id = string_field(&channel, "id");
    if channel_id.is_empty() {
        anyhow::bail!("Discord did not return a DM channel id for {user_id}");
    }
    post_chunks(&channel_id, &render_response_content(payload))
}

fn render_response_content(payload: &ResponsePayload) -> String {
    let content = payload.content.trim();
    let mention = payload.requested_by_user_id.trim();
    let prefix = match payload.response_kind {
        ResponseKind::Message => "",
        ResponseKind::Question => "Question: ",
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
        let payload = send_message(channel_id, &chunk)?;
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
