use crate::Result;
use crate::runtime::core::execution::JobDecision;
use crate::runtime::domain::external::RuntimeExternalApi;
use crate::runtime::{
    DiscordForumThreadCreatePayload, DiscordForumThreadRenamePayload, DiscordTextSendPayload,
    JobOutput, Runtime,
};

impl Runtime {
    pub(crate) async fn execute_discord_text_send_job<A>(
        &self,
        payload: &DiscordTextSendPayload,
        external_api: &A,
    ) -> Result<JobDecision>
    where
        A: RuntimeExternalApi,
    {
        let output = external_api.discord_text_send(payload.clone()).await?;
        Ok(JobDecision::Complete(JobOutput::DiscordTextSend(output)))
    }

    pub(crate) async fn execute_discord_forum_thread_create_job<A>(
        &self,
        payload: &DiscordForumThreadCreatePayload,
        external_api: &A,
    ) -> Result<JobDecision>
    where
        A: RuntimeExternalApi,
    {
        let output = external_api
            .discord_forum_thread_create(payload.clone())
            .await?;
        Ok(JobDecision::Complete(JobOutput::DiscordForumThreadCreate(
            output,
        )))
    }

    pub(crate) async fn execute_discord_forum_thread_rename_job<A>(
        &self,
        payload: &DiscordForumThreadRenamePayload,
        external_api: &A,
    ) -> Result<JobDecision>
    where
        A: RuntimeExternalApi,
    {
        let output = external_api
            .discord_forum_thread_rename(payload.clone())
            .await?;
        Ok(JobDecision::Complete(JobOutput::DiscordForumThreadRename(
            output,
        )))
    }
}
