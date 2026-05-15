use super::*;
use serde_json::json;

impl TimelineStore {
    pub async fn upsert_discord_members(&self, guild_id: &str, members: &[Value]) -> Result<usize> {
        let mut stored = 0usize;
        let updated_at_ms = instant_ms_dt(utc_now());
        for member in members {
            let payload = normalize_member_payload(guild_id, member);
            let user_id = string_field_map(payload.as_object().unwrap(), "id");
            if user_id.is_empty() {
                continue;
            }
            let username = string_field_map(payload.as_object().unwrap(), "username");
            let global_name = string_field_map(payload.as_object().unwrap(), "global_name");
            let nick = string_field_map(payload.as_object().unwrap(), "nick");
            let display_name = string_field_map(payload.as_object().unwrap(), "display_name");
            let normalized_search = member_search_blob(&payload);
            sqlx::query(
                r#"
                INSERT INTO discord_members(
                  guild_id, user_id, username, global_name, nick, display_name,
                  normalized_search, updated_at_ms, payload_json
                )
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
                ON CONFLICT (guild_id, user_id) DO UPDATE SET
                  username = EXCLUDED.username,
                  global_name = EXCLUDED.global_name,
                  nick = EXCLUDED.nick,
                  display_name = EXCLUDED.display_name,
                  normalized_search = EXCLUDED.normalized_search,
                  updated_at_ms = EXCLUDED.updated_at_ms,
                  payload_json = EXCLUDED.payload_json
                "#,
            )
            .bind(guild_id)
            .bind(&user_id)
            .bind(&username)
            .bind(&global_name)
            .bind(&nick)
            .bind(&display_name)
            .bind(&normalized_search)
            .bind(updated_at_ms)
            .bind(&payload)
            .execute(&self.pool)
            .await?;
            stored += 1;
        }
        Ok(stored)
    }

    pub async fn mark_discord_member_cache_refreshed(&self, guild_id: &str) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO discord_member_cache_refreshes(guild_id, refreshed_at_ms)
            VALUES ($1, $2)
            ON CONFLICT (guild_id) DO UPDATE SET refreshed_at_ms = EXCLUDED.refreshed_at_ms
            "#,
        )
        .bind(guild_id)
        .bind(instant_ms_dt(utc_now()))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn discord_member_cache_age_ms(&self, guild_id: &str) -> Result<Option<i64>> {
        let row = sqlx::query(
            "SELECT refreshed_at_ms FROM discord_member_cache_refreshes WHERE guild_id = $1",
        )
        .bind(guild_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|row| instant_ms_dt(utc_now()) - row.get::<i64, _>("refreshed_at_ms")))
    }

    pub async fn count_discord_members(&self, guild_id: &str) -> Result<usize> {
        let row = sqlx::query("SELECT COUNT(*) AS count FROM discord_members WHERE guild_id = $1")
            .bind(guild_id)
            .fetch_one(&self.pool)
            .await?;
        Ok(row.get::<i64, _>("count") as usize)
    }

    pub async fn get_discord_member(&self, guild_id: &str, user_id: &str) -> Result<Option<Value>> {
        let row = sqlx::query(
            "SELECT payload_json FROM discord_members WHERE guild_id = $1 AND user_id = $2",
        )
        .bind(guild_id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|row| row.get::<Value, _>("payload_json")))
    }

    pub async fn search_discord_members(
        &self,
        guild_id: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<Value>> {
        let rows = sqlx::query(
            r#"
            SELECT payload_json
            FROM discord_members
            WHERE guild_id = $1
            "#,
        )
        .bind(guild_id)
        .fetch_all(&self.pool)
        .await?;
        let mut scored = rows
            .into_iter()
            .map(|row| row.get::<Value, _>("payload_json"))
            .map(|member| {
                let score = member_match_score(&member, query);
                (score, member)
            })
            .filter(|(score, _)| *score > 0.0)
            .collect::<Vec<_>>();
        scored.sort_by(|left, right| {
            right
                .0
                .partial_cmp(&left.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| {
                    string_field(&left.1, "display_name")
                        .cmp(&string_field(&right.1, "display_name"))
                })
        });
        Ok(scored
            .into_iter()
            .take(limit.max(1))
            .map(|(score, mut member)| {
                if let Some(object) = member.as_object_mut() {
                    object.insert("score".to_string(), json!(round3(score)));
                }
                member
            })
            .collect())
    }
}

