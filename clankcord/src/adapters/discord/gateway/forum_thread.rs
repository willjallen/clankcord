use crate::Result;
use crate::adapters::discord::api::create_forum_thread;
use crate::runtime::util::string_field;
use crate::runtime::{DiscordForumThreadCreateOutput, DiscordForumThreadCreatePayload};

pub async fn create(
    payload: DiscordForumThreadCreatePayload,
) -> Result<DiscordForumThreadCreateOutput> {
    tokio::task::spawn_blocking(move || create_blocking(payload)).await?
}

fn create_blocking(
    payload: DiscordForumThreadCreatePayload,
) -> Result<DiscordForumThreadCreateOutput> {
    let created = create_forum_thread(
        &payload.parent_channel_id,
        &payload.name,
        &payload.content,
        payload.auto_archive_minutes,
    )?;
    let thread_id = string_field(&created, "id");
    if thread_id.trim().is_empty() {
        anyhow::bail!("Discord did not return a forum thread id");
    }
    Ok(DiscordForumThreadCreateOutput {
        parent_channel_id: payload.parent_channel_id,
        thread_id,
        name: payload.name,
        source_job_id: payload.source_job_id,
    })
}
