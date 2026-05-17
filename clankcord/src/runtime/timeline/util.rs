use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use chrono::{DateTime, SecondsFormat, TimeZone, Utc};
use regex::Regex;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use sqlx::Row as SqlxRow;
use sqlx::postgres::PgRow;
use uuid::Uuid;

use crate::Result;
use crate::runtime::util::{first_value_string, non_empty, string_field};

pub(crate) const SPEECH_KINDS: &[&str] = &["speech_segment", "transcript"];

pub(crate) fn set_default_string(payload: &mut Map<String, Value>, key: &str, value: &str) {
    if !payload.contains_key(key) || payload.get(key).is_some_and(value_is_empty) {
        payload.insert(key.to_string(), Value::String(value.to_string()));
    }
}

pub(crate) fn update_value_object<const N: usize>(payload: &mut Value, fields: [(&str, Value); N]) {
    let Some(map) = payload.as_object_mut() else {
        return;
    };
    for (key, value) in fields {
        map.insert(key.to_string(), value);
    }
}

fn value_is_empty(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(text) => text.is_empty(),
        Value::Array(values) => values.is_empty(),
        Value::Object(map) => map.is_empty(),
        _ => false,
    }
}

pub(crate) fn string_field_map(map: &Map<String, Value>, key: &str) -> String {
    match map.get(key) {
        Some(Value::String(text)) => text.trim().to_string(),
        Some(Value::Number(number)) => number.to_string(),
        Some(Value::Bool(boolean)) => boolean.to_string(),
        _ => String::new(),
    }
}

pub(crate) fn first_string(map: &Map<String, Value>, keys: &[&str]) -> String {
    keys.iter()
        .map(|key| string_field_map(map, key))
        .find(|value| !value.is_empty())
        .unwrap_or_default()
}

pub(crate) fn set<const N: usize>(values: [&str; N]) -> BTreeSet<String> {
    values.into_iter().map(ToString::to_string).collect()
}