pub(crate) fn normalize_member_query(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn normalize_member_payload(guild_id: &str, member: &Value) -> Value {
    let user = member.get("user").unwrap_or(member);
    let user_id = string_field(user, "id");
    let username = string_field(user, "username");
    let global_name = string_field(user, "global_name");
    let nick = string_field(member, "nick");
    let display_name = non_empty(
        [nick.clone(), global_name.clone(), username.clone()]
            .into_iter()
            .find(|value| !value.trim().is_empty())
            .unwrap_or_default(),
        user_id.clone(),
    );
    json!({
        "guild_id": guild_id,
        "id": user_id,
        "username": username,
        "global_name": global_name,
        "nick": if nick.is_empty() { Value::Null } else { Value::String(nick) },
        "display_name": display_name,
        "avatar": user.get("avatar").cloned().unwrap_or(Value::Null),
        "member_avatar": member.get("avatar").cloned().unwrap_or(Value::Null),
        "bot": user.get("bot").and_then(Value::as_bool).unwrap_or(false),
    })
}

fn member_search_blob(member: &Value) -> String {
    member_aliases(member)
        .into_iter()
        .map(|alias| normalize_member_query(&alias))
        .filter(|alias| !alias.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn member_aliases(member: &Value) -> Vec<String> {
    let mut values = vec![
        string_field(member, "id"),
        string_field(member, "username"),
        string_field(member, "global_name"),
        string_field(member, "nick"),
        string_field(member, "display_name"),
    ];
    values.extend(
        member
            .get("speaker_labels")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|value| value.as_str().map(ToOwned::to_owned)),
    );
    values
}

fn member_match_score(member: &Value, query: &str) -> f64 {
    let normalized_query = normalize_member_query(query);
    if normalized_query.is_empty() {
        return 0.0;
    }
    let query_tokens = member_tokens(query);
    let mut best = 0.0f64;
    for alias in member_aliases(member) {
        let normalized_alias = normalize_member_query(&alias);
        if normalized_alias.is_empty() {
            continue;
        }
        let score = if normalized_alias == normalized_query {
            1.0
        } else if normalized_alias.starts_with(&normalized_query)
            || normalized_query.starts_with(&normalized_alias)
        {
            0.92
        } else if normalized_alias.contains(&normalized_query)
            || normalized_query.contains(&normalized_alias)
        {
            0.84
        } else {
            token_overlap_score(&query_tokens, &member_tokens(&alias))
        };
        best = best.max(score);
    }
    best
}

fn member_tokens(value: &str) -> BTreeSet<String> {
    value
        .split(|character: char| !character.is_ascii_alphanumeric())
        .flat_map(split_camel_token)
        .map(|token| token.to_ascii_lowercase())
        .filter(|token| !token.is_empty())
        .collect()
}

fn split_camel_token(value: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for character in value.chars() {
        if character.is_ascii_uppercase() && !current.is_empty() {
            tokens.push(current);
            current = String::new();
        }
        current.push(character);
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

fn token_overlap_score(left: &BTreeSet<String>, right: &BTreeSet<String>) -> f64 {
    if left.is_empty() || right.is_empty() {
        return 0.0;
    }
    let shared = left.intersection(right).count() as f64;
    let total = left.union(right).count() as f64;
    if total == 0.0 {
        0.0
    } else {
        (shared / total) * 0.78
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn member_normalization_matches_spaced_name_to_camel_name() {
        let member = json!({
            "id": "284362763386617857",
            "username": "mysterymanchien",
            "global_name": "MysteryManChien",
            "nick": null,
            "display_name": "MysteryManChien"
        });

        assert_eq!(
            normalize_member_query("Mystery Man Chien"),
            normalize_member_query("MysteryManChien")
        );
        assert_eq!(member_match_score(&member, "Mystery Man Chien"), 1.0);
    }

    #[test]
    fn ambiguous_member_queries_can_remain_ranked_candidates() {
        let mystery = json!({"id": "1", "display_name": "MysteryManChien"});
        let guest = json!({"id": "2", "display_name": "MysteryGuest"});

        assert!(member_match_score(&mystery, "mystery") > 0.0);
        assert!(member_match_score(&guest, "mystery") > 0.0);
    }
}
