use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Datelike, Utc};
use chrono_tz::Tz;
use regex::Regex;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use url::Url;

use crate::Result;
use crate::adapters::discord::api::has_discord_bot_token;
use crate::errors::discord_tool_error;

pub const VOICE_BINDING_PREFIX: &str = "managed:discord-voice:";
pub const CONTROL_BINDING_PREFIX: &str = "managed:discord-control:";
pub const MESSAGE_CHUNK_LIMIT: usize = 1800;

pub fn durable_dir() -> PathBuf {
    PathBuf::from(
        env::var("CLANKCORD_DURABLE_DIR").unwrap_or_else(|_| "/clankcord/durable".to_string()),
    )
}

pub fn state_dir() -> PathBuf {
    PathBuf::from(
        env::var("CLANKCORD_STATE_DIR")
            .unwrap_or_else(|_| "/clankcord/state/voice-pool".to_string()),
    )
}

pub fn config_path() -> PathBuf {
    PathBuf::from(env::var("CLANKCORD_CONFIG_PATH").unwrap_or_else(|_| {
        durable_dir()
            .join("config")
            .join("voice-pool.json")
            .display()
            .to_string()
    }))
}

pub fn rooms_path() -> PathBuf {
    durable_dir()
        .join("config")
        .join("discord-voice")
        .join("rooms.json")
}

pub fn control_config_path() -> PathBuf {
    durable_dir().join("config").join("discord-control.json")
}

pub fn tokens_path() -> PathBuf {
    PathBuf::from(
        env::var("CLANKCORD_BOT_TOKENS_PATH")
            .unwrap_or_else(|_| state_dir().join("bot_tokens.txt").display().to_string()),
    )
}

pub fn room_controls_path() -> PathBuf {
    PathBuf::from(
        env::var("CLANKCORD_ROOM_CONTROLS_PATH")
            .unwrap_or_else(|_| state_dir().join("room-controls.json").display().to_string()),
    )
}

pub fn read_json(path: &Path, fallback: Value) -> Value {
    if !path.is_file() {
        return fallback;
    }
    match fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
    {
        Some(value) => value,
        None => fallback,
    }
}

pub fn write_json(path: &Path, payload: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(payload)? + "\n";
    fs::write(path, text)?;
    Ok(())
}

pub fn sha256_text(content: &str) -> String {
    format!("{:x}", Sha256::digest(content.as_bytes()))
}

pub fn slugify(value: &str) -> String {
    let lower = value.to_lowercase();
    let non_slug = Regex::new(r"[^a-z0-9]+").expect("valid slug regex");
    let multi_dash = Regex::new(r"-{2,}").expect("valid slug regex");
    let slug = non_slug.replace_all(&lower, "-");
    multi_dash
        .replace_all(slug.trim_matches('-'), "-")
        .to_string()
}

pub fn local_tz() -> Tz {
    env::var("CLANKCORD_TZ")
        .unwrap_or_else(|_| "UTC".to_string())
        .parse::<Tz>()
        .unwrap_or(chrono_tz::UTC)
}

pub fn format_timestamp_local(value: DateTime<Utc>, tz: Tz) -> BTreeMap<String, String> {
    let local = value.with_timezone(&tz);
    let unix = value.timestamp();
    BTreeMap::from([
        (
            "iso".to_string(),
            value.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        ),
        ("local_iso".to_string(), local.to_rfc3339()),
        ("discord_full".to_string(), format!("<t:{unix}:F>")),
        ("discord_relative".to_string(), format!("<t:{unix}:R>")),
        ("discord_short_time".to_string(), format!("<t:{unix}:T>")),
        (
            "display_date".to_string(),
            local.format("%Y-%m-%d").to_string(),
        ),
        (
            "display_time".to_string(),
            local.format("%H:%M:%S").to_string(),
        ),
        (
            "display_minute".to_string(),
            local.format("%H:%M").to_string(),
        ),
        (
            "display_started".to_string(),
            local.format("%Y-%m-%d %H:%M:%S %Z").to_string(),
        ),
        ("hour_slug".to_string(), local.format("%H").to_string()),
        ("minute_slug".to_string(), local.format("%H-%M").to_string()),
        (
            "day_path".to_string(),
            format!(
                "{:04}/{:02}/{:02}",
                local.year(),
                local.month(),
                local.day()
            ),
        ),
    ])
}

