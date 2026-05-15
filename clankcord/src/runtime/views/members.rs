use serde_json::{Value, json};

use crate::Result;
use crate::adapters::discord::api::list_guild_members;
use crate::errors::discord_tool_error;
use crate::runtime::Runtime;

const MEMBER_CACHE_MAX_AGE_MS: i64 = 60 * 60 * 1000;

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

impl Runtime {
    pub async fn members_search(&self, request: MemberSearchRequest) -> Result<Value> {
        let guild_id = require_guild(request.guild_id)?;
        let refresh = self.ensure_member_cache(&guild_id).await?;
        let members = self
            .timeline_store
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

    pub async fn members_resolve(&self, request: MemberResolveRequest) -> Result<Value> {
        let guild_id = require_guild(request.guild_id)?;
        let refresh = self.ensure_member_cache(&guild_id).await?;
        if request
            .query
            .chars()
            .all(|character| character.is_ascii_digit())
        {
            if let Some(user) = self
                .timeline_store
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
        let candidates = self
            .timeline_store
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

    pub async fn members_get(&self, request: MemberGetRequest) -> Result<Value> {
        let guild_id = require_guild(request.guild_id)?;
        let refresh = self.ensure_member_cache(&guild_id).await?;
        let user = self
            .timeline_store
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

    async fn ensure_member_cache(&self, guild_id: &str) -> Result<Value> {
        let age = self
            .timeline_store
            .discord_member_cache_age_ms(guild_id)
            .await?;
        let current_count = self.timeline_store.count_discord_members(guild_id).await?;
        if age.is_some_and(|age| age < MEMBER_CACHE_MAX_AGE_MS) && current_count > 0 {
            return Ok(
                json!({"refreshed": false, "ageMs": age.unwrap_or(0), "count": current_count}),
            );
        }
        let guild = guild_id.to_string();
        let fetched = tokio::task::spawn_blocking(move || list_guild_members(&guild)).await?;
        match fetched {
            Ok(members) => {
                let stored = self
                    .timeline_store
                    .upsert_discord_members(guild_id, &members)
                    .await?;
                self.timeline_store
                    .mark_discord_member_cache_refreshed(guild_id)
                    .await?;
                Ok(json!({"refreshed": true, "stored": stored, "count": stored}))
            }
            Err(error) if current_count > 0 => Ok(json!({
                "refreshed": false,
                "ageMs": age.unwrap_or(0),
                "count": current_count,
                "refreshError": error.to_string(),
            })),
            Err(error) => Err(error),
        }
    }
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
