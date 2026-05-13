use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::PathBuf;

use anyhow::Context;
use reqwest::blocking::Client;
use serde_json::Value;

use crate::Result;
use crate::errors::discord_tool_error;

pub const GUILD_TEXT_CHANNEL_TYPES: &[i64] = &[0, 5];
pub const THREAD_CHANNEL_TYPES: &[i64] = &[10, 11, 12];
pub const FORUM_CHANNEL_TYPE: i64 = 15;

pub fn discord_api_base() -> String {
    env::var("OPENCLAW_DISCORD_API_BASE")
        .unwrap_or_else(|_| "https://discord.com/api/v10".to_string())
        .trim_end_matches('/')
        .to_string()
}

pub fn read_secret_value(env_key: &str, file_env_key: &str, default_secret_path: &str) -> String {
    let value = env::var(env_key).unwrap_or_default().trim().to_string();
    if !value.is_empty() {
        return value;
    }
    let file_path = env::var(file_env_key)
        .unwrap_or_default()
        .trim()
        .to_string();
    let file_path = if file_path.is_empty() {
        default_secret_path.to_string()
    } else {
        file_path
    };
    if file_path.is_empty() {
        return String::new();
    }
    let path = PathBuf::from(file_path);
    if path.is_file() {
        fs::read_to_string(path)
            .unwrap_or_default()
            .trim()
            .to_string()
    } else {
        String::new()
    }
}

pub fn load_discord_bot_token() -> Result<String> {
    let token = read_secret_value(
        "OPENCLAW_DISCORD_BOT_TOKEN",
        "OPENCLAW_DISCORD_BOT_TOKEN_FILE",
        "/run/secrets/openclaw_discord_bot_token",
    );
    if token.is_empty() {
        Err(discord_tool_error("OPENCLAW_DISCORD_BOT_TOKEN is not set"))
    } else {
        Ok(token)
    }
}

pub fn has_discord_bot_token() -> bool {
    load_discord_bot_token().is_ok()
}

pub fn discord_request(
    method: &str,
    path: &str,
    json_body: Option<&Value>,
    params: Option<&BTreeMap<String, String>>,
    token: Option<&str>,
    timeout_seconds: u64,
) -> Result<Value> {
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(timeout_seconds))
        .build()
        .context("failed to create Discord HTTP client")?;
    let method_value = reqwest::Method::from_bytes(method.as_bytes())
        .with_context(|| format!("invalid HTTP method {method}"))?;
    let resolved_token = match token {
        Some(value) => value.to_string(),
        None => load_discord_bot_token()?,
    };
    let mut request = client
        .request(method_value, format!("{}{}", discord_api_base(), path))
        .header("Authorization", format!("Bot {resolved_token}"))
        .header("Content-Type", "application/json");
    if let Some(body) = json_body {
        request = request.json(body);
    }
    if let Some(query) = params {
        request = request.query(query);
    }
    let response = request.send()?;
    if response.status().as_u16() == 204 {
        return Ok(Value::Null);
    }
    let status = response.status();
    let text = response.text().unwrap_or_default();
    if !status.is_success() {
        let detail = text.split_whitespace().collect::<Vec<_>>().join(" ");
        return Err(discord_tool_error(format!(
            "discord api {method} {path} failed ({}): {}",
            status.as_u16(),
            detail.chars().take(500).collect::<String>()
        )));
    }
    if text.trim().is_empty() {
        Ok(Value::Null)
    } else {
        serde_json::from_str(&text).context("Discord API returned invalid JSON")
    }
}

pub fn get_channel(channel_id: &str) -> Result<Value> {
    let payload = discord_request(
        "GET",
        &format!("/channels/{channel_id}"),
        None,
        None,
        None,
        30,
    )?;
    Ok(if payload.is_object() {
        payload
    } else {
        Value::Object(Default::default())
    })
}

pub fn maybe_unarchive_thread(thread_id: &str) -> Result<()> {
    let channel = get_channel(thread_id)?;
    if channel
        .get("thread_metadata")
        .and_then(|value| value.get("archived"))
        .and_then(Value::as_bool)
        == Some(true)
    {
        let body = serde_json::json!({"archived": false, "locked": false});
        discord_request(
            "PATCH",
            &format!("/channels/{thread_id}"),
            Some(&body),
            None,
            None,
            30,
        )?;
    }
    Ok(())
}

pub fn send_message(channel_id: &str, content: &str) -> Result<Value> {
    let body = serde_json::json!({"content": content});
    let payload = discord_request(
        "POST",
        &format!("/channels/{channel_id}/messages"),
        Some(&body),
        None,
        None,
        30,
    )?;
    Ok(if payload.is_object() {
        payload
    } else {
        Value::Object(Default::default())
    })
}

