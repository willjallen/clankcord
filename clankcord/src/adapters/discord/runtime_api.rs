use std::sync::Arc;

use crate::adapters::discord::gateway::{forum_thread, text_send};
use crate::adapters::discord::voice::live::LiveVoiceAdapter;
use crate::runtime::domain::external::{ExternalApiFuture, RuntimeExternalApi};
use crate::runtime::{
    DiscordForumThreadCreateOutput, DiscordForumThreadCreatePayload, DiscordTextSendOutput,
    DiscordTextSendPayload, DiscordVoiceJoinOutput, DiscordVoiceJoinPayload,
    DiscordVoiceLeaveOutput, DiscordVoiceLeavePayload, DiscordVoiceMuteOutput,
    DiscordVoiceMutePayload, DiscordVoicePlayAudioOutput, DiscordVoicePlayAudioPayload,
    DiscordVoiceStatusSnapshotOutput,
};

#[derive(Clone)]
pub(crate) struct DiscordRuntimeApi {
    live_voice: Arc<LiveVoiceAdapter>,
}

impl DiscordRuntimeApi {
    pub(crate) fn new(live_voice: Arc<LiveVoiceAdapter>) -> Self {
        Self { live_voice }
    }
}

impl RuntimeExternalApi for DiscordRuntimeApi {
    fn discord_text_send<'a>(
        &'a self,
        payload: DiscordTextSendPayload,
    ) -> ExternalApiFuture<'a, DiscordTextSendOutput> {
        Box::pin(async move { text_send::send(payload).await })
    }

    fn discord_forum_thread_create<'a>(
        &'a self,
        payload: DiscordForumThreadCreatePayload,
    ) -> ExternalApiFuture<'a, DiscordForumThreadCreateOutput> {
        Box::pin(async move { forum_thread::create(payload).await })
    }

    fn discord_voice_join<'a>(
        &'a self,
        payload: DiscordVoiceJoinPayload,
    ) -> ExternalApiFuture<'a, DiscordVoiceJoinOutput> {
        Box::pin(
            async move { LiveVoiceAdapter::join_assigned_room(&self.live_voice, payload).await },
        )
    }

    fn discord_voice_leave<'a>(
        &'a self,
        payload: DiscordVoiceLeavePayload,
    ) -> ExternalApiFuture<'a, DiscordVoiceLeaveOutput> {
        Box::pin(async move { LiveVoiceAdapter::finish_session(&self.live_voice, payload).await })
    }

    fn discord_voice_mute<'a>(
        &'a self,
        payload: DiscordVoiceMutePayload,
    ) -> ExternalApiFuture<'a, DiscordVoiceMuteOutput> {
        Box::pin(async move { LiveVoiceAdapter::set_session_mute(&self.live_voice, payload).await })
    }

    fn discord_voice_play_audio<'a>(
        &'a self,
        payload: DiscordVoicePlayAudioPayload,
    ) -> ExternalApiFuture<'a, DiscordVoicePlayAudioOutput> {
        Box::pin(async move { LiveVoiceAdapter::play_session_cue(&self.live_voice, payload).await })
    }

    fn discord_voice_status_snapshot<'a>(
        &'a self,
    ) -> ExternalApiFuture<'a, DiscordVoiceStatusSnapshotOutput> {
        Box::pin(async move { Ok(self.live_voice.voice_status_snapshot().await) })
    }
}
