use std::collections::{BTreeMap, BTreeSet};

use crate::Result;
use crate::runtime::timeline::utc_now;
use crate::runtime::{Runtime, VoiceBotStatus, VoiceCaptureSessionStatus};

impl Runtime {
    pub async fn sync_voice_adapter_status(
        &self,
        bots: Vec<VoiceBotStatus>,
        sessions: Vec<VoiceCaptureSessionStatus>,
    ) -> Result<()> {
        self.timeline_store.upsert_voice_bot_states(&bots).await?;
        self.timeline_store
            .upsert_capture_session_statuses(&sessions)
            .await?;

        let active_assignments = self.timeline_store.list_active_voice_assignments().await?;
        let assignments_by_capture_run = active_assignments
            .iter()
            .map(|assignment| (assignment.capture_run_id.clone(), assignment.clone()))
            .collect::<BTreeMap<_, _>>();
        let bot_channels = bots
            .iter()
            .filter(|bot| {
                bot.ready
                    && !bot.current_guild_id.trim().is_empty()
                    && !bot.current_channel_id.trim().is_empty()
            })
            .map(|bot| {
                (
                    bot.bot_id.clone(),
                    (bot.current_guild_id.clone(), bot.current_channel_id.clone()),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let active_session_ids = sessions
            .iter()
            .filter(|session| {
                session.active
                    && session.ended_at.trim().is_empty()
                    && bot_channels
                        .get(&session.bot_id)
                        .is_some_and(|(guild_id, channel_id)| {
                            guild_id == &session.guild_id && channel_id == &session.voice_channel_id
                        })
            })
            .map(|session| session.session_id.clone())
            .collect::<BTreeSet<_>>();

        let mut closed_capture_runs = BTreeSet::new();
        for session in self.timeline_store.list_active_capture_sessions().await? {
            if active_session_ids.contains(&session.session_id) {
                continue;
            }
            if assignments_by_capture_run
                .get(&session.capture_run_id)
                .is_some_and(|assignment| assignment.state == "joining")
            {
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
            closed_capture_runs.insert(session.capture_run_id.clone());
        }

        for assignment in active_assignments {
            if assignment.state == "joining" {
                continue;
            }
            if closed_capture_runs.contains(&assignment.capture_run_id) {
                continue;
            }
            let bot_matches_assignment =
                bot_channels
                    .get(&assignment.voice_bot_id)
                    .is_some_and(|(guild_id, channel_id)| {
                        guild_id == &assignment.guild_id
                            && channel_id == &assignment.voice_channel_id
                    });
            let session_matches_assignment = sessions.iter().any(|session| {
                active_session_ids.contains(&session.session_id)
                    && session.capture_run_id == assignment.capture_run_id
            });
            if bot_matches_assignment && session_matches_assignment {
                continue;
            }
            let ended_at = utc_now();
            self.timeline_store
                .close_capture_run(
                    &assignment.guild_id,
                    &assignment.voice_channel_id,
                    &assignment.capture_run_id,
                    Some(ended_at),
                    "adapter_sync_missing",
                    "ended",
                )
                .await?;
        }

        Ok(())
    }
}