pub fn edit_message(channel_id: &str, message_id: &str, content: &str) -> Result<Value> {
    let body = serde_json::json!({"content": content});
    let payload = discord_request(
        "PATCH",
        &format!("/channels/{channel_id}/messages/{message_id}"),
        Some(&body),
        None,
        None,
        30,
    )?;
    Ok(if payload.is_object() {
        payload
    } else {
        Value::Object(Default::default())
    })
}

pub fn delete_message(channel_id: &str, message_id: &str) -> Result<()> {
    discord_request(
        "DELETE",
        &format!("/channels/{channel_id}/messages/{message_id}"),
        None,
        None,
        None,
        30,
    )?;
    Ok(())
}

pub fn list_active_guild_threads(guild_id: &str) -> Result<Vec<Value>> {
    let payload = discord_request(
        "GET",
        &format!("/guilds/{guild_id}/threads/active"),
        None,
        None,
        None,
        30,
    )?;
    Ok(payload
        .get("threads")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(Value::is_object)
        .collect())
}

pub fn list_public_archived_threads(channel_id: &str) -> Result<Vec<Value>> {
    let mut threads = Vec::new();
    let mut before = String::new();
    loop {
        let mut params = BTreeMap::from([("limit".to_string(), "100".to_string())]);
        if !before.is_empty() {
            params.insert("before".to_string(), before.clone());
        }
        let archived = discord_request(
            "GET",
            &format!("/channels/{channel_id}/threads/archived/public"),
            None,
            Some(&params),
            None,
            30,
        )?;
        let batch: Vec<Value> = archived
            .get("threads")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(Value::is_object)
            .collect();
        threads.extend(batch.iter().cloned());
        if archived.get("has_more").and_then(Value::as_bool) != Some(true) || batch.is_empty() {
            break;
        }
        let cursor = batch
            .last()
            .and_then(|thread| thread.get("thread_metadata"))
            .and_then(|metadata| metadata.get("archive_timestamp"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        if cursor.is_empty() || cursor == before {
            break;
        }
        before = cursor;
    }
    Ok(threads)
}

pub fn list_forum_threads(forum_channel_id: &str, include_archived: bool) -> Result<Vec<Value>> {
    let mut threads = Vec::new();
    let forum_channel = get_channel(forum_channel_id)?;
    let guild_id = string_field(&forum_channel, "guild_id");
    if !guild_id.is_empty() {
        threads.extend(
            list_active_guild_threads(&guild_id)?
                .into_iter()
                .filter(|thread| string_field(thread, "parent_id") == forum_channel_id),
        );
    }
    if include_archived {
        threads.extend(list_public_archived_threads(forum_channel_id)?);
    }
    let mut deduped = BTreeMap::<String, Value>::new();
    for thread in threads {
        let thread_id = string_field(&thread, "id");
        if !thread_id.is_empty() {
            deduped.insert(thread_id, thread);
        }
    }
    Ok(deduped.into_values().collect())
}

pub fn list_guild_channels(guild_id: &str) -> Result<Vec<Value>> {
    let payload = discord_request(
        "GET",
        &format!("/guilds/{guild_id}/channels"),
        None,
        None,
        None,
        30,
    )?;
    Ok(payload
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(Value::is_object)
        .collect())
}

pub fn iter_channel_messages(channel_id: &str, page_limit: usize) -> Result<Vec<Value>> {
    let mut messages = Vec::new();
    let mut before = String::new();
    let limit = page_limit.clamp(1, 100);
    loop {
        let mut params = BTreeMap::from([("limit".to_string(), limit.to_string())]);
        if !before.is_empty() {
            params.insert("before".to_string(), before.clone());
        }
        let payload = discord_request(
            "GET",
            &format!("/channels/{channel_id}/messages"),
            None,
            Some(&params),
            None,
            30,
        )?;
        let batch: Vec<Value> = payload
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(Value::is_object)
            .collect();
        if batch.is_empty() {
            break;
        }
        messages.extend(batch.iter().cloned());
        if batch.len() < limit {
            break;
        }
        let cursor = batch
            .last()
            .map(|message| string_field(message, "id"))
            .unwrap_or_default();
        if cursor.is_empty() || cursor == before {
            break;
        }
        before = cursor;
    }
    Ok(messages)
}

pub fn string_field(value: &Value, key: &str) -> String {
    match value.get(key) {
        Some(Value::String(text)) => text.trim().to_string(),
        Some(Value::Number(number)) => number.to_string(),
        Some(Value::Bool(boolean)) => boolean.to_string(),
        _ => String::new(),
    }
}