pub(crate) fn sorted_unique<I>(values: I) -> Vec<String>
where
    I: IntoIterator<Item = String>,
{
    values
        .into_iter()
        .filter(|value| !value.is_empty())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

pub(crate) fn excerpt(content: &str, needle: &str, radius: usize) -> String {
    let lower = content.to_lowercase();
    let Some(index) = lower.find(needle) else {
        return content
            .chars()
            .take(radius * 2)
            .collect::<String>()
            .trim()
            .to_string();
    };
    let start = index.saturating_sub(radius);
    let end = (index + needle.len() + radius).min(content.len());
    content[start..end].trim().to_string()
}

pub(crate) fn round3(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

const ISO_FIELDS: &[&str] = &[
    "segment_start_time",
    "startedAt",
    "start_time",
    "assigned_at",
    "created_at",
    "timestamp",
];
const END_FIELDS: &[&str] = &["segment_end_time", "endedAt", "end_time", "released_at"];

pub fn utc_now() -> DateTime<Utc> {
    Utc::now()
}

pub fn isoformat_z(value: Option<DateTime<Utc>>) -> String {
    value
        .unwrap_or_else(utc_now)
        .to_rfc3339_opts(SecondsFormat::Millis, true)
}

pub fn format_timestamp_local(value: DateTime<Utc>, tz: chrono_tz::Tz) -> BTreeMap<String, String> {
    let local = value.with_timezone(&tz);
    let unix = value.timestamp();
    BTreeMap::from([
        (
            "iso".to_string(),
            value.to_rfc3339_opts(SecondsFormat::Millis, true),
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
                chrono::Datelike::year(&local),
                chrono::Datelike::month(&local),
                chrono::Datelike::day(&local)
            ),
        ),
    ])
}

pub fn parse_instant(raw: &str) -> Option<DateTime<Utc>> {
    let value = raw.trim();
    if value.is_empty() {
        return None;
    }
    let normalized = if let Some(prefix) = value.strip_suffix('Z') {
        format!("{prefix}+00:00")
    } else {
        value.to_string()
    };
    DateTime::parse_from_rfc3339(&normalized)
        .map(|value| value.with_timezone(&Utc))
        .ok()
        .or_else(|| {
            chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S%.f")
                .ok()
                .map(|value| value.and_utc())
        })
}

pub fn parse_duration(raw: &str) -> Option<chrono::Duration> {
    let value = raw.trim().to_lowercase();
    if value.is_empty() {
        return None;
    }
    let regex = Regex::new(r"^(?P<sign>[+-])?\s*(?P<count>\d+(?:\.\d+)?)\s*(?P<unit>ms|s|sec|secs|m|min|mins|h|hr|hrs|d|day|days)$").ok()?;
    let captures = regex.captures(&value)?;
    let count: f64 = captures.name("count")?.as_str().parse().ok()?;
    let sign = if captures.name("sign").map(|m| m.as_str()) == Some("-") {
        -1.0
    } else {
        1.0
    };
    let unit = captures.name("unit")?.as_str();
    let millis = match unit {
        "ms" => count,
        "s" | "sec" | "secs" => count * 1000.0,
        "m" | "min" | "mins" => count * 60_000.0,
        "h" | "hr" | "hrs" => count * 3_600_000.0,
        _ => count * 86_400_000.0,
    };
    Some(chrono::Duration::milliseconds((sign * millis) as i64))
}

pub fn resolve_time_reference(raw: &str, now: Option<DateTime<Utc>>) -> Option<DateTime<Utc>> {
    let value = raw.trim();
    if value.is_empty() {
        return None;
    }
    let current = now.unwrap_or_else(utc_now);
    parse_duration(value)
        .map(|delta| current + delta)
        .or_else(|| parse_instant(value))
}

pub fn event_start(event: &Value) -> Option<DateTime<Utc>> {
    ISO_FIELDS
        .iter()
        .find_map(|field| parse_instant(&string_field(event, field)))
}

pub fn event_end(event: &Value) -> Option<DateTime<Utc>> {
    END_FIELDS
        .iter()
        .find_map(|field| parse_instant(&string_field(event, field)))
        .or_else(|| event_start(event))
}

pub fn overlaps(
    start_a: Option<DateTime<Utc>>,
    end_a: Option<DateTime<Utc>>,
    start_b: Option<DateTime<Utc>>,
    end_b: Option<DateTime<Utc>>,
) -> bool {
    match (start_a, end_a, start_b, end_b) {
        (Some(start_a), Some(end_a), Some(start_b), Some(end_b)) => {
            start_a < end_b && start_b < end_a
        }
        _ => false,
    }
}

pub fn instant_ms_dt(value: DateTime<Utc>) -> i64 {
    value.timestamp_millis()
}

pub fn instant_ms_str(value: Option<&str>) -> Option<i64> {
    parse_instant(value.unwrap_or("")).map(instant_ms_dt)
}

pub fn ms_to_datetime(value: i64) -> Option<DateTime<Utc>> {
    Utc.timestamp_millis_opt(value).single()
}

pub(crate) fn event_started_ms(payload: &Value) -> Option<i64> {
    event_start(payload).map(instant_ms_dt)
}

pub(crate) fn event_ended_ms(payload: &Value) -> Option<i64> {
    event_end(payload).map(instant_ms_dt)
}

pub fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", Uuid::new_v4().simple())
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let mut digest = Sha256::new();
    let mut file = fs::File::open(path)?;
    std::io::copy(&mut file, &mut digest)?;
    Ok(format!("sha256:{:x}", digest.finalize()))
}

pub fn read_json_file(path: &Path, fallback: Value) -> Value {
    if !path.is_file() {
        return fallback;
    }
    fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .unwrap_or(fallback)
}

pub fn write_json_file(path: &Path, payload: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!(
        "{}tmp",
        path.extension()
            .map(|ext| format!("{}.", ext.to_string_lossy()))
            .unwrap_or_default()
    ));
    fs::write(&tmp, serde_json::to_string_pretty(payload)? + "\n")?;
    fs::rename(tmp, path)?;
    Ok(())
}

