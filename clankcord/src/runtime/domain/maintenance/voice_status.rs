use std::collections::BTreeSet;

use crate::Result;
use crate::runtime::timeline::utc_now;
use crate::runtime::{Runtime, VoiceBotStatus, VoiceCaptureSessionStatus};

impl Runtime {
    pub(crate) async fn sync_voice_adapter_status(
        &self,
        bots: Vec<VoiceBotStatus>,
        sessions: Vec<VoiceCaptureSessionStatus>,
    ) -> Result<()> {
        self.timeline_store.upsert_voice_bot_states(&bots).await?;
        self.timeline_store
            .upsert_capture_session_statuses(&sessions)
            .await?;

        let active_session_ids = sessions
            .iter()
            .filter(|session| session.active)
            .map(|session| session.session_id.clone())
            .collect::<BTreeSet<_>>();

        for session in self.timeline_store.list_active_capture_sessions().await? {
            if active_session_ids.contains(&session.session_id) {
                continue;
            }
            let ended_at = utc_now();
            self.timeline_store
                .mark_capture_session_ended(&session.session_id, ended_at)
                .await?;
            self.timeline_store
                .close_capture_run(
                    &session.guild_id,
                    &session.voice_channel_id,
                    &session.capture_run_id,
                    Some(ended_at),
                    "adapter_sync_missing",
                    "ended",
                )
                .await?;
        }

        Ok(())
    }
}
