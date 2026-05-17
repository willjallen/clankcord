use crate::Result;
use crate::runtime::core::execution::JobDecision;
use crate::runtime::domain::external::RuntimeExternalApi;
use crate::runtime::{
    DiscordVoiceDeafenPayload, DiscordVoiceJoinPayload, DiscordVoiceLeavePayload,
    DiscordVoiceMutePayload, DiscordVoicePlayAudioPayload, JobOutput, Runtime,
};

impl Runtime {
    pub(crate) async fn execute_discord_voice_join_job<A>(
        &self,
        payload: &DiscordVoiceJoinPayload,
        external_api: &A,
    ) -> Result<JobDecision>
    where
        A: RuntimeExternalApi,
    {
        let output = external_api.discord_voice_join(payload.clone()).await?;
        Ok(JobDecision::Complete(JobOutput::DiscordVoiceJoin(output)))
    }

    pub(crate) async fn execute_discord_voice_leave_job<A>(
        &self,
        payload: &DiscordVoiceLeavePayload,
        external_api: &A,
    ) -> Result<JobDecision>
    where
        A: RuntimeExternalApi,
    {
        let output = external_api.discord_voice_leave(payload.clone()).await?;
        Ok(JobDecision::Complete(JobOutput::DiscordVoiceLeave(output)))
    }

    pub(crate) async fn execute_discord_voice_mute_job<A>(
        &self,
        payload: &DiscordVoiceMutePayload,
        external_api: &A,
    ) -> Result<JobDecision>
    where
        A: RuntimeExternalApi,
    {
        let output = external_api.discord_voice_mute(payload.clone()).await?;
        Ok(JobDecision::Complete(JobOutput::DiscordVoiceMute(output)))
    }

    pub(crate) async fn execute_discord_voice_deafen_job<A>(
        &self,
        payload: &DiscordVoiceDeafenPayload,
        external_api: &A,
    ) -> Result<JobDecision>
    where
        A: RuntimeExternalApi,
    {
        let output = external_api.discord_voice_deafen(payload.clone()).await?;
        Ok(JobDecision::Complete(JobOutput::DiscordVoiceDeafen(output)))
    }

    pub(crate) async fn execute_discord_voice_play_audio_job<A>(
        &self,
        payload: &DiscordVoicePlayAudioPayload,
        external_api: &A,
    ) -> Result<JobDecision>
    where
        A: RuntimeExternalApi,
    {
        let output = external_api
            .discord_voice_play_audio(payload.clone())
            .await?;
        Ok(JobDecision::Complete(JobOutput::DiscordVoicePlayAudio(
            output,
        )))
    }

    pub(crate) async fn execute_discord_voice_status_snapshot_job<A>(
        &self,
        external_api: &A,
    ) -> Result<JobDecision>
    where
        A: RuntimeExternalApi,
    {
        let output = external_api.discord_voice_status_snapshot().await?;
        Ok(JobDecision::Complete(
            JobOutput::DiscordVoiceStatusSnapshot(output),
        ))
    }
}