pub fn read_wav_mono(path: &Path, sample_rate: u32) -> Result<Vec<i16>> {
    let mut reader = hound::WavReader::open(path)?;
    let spec = reader.spec();
    if spec.bits_per_sample != 16 || spec.sample_rate != sample_rate {
        anyhow::bail!(
            "unsupported wav format for {}: {}ch {}bit {}Hz",
            path.display(),
            spec.channels,
            spec.bits_per_sample,
            spec.sample_rate
        );
    }
    let samples = reader
        .samples::<i16>()
        .collect::<std::result::Result<Vec<_>, _>>()?;
    match spec.channels {
        1 => Ok(samples),
        2 => Ok(samples
            .chunks_exact(2)
            .map(|pair| ((pair[0] as i32 + pair[1] as i32) / 2) as i16)
            .collect()),
        count => anyhow::bail!("unsupported channel count for {}: {count}", path.display()),
    }
}

pub fn event_text(event: &Value) -> String {
    non_empty(
        string_field(event, "text_draft"),
        string_field(event, "text"),
    )
    .trim()
    .to_string()
}

pub fn event_speaker(event: &Value) -> String {
    non_empty(
        first_value_string(
            event,
            &[
                "speaker_label",
                "speakerLabel",
                "speaker_user_id",
                "speakerId",
            ],
        ),
        "unknown".to_string(),
    )
}

pub(crate) fn timeline_event_payload(row: &PgRow) -> Result<Value> {
    let payload_json: Value = row.try_get("payload_json")?;
    let mut payload = payload_json.as_object().cloned().unwrap_or_default();
    let event_id: String = row.try_get("event_id")?;
    let kind: String = row.try_get("event_kind")?;
    let scope_kind: String = row.try_get("scope_kind")?;
    let guild_id: String = row.try_get("guild_id")?;
    let scope_id: String = row.try_get("scope_id")?;
    let capture_run_id: String = row.try_get("capture_run_id")?;
    let conversation_id: String = row.try_get("conversation_id")?;
    let speaker_user_id: String = row.try_get("speaker_user_id")?;
    let speaker_label: String = row.try_get("speaker_label")?;
    let text: String = row.try_get("text")?;
    let started = row
        .try_get::<Option<i64>, _>("started_at_ms")?
        .and_then(ms_to_datetime);
    let ended = row
        .try_get::<Option<i64>, _>("ended_at_ms")?
        .and_then(ms_to_datetime);
    let created = row
        .try_get::<Option<i64>, _>("created_at_ms")?
        .and_then(ms_to_datetime);
    set_default_string(&mut payload, "event_id", &event_id);
    set_default_string(&mut payload, "eventId", &event_id);
    set_default_string(&mut payload, "event_kind", &kind);
    set_default_string(&mut payload, "kind", &kind);
    set_default_string(&mut payload, "scope_kind", &scope_kind);
    set_default_string(&mut payload, "scope_id", &scope_id);
    set_default_string(&mut payload, "guild_id", &guild_id);
    set_default_string(&mut payload, "guildId", &guild_id);
    if scope_kind == "voice_channel" {
        set_default_string(&mut payload, "voice_channel_id", &scope_id);
        set_default_string(&mut payload, "channelId", &scope_id);
    }
    if let Ok(value) = row.try_get::<String, _>("room_guild_slug") {
        if !value.is_empty() {
            set_default_string(&mut payload, "guild_slug", &value);
            set_default_string(&mut payload, "guildSlug", &value);
        }
    }
    if let Ok(value) = row.try_get::<String, _>("room_voice_channel_name") {
        if !value.is_empty() {
            set_default_string(&mut payload, "voice_channel_name", &value);
            set_default_string(&mut payload, "channelName", &value);
        }
    }
    if let Ok(value) = row.try_get::<String, _>("room_voice_channel_slug") {
        if !value.is_empty() {
            set_default_string(&mut payload, "voice_channel_slug", &value);
            set_default_string(&mut payload, "channelSlug", &value);
        }
    }
    if !capture_run_id.is_empty() {
        set_default_string(&mut payload, "capture_run_id", &capture_run_id);
        set_default_string(&mut payload, "captureRunId", &capture_run_id);
    }
    if !conversation_id.is_empty() {
        set_default_string(&mut payload, "conversation_id", &conversation_id);
        set_default_string(&mut payload, "conversationId", &conversation_id);
        if SPEECH_KINDS.contains(&kind.as_str()) {
            set_default_string(
                &mut payload,
                "provisional_conversation_id",
                &conversation_id,
            );
        }
    }
    if !speaker_user_id.is_empty() {
        set_default_string(&mut payload, "speaker_user_id", &speaker_user_id);
        set_default_string(&mut payload, "speakerId", &speaker_user_id);
    }
    if !speaker_label.is_empty() {
        set_default_string(&mut payload, "speaker_label", &speaker_label);
        set_default_string(&mut payload, "speakerLabel", &speaker_label);
    }
    if let Some(started) = started {
        set_default_string(
            &mut payload,
            "segment_start_time",
            &isoformat_z(Some(started)),
        );
        set_default_string(&mut payload, "startedAt", &isoformat_z(Some(started)));
    }
    if let Some(ended) = ended {
        set_default_string(&mut payload, "segment_end_time", &isoformat_z(Some(ended)));
        set_default_string(&mut payload, "endedAt", &isoformat_z(Some(ended)));
    }
    if let Some(created) = created {
        set_default_string(&mut payload, "created_at", &isoformat_z(Some(created)));
        set_default_string(&mut payload, "timestamp", &isoformat_z(Some(created)));
    }
    if !text.is_empty() {
        set_default_string(&mut payload, "text_draft", &text);
        set_default_string(&mut payload, "text", &text);
    }
    for (canonical, alias) in [
        ("voice_bot_id", "botId"),
        ("voice_bot_discord_user_id", "botUserId"),
        ("speaker_username", "speakerUsername"),
        ("source_audio_path", "sourceAudioPath"),
        ("audio_checksum", "audioChecksum"),
        ("segment_index", "segmentIndex"),
        ("duration_ms", "durationMs"),
    ] {
        if let Some(value) = payload.get(canonical).cloned() {
            payload.entry(alias.to_string()).or_insert(value);
        }
    }
    let forgotten: bool = row.try_get("forgotten")?;
    if forgotten {
        payload.insert("_forgotten".to_string(), Value::Bool(true));
    }
    Ok(Value::Object(payload))
}

