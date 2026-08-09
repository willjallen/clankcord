use crate::Result;
use crate::domain::Ctx;
use crate::engine::JobDecision;
use crate::model::job::{
    DiscordForumThreadCreatePayload, DiscordForumThreadRenamePayload, DiscordTextSendPayload,
    JobOutput,
};
use crate::ports::discord::DiscordApi;

pub(crate) async fn execute_discord_text_send_job<A>(
    _ctx: &Ctx,
    payload: &DiscordTextSendPayload,
    external_api: &A,
) -> Result<JobDecision>
where
    A: DiscordApi,
{
    let output = external_api.discord_text_send(payload.clone()).await?;
    Ok(JobDecision::Complete(JobOutput::DiscordTextSend(output)))
}

pub(crate) async fn execute_discord_forum_thread_create_job<A>(
    _ctx: &Ctx,
    payload: &DiscordForumThreadCreatePayload,
    external_api: &A,
) -> Result<JobDecision>
where
    A: DiscordApi,
{
    let output = external_api
        .discord_forum_thread_create(payload.clone())
        .await?;
    Ok(JobDecision::Complete(JobOutput::DiscordForumThreadCreate(
        output,
    )))
}

pub(crate) async fn execute_discord_forum_thread_rename_job<A>(
    _ctx: &Ctx,
    payload: &DiscordForumThreadRenamePayload,
    external_api: &A,
) -> Result<JobDecision>
where
    A: DiscordApi,
{
    let output = external_api
        .discord_forum_thread_rename(payload.clone())
        .await?;
    Ok(JobDecision::Complete(JobOutput::DiscordForumThreadRename(
        output,
    )))
}
