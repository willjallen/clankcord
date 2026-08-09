use std::collections::{BTreeMap, BTreeSet};

use crate::Result;
use crate::store::utc_now;
use serde_json::Value;

use crate::domain::Ctx;
use crate::domain::voice::{VoiceBotStatus, VoiceCaptureSessionStatus};

pub async fn sync_voice_adapter_status(
    ctx: &Ctx,
    bots: Vec<VoiceBotStatus>,
    sessions: Vec<VoiceCaptureSessionStatus>,
    voice_state_guild_ids: Vec<String>,
    voice_states: Vec<Value>,
) -> Result<()> {
    let bot_count = bots.len();
    let session_count = sessions.len();
    let voice_state_guild_count = voice_state_guild_ids.len();
    let voice_state_count = voice_states.len();
    ctx.store.upsert_voice_bot_states(&bots).await?;
    ctx.store.upsert_capture_session_statuses(&sessions).await?;
    ctx.store
        .sync_authoritative_voice_states(&voice_state_guild_ids, &voice_states)
        .await?;

    let active_assignments = ctx.store.list_active_voice_assignments().await?;
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
    for session in ctx.store.list_active_capture_sessions().await? {
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
        ctx.store
            .mark_capture_session_ended(&session.session_id, ended_at)
            .await?;
        ctx.store
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
                    guild_id == &assignment.guild_id && channel_id == &assignment.voice_channel_id
                });
        let session_matches_assignment = sessions.iter().any(|session| {
            active_session_ids.contains(&session.session_id)
                && session.capture_run_id == assignment.capture_run_id
        });
        if bot_matches_assignment && session_matches_assignment {
            continue;
        }
        let ended_at = utc_now();
        ctx.store
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

    ctx.store
        .record_voice_adapter_snapshot(
            bot_count,
            session_count,
            voice_state_guild_count,
            voice_state_count,
        )
        .await?;

    Ok(())
}