pub(crate) fn json_value(row: &PgRow, column: &str) -> Result<Value> {
    Ok(row.try_get::<Value, _>(column)?)
}

pub(crate) fn compact_timeline_payload(payload: &Value, kind: &str) -> Value {
    let mut compact = payload.as_object().cloned().unwrap_or_default();
    if !SPEECH_KINDS.contains(&kind) {
        return Value::Object(compact);
    }
    for key in [
        "event_id",
        "eventId",
        "event_kind",
        "kind",
        "guild_id",
        "guildId",
        "scope_kind",
        "scope_id",
        "guild_slug",
        "guildSlug",
        "voice_channel_id",
        "channelId",
        "voice_channel_name",
        "channelName",
        "voice_channel_slug",
        "channelSlug",
        "capture_run_id",
        "captureRunId",
        "conversation_id",
        "conversationId",
        "provisional_conversation_id",
        "speaker_user_id",
        "speakerId",
        "speaker_label",
        "speakerLabel",
        "segment_start_time",
        "startedAt",
        "segment_end_time",
        "endedAt",
        "text_draft",
        "text",
        "created_at",
        "timestamp",
    ] {
        compact.remove(key);
    }
    for (alias, canonical) in [
        ("botId", "voice_bot_id"),
        ("botUserId", "voice_bot_discord_user_id"),
        ("speakerUsername", "speaker_username"),
        ("sourceAudioPath", "source_audio_path"),
        ("audioChecksum", "audio_checksum"),
        ("segmentIndex", "segment_index"),
        ("durationMs", "duration_ms"),
    ] {
        compact.remove(alias);
        if compact
            .get(canonical)
            .is_some_and(|value| matches!(value, Value::Null) || value == "" || value == -1)
        {
            compact.remove(canonical);
        }
    }
    compact.retain(|_, value| {
        !matches!(value, Value::Null)
            && value != ""
            && value != &serde_json::json!([])
            && value != &serde_json::json!({})
    });
    Value::Object(compact)
}
