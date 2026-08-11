//! Durable transcript-window operations: materializing a window (and
//! publishing it when asked) and forgetting a time range. These mutate the
//! timeline; read-only history surfaces stay in views.

use serde_json::Value;

use crate::Result;
use crate::domain::Ctx;
use crate::domain::rooms::catalog;
use crate::domain::transcripts::publication;
use crate::time::{parse_instant, resolve_time_reference, utc_now};
use crate::util::{first_non_empty, non_empty, string_field};

#[derive(Debug, Clone, Default)]
pub struct MaterializeTranscriptRequest {
    pub guild_id: String,
    pub channel_id: String,
    pub since: String,
    pub from: String,
    pub to: String,
    pub publish: String,
    pub live: bool,
    pub created_by_user_id: String,
    pub parent_job_id: String,
}

#[derive(Debug, Clone)]
pub struct ForgetRequest {
    pub window_id: String,
    pub guild_id: String,
    pub channel_id: String,
    pub since: String,
    pub to: String,
    pub requested_by_user_id: String,
    pub unpublished_only: bool,
}

impl Default for ForgetRequest {
    fn default() -> Self {
        Self {
            window_id: String::new(),
            guild_id: String::new(),
            channel_id: String::new(),
            since: "-10m".to_string(),
            to: String::new(),
            requested_by_user_id: String::new(),
            unpublished_only: true,
        }
    }
}

pub async fn materialize_transcript(
    ctx: &Ctx,
    request: MaterializeTranscriptRequest,
) -> Result<Value> {
    let mut guild_id = request.guild_id;
    let mut channel_id = request.channel_id;
    if !guild_id.is_empty() && !channel_id.is_empty() {
        let room = catalog::resolve_room_scope(ctx, &guild_id, Some(&channel_id)).await?;
        guild_id = room.guild_id;
        channel_id = room.channel_id;
    } else {
        let room = catalog::room_for_identifier(
            ctx,
            if channel_id.is_empty() {
                None
            } else {
                Some(&channel_id)
            },
        )
        .await?;
        guild_id = room.guild_id;
        channel_id = room.channel_id;
    }
    let now = utc_now();
    let has_since = !request.since.trim().is_empty();
    let start_raw = first_non_empty([request.since, request.from]);
    let start = resolve_time_reference(&start_raw, Some(now))
        .unwrap_or_else(|| now - chrono::Duration::minutes(10));
    let end = resolve_time_reference(&request.to, Some(now)).unwrap_or(now);
    let publish = non_empty(request.publish, "local".to_string());
    let mut result = ctx
        .store
        .materialize(
            &guild_id,
            &channel_id,
            start,
            end,
            if has_since {
                "relative_time"
            } else {
                "absolute_time_range"
            },
            &non_empty(start_raw, "last 10 minutes".to_string()),
            &request.created_by_user_id,
            &publish,
            request.live,
            if request.parent_job_id.trim().is_empty() {
                None
            } else {
                Some(request.parent_job_id.as_str())
            },
        )
        .await?;
    if publish == "discord" {
        publication::publish_materialized_transcript(ctx, &mut result, request.live).await?;
    }
    Ok(result)
}

pub async fn forget(ctx: &Ctx, request: ForgetRequest) -> Result<Value> {
    let window_id = request.window_id;
    let (guild_id, channel_id, start, end) = if !window_id.is_empty() {
        let window = ctx.store.get_window(&window_id).await?;
        (
            string_field(&window, "guild_id"),
            string_field(&window, "voice_channel_id"),
            parse_instant(&string_field(&window, "start_time"))
                .ok_or_else(|| anyhow::anyhow!("invalid forget window"))?,
            parse_instant(&string_field(&window, "end_time"))
                .ok_or_else(|| anyhow::anyhow!("invalid forget window"))?,
        )
    } else {
        let guild_id = request.guild_id;
        let channel_id = request.channel_id;
        let now = utc_now();
        (
            guild_id,
            channel_id,
            resolve_time_reference(&non_empty(request.since, "-10m".to_string()), Some(now))
                .ok_or_else(|| anyhow::anyhow!("invalid forget start"))?,
            resolve_time_reference(&request.to, Some(now)).unwrap_or(now),
        )
    };
    if guild_id.is_empty() || channel_id.is_empty() {
        return Err(anyhow::anyhow!("invalid forget window"));
    }
    ctx.store
        .apply_forget(
            &guild_id,
            &channel_id,
            start,
            end,
            &request.requested_by_user_id,
            request.unpublished_only,
        )
        .await
}
