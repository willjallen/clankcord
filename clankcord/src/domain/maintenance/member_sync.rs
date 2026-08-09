use serde_json::{Value, json};

use crate::Result;
use crate::domain::Ctx;
use crate::model::job::MemberSyncPayload;
use crate::ports::discord::DiscordApi;
use crate::store::{instant_ms_dt, utc_now};

/// Refreshes the durable members table for one guild from Discord. The
/// members views read only the table; this job is the sole writer.
pub async fn execute<A: DiscordApi>(
    ctx: &Ctx,
    payload: &MemberSyncPayload,
    external_api: &A,
) -> Result<Value> {
    let started_ms = instant_ms_dt(utc_now());
    let members = external_api
        .discord_list_guild_members(payload.guild_id.clone())
        .await?;
    let stored = ctx
        .store
        .upsert_discord_members(&payload.guild_id, &members)
        .await?;
    ctx.store
        .mark_discord_member_cache_refreshed(&payload.guild_id)
        .await?;
    Ok(json!({
        "kind": "member_sync",
        "guild_id": payload.guild_id,
        "stored": stored,
        "elapsed_ms": instant_ms_dt(utc_now()).saturating_sub(started_ms),
    }))
}
