use crate::Result;
use crate::ports::discord::DiscordApi;
use crate::runtime::core::execution::JobDecision;
use crate::runtime::{
    Ctx, DiscordVoiceDeafenPayload, DiscordVoiceJoinPayload, DiscordVoiceLeavePayload,
    DiscordVoiceMutePayload, DiscordVoicePlayAudioPayload, JobOutput,
};

pub(crate) async fn execute_discord_voice_join_job<A>(
    _ctx: &Ctx,
    payload: &DiscordVoiceJoinPayload,
    external_api: &A,
) -> Result<JobDecision>
where
    A: DiscordApi,
{
    let output = external_api.discord_voice_join(payload.clone()).await?;
    Ok(JobDecision::Complete(JobOutput::DiscordVoiceJoin(output)))
}

pub(crate) async fn execute_discord_voice_leave_job<A>(
    _ctx: &Ctx,
    job: &crate::runtime::Job,
    payload: &DiscordVoiceLeavePayload,
    external_api: &A,
) -> Result<JobDecision>
where
    A: DiscordApi,
{
    let output = external_api
        .discord_voice_leave(job.guild_id.clone(), job.scope_id.clone(), payload.clone())
        .await?;
    Ok(JobDecision::Complete(JobOutput::DiscordVoiceLeave(output)))
}

pub(crate) async fn execute_discord_voice_mute_job<A>(
    _ctx: &Ctx,
    payload: &DiscordVoiceMutePayload,
    external_api: &A,
) -> Result<JobDecision>
where
    A: DiscordApi,
{
    let output = external_api.discord_voice_mute(payload.clone()).await?;
    Ok(JobDecision::Complete(JobOutput::DiscordVoiceMute(output)))
}

pub(crate) async fn execute_discord_voice_deafen_job<A>(
    _ctx: &Ctx,
    payload: &DiscordVoiceDeafenPayload,
    external_api: &A,
) -> Result<JobDecision>
where
    A: DiscordApi,
{
    let output = external_api.discord_voice_deafen(payload.clone()).await?;
    Ok(JobDecision::Complete(JobOutput::DiscordVoiceDeafen(output)))
}

pub(crate) async fn execute_discord_voice_play_audio_job<A>(
    _ctx: &Ctx,
    payload: &DiscordVoicePlayAudioPayload,
    external_api: &A,
) -> Result<JobDecision>
where
    A: DiscordApi,
{
    let output = external_api
        .discord_voice_play_audio(payload.clone())
        .await?;
    Ok(JobDecision::Complete(JobOutput::DiscordVoicePlayAudio(
        output,
    )))
}

pub(crate) async fn execute_discord_voice_status_snapshot_job<A>(
    _ctx: &Ctx,
    external_api: &A,
) -> Result<JobDecision>
where
    A: DiscordApi,
{
    let output = external_api.discord_voice_status_snapshot().await?;
    Ok(JobDecision::Complete(
        JobOutput::DiscordVoiceStatusSnapshot(output),
    ))
}