pub fn split_message_chunks(content: &str, limit: usize) -> Vec<String> {
    let normalized = content.trim();
    if normalized.is_empty() {
        return Vec::new();
    }
    let mut chunks = Vec::new();
    let mut current = String::new();
    for line in normalized.lines() {
        let line_text = format!("{line}\n");
        if line_text.len() > limit {
            if !current.is_empty() {
                chunks.push(current.trim_end().to_string());
                current.clear();
            }
            let mut start = 0;
            while start < line_text.len() {
                let end = (start + limit).min(line_text.len());
                chunks.push(line_text[start..end].trim_end().to_string());
                start = end;
            }
            continue;
        }
        if !current.is_empty() && current.len() + line_text.len() > limit {
            chunks.push(current.trim_end().to_string());
            current.clear();
        }
        current.push_str(&line_text);
    }
    if !current.is_empty() {
        chunks.push(current.trim_end().to_string());
    }
    chunks
}

pub fn load_rooms_payload() -> Value {
    match read_json(&rooms_path(), serde_json::json!({"rooms": []})) {
        Value::Object(map) => Value::Object(map),
        _ => serde_json::json!({"rooms": []}),
    }
}

pub fn load_control_config() -> Value {
    match read_json(&control_config_path(), Value::Object(Map::new())) {
        Value::Object(map) => Value::Object(map),
        _ => Value::Object(Map::new()),
    }
}

pub fn iter_valid_voice_rooms() -> Vec<Value> {
    let payload = load_rooms_payload();
    payload
        .get("rooms")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|room| {
            let mut map = room.as_object()?.clone();
            let guild_id = string_value(map.get("guildId")).trim().to_string();
            let channel_id = string_value(map.get("channelId")).trim().to_string();
            if guild_id.is_empty() || channel_id.is_empty() {
                return None;
            }
            let channel_name = non_empty(
                string_value(map.get("channelName")),
                non_empty(string_value(map.get("channelSlug")), channel_id.clone()),
            );
            let channel_slug =
                non_empty(string_value(map.get("channelSlug")), slugify(&channel_name));
            map.insert("guildId".to_string(), Value::String(guild_id.clone()));
            map.insert("channelId".to_string(), Value::String(channel_id));
            map.insert(
                "id".to_string(),
                Value::String(string_value(map.get("id")).trim().to_string()),
            );
            map.insert("channelSlug".to_string(), Value::String(channel_slug));
            map.insert("channelName".to_string(), Value::String(channel_name));
            map.insert(
                "guildSlug".to_string(),
                Value::String(non_empty(
                    string_value(map.get("guildSlug")),
                    slugify(&guild_id),
                )),
            );
            map.insert(
                "accountId".to_string(),
                Value::String(non_empty(
                    string_value(map.get("accountId")),
                    "default".to_string(),
                )),
            );
            Some(Value::Object(map))
        })
        .collect()
}

pub fn synthesize_room(identifier: &str) -> Result<Value> {
    let mut raw = identifier.trim().to_string();
    if raw.starts_with("<#") && raw.ends_with('>') {
        raw = raw[2..raw.len() - 1].trim().to_string();
    }
    if raw.is_empty() {
        return Err(discord_tool_error(
            "room is ambiguous; specify a room name or channel id",
        ));
    }
    let control = load_control_config();
    let guild_id = string_field(&control, "guildId");
    let guild_slug = non_empty(
        string_field(&control, "guildSlug"),
        slugify(if guild_id.is_empty() {
            "discord"
        } else {
            &guild_id
        }),
    );
    let channel_id = if raw.chars().all(|ch| ch.is_ascii_digit()) {
        raw.clone()
    } else {
        String::new()
    };
    let channel_slug = non_empty(
        slugify(&raw),
        if !channel_id.is_empty() {
            format!("channel-{channel_id}")
        } else {
            raw.to_lowercase()
        },
    );
    Ok(serde_json::json!({
        "id": raw,
        "guildId": guild_id,
        "guildSlug": guild_slug,
        "channelId": channel_id,
        "channelSlug": channel_slug,
        "channelName": raw,
        "accountId": "default"
    }))
}

