use serde_json::{Value, json};

use crate::Result;
use crate::config;
use crate::errors::discord_tool_error;
use crate::runtime::Ctx;

#[derive(Debug, Clone, Default)]
pub struct MemberSearchRequest {
    pub guild_id: String,
    pub query: String,
    pub limit: usize,
}

#[derive(Debug, Clone, Default)]
pub struct MemberResolveRequest {
    pub guild_id: String,
    pub query: String,
}

#[derive(Debug, Clone, Default)]
pub struct MemberGetRequest {
    pub guild_id: String,
    pub user_id: String,
}

pub async fn members_search(ctx: &Ctx, request: MemberSearchRequest) -> Result<Value> {
    let guild_id = require_guild(request.guild_id)?;
    let refresh = ensure_member_cache(ctx, &guild_id).await?;
    let members = ctx
        .store
        .search_discord_members(&guild_id, &request.query, request.limit.max(1))
        .await?;
    Ok(json!({
        "guildId": guild_id,
        "query": request.query,
        "count": members.len(),
        "members": members,
        "cache": refresh,
    }))
}

pub async fn members_resolve(ctx: &Ctx, request: MemberResolveRequest) -> Result<Value> {
    let guild_id = require_guild(request.guild_id)?;
    let refresh = ensure_member_cache(ctx, &guild_id).await?;
    if request
        .query
        .chars()
        .all(|character| character.is_ascii_digit())
    {
        if let Some(user) = ctx
            .store
            .get_discord_member(&guild_id, &request.query)
            .await?
        {
            return Ok(json!({
                "guildId": guild_id,
                "query": request.query,
                "resolved": true,
                "confidence": "high",
                "user": user,
                "candidates": [],
                "cache": refresh,
            }));
        }
    }
    let candidates = ctx
        .store
        .search_discord_members(&guild_id, &request.query, 10)
        .await?;
    let resolved = unambiguous_member(&candidates);
    if let Some(user) = resolved {
        return Ok(json!({
            "guildId": guild_id,
            "query": request.query,
            "resolved": true,
            "confidence": "high",
            "user": user,
            "candidates": [],
            "cache": refresh,
        }));
    }
    Ok(json!({
        "guildId": guild_id,
        "query": request.query,
        "resolved": false,
        "reason": if candidates.is_empty() { "no_match" } else { "ambiguous" },
        "candidates": candidates,
        "cache": refresh,
    }))
}

pub async fn members_get(ctx: &Ctx, request: MemberGetRequest) -> Result<Value> {
    let guild_id = require_guild(request.guild_id)?;
    let refresh = ensure_member_cache(ctx, &guild_id).await?;
    let user = ctx
        .store
        .get_discord_member(&guild_id, &request.user_id)
        .await?;
    Ok(json!({
        "guildId": guild_id,
        "userId": request.user_id,
        "found": user.is_some(),
        "user": user,
        "cache": refresh,
    }))
}

/// Reads never perform Discord I/O. When the members table is stale a
/// member_sync job is submitted (deduplicated per guild) and the current
/// table contents are served with the staleness stated.
async fn ensure_member_cache(ctx: &Ctx, guild_id: &str) -> Result<Value> {
    let age = ctx.store.discord_member_cache_age_ms(guild_id).await?;
    let current_count = ctx.store.count_discord_members(guild_id).await?;
    if age.is_some_and(|age| age < config::discord_member_cache_max_age_ms()) && current_count > 0 {
        return Ok(json!({"fresh": true, "ageMs": age.unwrap_or(0), "count": current_count}));
    }
    let sync_submitted = if ctx.store.has_pending_member_sync(guild_id).await? {
        false
    } else {
        ctx.bus
            .submit_detached(crate::model::job::Job::member_sync(guild_id));
        true
    };
    Ok(json!({
        "fresh": false,
        "ageMs": age.unwrap_or(0),
        "count": current_count,
        "syncSubmitted": sync_submitted,
    }))
}

fn require_guild(guild_id: String) -> Result<String> {
    let guild_id = guild_id.trim().to_string();
    if guild_id.is_empty() {
        Err(discord_tool_error("guild is required"))
    } else {
        Ok(guild_id)
    }
}

fn unambiguous_member(candidates: &[Value]) -> Option<Value> {
    let first = candidates.first()?;
    let first_score = first.get("score").and_then(Value::as_f64).unwrap_or(0.0);
    let second_score = candidates
        .get(1)
        .and_then(|value| value.get("score"))
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    if first_score >= 0.98 || (first_score >= 0.9 && first_score - second_score >= 0.08) {
        Some(first.clone())
    } else {
        None
    }
}
