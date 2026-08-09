use std::future::Future;
use std::pin::Pin;

use crate::Result;
use crate::model::job::{
    DiscordForumThreadCreateOutput, DiscordForumThreadCreatePayload,
    DiscordForumThreadRenameOutput, DiscordForumThreadRenamePayload, DiscordTextSendOutput,
    DiscordTextSendPayload, DiscordTypingIndicatorOutput, DiscordTypingIndicatorPayload,
    DiscordVoiceDeafenOutput, DiscordVoiceDeafenPayload, DiscordVoiceJoinOutput,
    DiscordVoiceJoinPayload, DiscordVoiceLeaveOutput, DiscordVoiceLeavePayload,
    DiscordVoiceMuteOutput, DiscordVoiceMutePayload, DiscordVoicePlayAudioOutput,
    DiscordVoicePlayAudioPayload, DiscordVoiceStatusSnapshotOutput,
};

pub type DiscordApiFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

pub trait DiscordApi: Send + Sync {
    fn discord_text_send<'a>(
        &'a self,
        payload: DiscordTextSendPayload,
    ) -> DiscordApiFuture<'a, DiscordTextSendOutput>;

    fn discord_forum_thread_create<'a>(
        &'a self,
        payload: DiscordForumThreadCreatePayload,
    ) -> DiscordApiFuture<'a, DiscordForumThreadCreateOutput>;

    fn discord_forum_thread_rename<'a>(
        &'a self,
        payload: DiscordForumThreadRenamePayload,
    ) -> DiscordApiFuture<'a, DiscordForumThreadRenameOutput>;

    fn discord_typing_indicator<'a>(
        &'a self,
        payload: DiscordTypingIndicatorPayload,
    ) -> DiscordApiFuture<'a, DiscordTypingIndicatorOutput>;

    fn discord_voice_join<'a>(
        &'a self,
        payload: DiscordVoiceJoinPayload,
    ) -> DiscordApiFuture<'a, DiscordVoiceJoinOutput>;

    fn discord_voice_leave<'a>(
        &'a self,
        guild_id: String,
        voice_channel_id: String,
        payload: DiscordVoiceLeavePayload,
    ) -> DiscordApiFuture<'a, DiscordVoiceLeaveOutput>;

    fn discord_voice_mute<'a>(
        &'a self,
        payload: DiscordVoiceMutePayload,
    ) -> DiscordApiFuture<'a, DiscordVoiceMuteOutput>;

    fn discord_voice_deafen<'a>(
        &'a self,
        payload: DiscordVoiceDeafenPayload,
    ) -> DiscordApiFuture<'a, DiscordVoiceDeafenOutput>;

    fn discord_voice_play_audio<'a>(
        &'a self,
        payload: DiscordVoicePlayAudioPayload,
    ) -> DiscordApiFuture<'a, DiscordVoicePlayAudioOutput>;

    fn discord_voice_status_snapshot<'a>(
        &'a self,
    ) -> DiscordApiFuture<'a, DiscordVoiceStatusSnapshotOutput>;

    /// Full member listing for a guild, used by the member_sync job to
    /// refresh the members table.
    fn discord_list_guild_members<'a>(
        &'a self,
        guild_id: String,
    ) -> DiscordApiFuture<'a, Vec<serde_json::Value>>;
}

/// Explicit "no Discord wired" implementation for headless contexts (tests,
/// one-shot CLI). Every effect fails loudly; nothing is silently dropped.
#[derive(Debug, Clone, Copy, Default)]
pub struct DiscordApiUnavailable;

macro_rules! unavailable {
    ($name:ident, $payload:ty, $output:ty) => {
        fn $name<'a>(&'a self, _payload: $payload) -> DiscordApiFuture<'a, $output> {
            Box::pin(async { Err(anyhow::anyhow!("discord api is not wired in this context")) })
        }
    };
}

impl DiscordApi for DiscordApiUnavailable {
    unavailable!(
        discord_text_send,
        DiscordTextSendPayload,
        DiscordTextSendOutput
    );
    unavailable!(
        discord_forum_thread_create,
        DiscordForumThreadCreatePayload,
        DiscordForumThreadCreateOutput
    );
    unavailable!(
        discord_forum_thread_rename,
        DiscordForumThreadRenamePayload,
        DiscordForumThreadRenameOutput
    );
    unavailable!(
        discord_typing_indicator,
        DiscordTypingIndicatorPayload,
        DiscordTypingIndicatorOutput
    );
    unavailable!(
        discord_voice_join,
        DiscordVoiceJoinPayload,
        DiscordVoiceJoinOutput
    );

    fn discord_voice_leave<'a>(
        &'a self,
        _guild_id: String,
        _voice_channel_id: String,
        _payload: DiscordVoiceLeavePayload,
    ) -> DiscordApiFuture<'a, DiscordVoiceLeaveOutput> {
        Box::pin(async { Err(anyhow::anyhow!("discord api is not wired in this context")) })
    }

    unavailable!(
        discord_voice_mute,
        DiscordVoiceMutePayload,
        DiscordVoiceMuteOutput
    );
    unavailable!(
        discord_voice_deafen,
        DiscordVoiceDeafenPayload,
        DiscordVoiceDeafenOutput
    );
    unavailable!(
        discord_voice_play_audio,
        DiscordVoicePlayAudioPayload,
        DiscordVoicePlayAudioOutput
    );

    fn discord_voice_status_snapshot<'a>(
        &'a self,
    ) -> DiscordApiFuture<'a, DiscordVoiceStatusSnapshotOutput> {
        Box::pin(async { Err(anyhow::anyhow!("discord api is not wired in this context")) })
    }

    fn discord_list_guild_members<'a>(
        &'a self,
        _guild_id: String,
    ) -> DiscordApiFuture<'a, Vec<serde_json::Value>> {
        Box::pin(async { Err(anyhow::anyhow!("discord api is not wired in this context")) })
    }
}