pub fn find_room(identifier: Option<&str>) -> Result<Value> {
    let rooms = iter_valid_voice_rooms();
    let mut wanted = identifier.unwrap_or("").trim().to_lowercase();
    if wanted.is_empty() {
        wanted = string_field(&load_control_config(), "defaultVoiceRoomId").to_lowercase();
    }
    if wanted.is_empty() {
        if rooms.len() == 1 {
            return Ok(rooms[0].clone());
        }
        return Err(discord_tool_error("room is required"));
    }
    let exact_matches: Vec<Value> = rooms
        .iter()
        .filter(|room| {
            ["id", "channelId", "channelSlug", "channelName"]
                .iter()
                .any(|key| string_field(room, key).to_lowercase() == wanted)
        })
        .cloned()
        .collect();
    if exact_matches.len() == 1 {
        return Ok(exact_matches[0].clone());
    }
    if exact_matches.len() > 1 {
        return Err(discord_tool_error(format!(
            "room is ambiguous: {}",
            identifier.unwrap_or("")
        )));
    }
    let prefix_matches: Vec<Value> = rooms
        .iter()
        .filter(|room| {
            ["id", "channelSlug", "channelName"]
                .iter()
                .any(|key| string_field(room, key).to_lowercase().starts_with(&wanted))
        })
        .cloned()
        .collect();
    if prefix_matches.len() == 1 {
        return Ok(prefix_matches[0].clone());
    }
    if prefix_matches.len() > 1 {
        return Err(discord_tool_error(format!(
            "room is ambiguous: {}",
            identifier.unwrap_or("")
        )));
    }
    synthesize_room(identifier.unwrap_or(""))
}

pub fn derive_stt_base_url(ollama_base_url: &str) -> String {
    let Ok(mut parsed) = Url::parse(ollama_base_url.trim()) else {
        return String::new();
    };
    if parsed.host_str().unwrap_or("").is_empty() {
        return String::new();
    }
    let port = match parsed.port() {
        None | Some(11434) => 8080,
        Some(value) => value,
    };
    if parsed.set_port(Some(port)).is_err() {
        return String::new();
    }
    parsed.set_path("/v1");
    parsed.set_query(None);
    parsed.set_fragment(None);
    parsed.to_string().trim_end_matches('/').to_string()
}

pub fn load_stt_base_url() -> Result<String> {
    let explicit = env::var("CLANKCORD_STT_BASE_URL").unwrap_or_default();
    let explicit = explicit.trim().trim_end_matches('/').to_string();
    if !explicit.is_empty() {
        return Ok(explicit);
    }
    let derived = derive_stt_base_url(&env::var("CLANKCORD_OLLAMA_BASE_URL").unwrap_or_default());
    if !derived.is_empty() {
        return Ok(derived);
    }
    Err(discord_tool_error("CLANKCORD_STT_BASE_URL is not set"))
}

pub fn has_stt_configuration() -> bool {
    load_stt_base_url().is_ok()
}

pub fn control_binding(control_config: &Value) -> Option<Value> {
    let guild_id = string_field(control_config, "guildId");
    let bots_channel_id = string_field(control_config, "botsChannelId");
    if guild_id.is_empty() || bots_channel_id.is_empty() {
        return None;
    }
    Some(serde_json::json!({
        "type": "route",
        "agentId": "control-plane",
        "comment": format!("{CONTROL_BINDING_PREFIX}bots"),
        "match": {
            "channel": "discord",
            "guildId": guild_id,
            "peer": {"kind": "channel", "id": bots_channel_id}
        }
    }))
}

pub fn merge_bindings(existing_bindings: &[Value], control_config: &Value) -> Vec<Value> {
    let mut merged: Vec<Value> = existing_bindings
        .iter()
        .filter(|binding| {
            let comment = string_field(binding, "comment");
            !comment.starts_with(VOICE_BINDING_PREFIX)
                && !comment.starts_with(CONTROL_BINDING_PREFIX)
        })
        .cloned()
        .collect();
    if let Some(control) = control_binding(control_config) {
        merged.push(control);
    }
    merged
}

pub fn default_autojoin_rooms(valid_rooms: &[Value]) -> Vec<Value> {
    valid_rooms
        .iter()
        .filter(|room| {
            non_empty(string_field(room, "accountId"), "default".to_string()) == "default"
                && room.get("autoJoin").and_then(Value::as_bool) != Some(false)
        })
        .map(|room| serde_json::json!({"guildId": string_field(room, "guildId"), "channelId": string_field(room, "channelId")}))
        .collect()
}

