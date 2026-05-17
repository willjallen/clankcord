use std::future::Future;
use std::pin::Pin;

use crate::Result;
use crate::runtime::{
    DiscordForumThreadCreateOutput, DiscordForumThreadCreatePayload,
    DiscordForumThreadRenameOutput, DiscordForumThreadRenamePayload, DiscordTextSendOutput,
    DiscordTextSendPayload, DiscordVoiceDeafenOutput, DiscordVoiceDeafenPayload,
    DiscordVoiceJoinOutput, DiscordVoiceJoinPayload, DiscordVoiceLeaveOutput,
    DiscordVoiceLeavePayload, DiscordVoiceMuteOutput, DiscordVoiceMutePayload,
    DiscordVoicePlayAudioOutput, DiscordVoicePlayAudioPayload, DiscordVoiceStatusSnapshotOutput,
};

pub(crate) type ExternalApiFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

pub(crate) trait RuntimeExternalApi: Send + Sync {
    fn discord_text_send<'a>(
        &'a self,
        payload: DiscordTextSendPayload,
    ) -> ExternalApiFuture<'a, DiscordTextSendOutput>;

    fn discord_forum_thread_create<'a>(
        &'a self,
        payload: DiscordForumThreadCreatePayload,
    ) -> ExternalApiFuture<'a, DiscordForumThreadCreateOutput>;

    fn discord_forum_thread_rename<'a>(
        &'a self,
        payload: DiscordForumThreadRenamePayload,
    ) -> ExternalApiFuture<'a, DiscordForumThreadRenameOutput>;

    fn discord_voice_join<'a>(
        &'a self,
        payload: DiscordVoiceJoinPayload,
    ) -> ExternalApiFuture<'a, DiscordVoiceJoinOutput>;

    fn discord_voice_leave<'a>(
        &'a self,
        payload: DiscordVoiceLeavePayload,
    ) -> ExternalApiFuture<'a, DiscordVoiceLeaveOutput>;

    fn discord_voice_mute<'a>(
        &'a self,
        payload: DiscordVoiceMutePayload,
    ) -> ExternalApiFuture<'a, DiscordVoiceMuteOutput>;

    fn discord_voice_deafen<'a>(
        &'a self,
        payload: DiscordVoiceDeafenPayload,
    ) -> ExternalApiFuture<'a, DiscordVoiceDeafenOutput>;

    fn discord_voice_play_audio<'a>(
        &'a self,
        payload: DiscordVoicePlayAudioPayload,
    ) -> ExternalApiFuture<'a, DiscordVoicePlayAudioOutput>;

    fn discord_voice_status_snapshot<'a>(
        &'a self,
    ) -> ExternalApiFuture<'a, DiscordVoiceStatusSnapshotOutput>;
}