pub fn build_managed_gateway_config(
    base_config: Value,
    valid_rooms: Option<Vec<Value>>,
    control_config: Option<Value>,
    discord_enabled: Option<bool>,
    audio_enabled: Option<bool>,
) -> Value {
    let mut config = base_config.as_object().cloned().unwrap_or_default();
    let valid_rooms = valid_rooms.unwrap_or_else(iter_valid_voice_rooms);
    let control_config = control_config.unwrap_or_else(load_control_config);
    let discord_enabled = discord_enabled.unwrap_or_else(has_discord_bot_token);
    let audio_enabled = audio_enabled.unwrap_or_else(has_stt_configuration);
    let voice_enabled = discord_enabled && audio_enabled;

    let mut channels = config
        .remove("channels")
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    let mut discord = channels
        .remove("discord")
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    let mut voice = discord
        .remove("voice")
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    voice.insert("enabled".to_string(), Value::Bool(voice_enabled));
    voice.insert(
        "autoJoin".to_string(),
        Value::Array(default_autojoin_rooms(&valid_rooms)),
    );
    discord.insert("enabled".to_string(), Value::Bool(discord_enabled));
    let existing_guilds = discord
        .remove("guilds")
        .unwrap_or_else(|| serde_json::json!({}));
    discord.insert(
        "guilds".to_string(),
        merge_guild_channels(existing_guilds, &valid_rooms, &control_config),
    );
    discord.insert("voice".to_string(), Value::Object(voice));
    channels.insert("discord".to_string(), Value::Object(discord));
    config.insert("channels".to_string(), Value::Object(channels));

    let mut tools = config
        .remove("tools")
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    let mut media = tools
        .remove("media")
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    let mut audio = media
        .remove("audio")
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    audio.insert("enabled".to_string(), Value::Bool(audio_enabled));
    media.insert("audio".to_string(), Value::Object(audio));
    tools.insert("media".to_string(), Value::Object(media));
    config.insert("tools".to_string(), Value::Object(tools));

    let existing_bindings = config
        .remove("bindings")
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_default();
    config.insert(
        "bindings".to_string(),
        Value::Array(merge_bindings(&existing_bindings, &control_config)),
    );
    Value::Object(config)
}

pub fn merge_guild_channels(
    existing_guilds: Value,
    valid_rooms: &[Value],
    control_config: &Value,
) -> Value {
    let mut merged = existing_guilds.as_object().cloned().unwrap_or_default();
    for room in valid_rooms {
        let guild_id = string_field(room, "guildId");
        let channel_id = string_field(room, "channelId");
        if guild_id.is_empty() || channel_id.is_empty() {
            continue;
        }
        let mut guild_entry = merged
            .remove(&guild_id)
            .and_then(|value| value.as_object().cloned())
            .unwrap_or_default();
        let guild_slug = string_field(room, "guildSlug");
        if !guild_slug.is_empty() {
            guild_entry.insert("slug".to_string(), Value::String(guild_slug));
        }
        let mut channels = guild_entry
            .remove("channels")
            .and_then(|value| value.as_object().cloned())
            .unwrap_or_default();
        let mut channel_entry = channels
            .remove(&channel_id)
            .and_then(|value| value.as_object().cloned())
            .unwrap_or_default();
        channel_entry.remove("allow");
        channel_entry.insert("enabled".to_string(), Value::Bool(true));
        channels.insert(channel_id, Value::Object(channel_entry));
        guild_entry.insert("channels".to_string(), Value::Object(channels));
        merged.insert(guild_id, Value::Object(guild_entry));
    }

    let control_guild_id = string_field(control_config, "guildId");
    if !control_guild_id.is_empty() {
        let mut guild_entry = merged
            .remove(&control_guild_id)
            .and_then(|value| value.as_object().cloned())
            .unwrap_or_default();
        let guild_slug = string_field(control_config, "guildSlug");
        if !guild_slug.is_empty() {
            guild_entry.insert("slug".to_string(), Value::String(guild_slug));
        }
        let mut channels = guild_entry
            .remove("channels")
            .and_then(|value| value.as_object().cloned())
            .unwrap_or_default();
        for key in ["botsChannelId", "transcriptsForumId"] {
            let channel_id = string_field(control_config, key);
            if channel_id.is_empty() {
                continue;
            }
            let mut channel_entry = channels
                .remove(&channel_id)
                .and_then(|value| value.as_object().cloned())
                .unwrap_or_default();
            channel_entry.remove("allow");
            channel_entry.insert("enabled".to_string(), Value::Bool(true));
            if key == "botsChannelId" {
                channel_entry.insert("requireMention".to_string(), Value::Bool(false));
            }
            channels.insert(channel_id, Value::Object(channel_entry));
        }
        guild_entry.insert("channels".to_string(), Value::Object(channels));
        merged.insert(control_guild_id, Value::Object(guild_entry));
    }
    Value::Object(merged)
}

pub fn string_value(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.trim().to_string(),
        Some(Value::Number(number)) => number.to_string(),
        Some(Value::Bool(boolean)) => boolean.to_string(),
        _ => String::new(),
    }
}

pub fn string_field(value: &Value, key: &str) -> String {
    string_value(value.get(key))
}

pub fn non_empty(value: String, fallback: String) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        fallback
    } else {
        trimmed.to_string()
    }
}
