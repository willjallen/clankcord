//! Timeline event payload semantics: reading the JSON shape persisted on
//! timeline_events rows, plus the row decoders that produce it.

use chrono::{DateTime, Utc};
use serde_json::{Map, Value};
use sqlx::Row as SqlxRow;
use sqlx::postgres::PgRow;

use crate::Result;
use crate::time::{instant_ms_dt, isoformat_z, ms_to_datetime, parse_instant};
use crate::util::{first_value_string, non_empty, string_field, string_value};

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

pub(crate) fn first_string(map: &Map<String, Value>, keys: &[&str]) -> String {
    keys.iter()
        .map(|key| string_value(map.get(*key)))
        .find(|value| !value.is_empty())
        .unwrap_or_default()
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

pub(crate) fn event_started_ms(payload: &Value) -> Option<i64> {
    event_start(payload).map(instant_ms_dt)
}

pub(crate) fn event_ended_ms(payload: &Value) -> Option<i64> {
    event_end(payload).map(instant_ms_dt)
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

pub fn timeline_event_payload(row: &PgRow) -> Result<Value> {
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
    let started = ms_to_datetime(row.try_get::<i64, _>("started_at_ms")?);
    let ended = ms_to_datetime(row.try_get::<i64, _>("ended_at_ms")?);
    let created = ms_to_datetime(row.try_get::<i64, _>("created_at_ms")?);
    for (key, value) in [
        ("event_id", &event_id),
        ("eventId", &event_id),
        ("event_kind", &kind),
        ("kind", &kind),
        ("scope_kind", &scope_kind),
        ("scopeKind", &scope_kind),
        ("scope_id", &scope_id),
        ("scopeId", &scope_id),
        ("guild_id", &guild_id),
        ("guildId", &guild_id),
    ] {
        payload.insert(key.to_string(), Value::String(value.to_string()));
    }
    if scope_kind == "voice_channel" {
        payload.insert(
            "voice_channel_id".to_string(),
            Value::String(scope_id.clone()),
        );
        payload.insert(
            "voiceChannelId".to_string(),
            Value::String(scope_id.clone()),
        );
        payload.insert("channelId".to_string(), Value::String(scope_id.clone()));
    } else {
        for key in [
            "voice_channel_id",
            "voiceChannelId",
            "channelId",
            "voice_channel_name",
            "channelName",
            "voice_channel_slug",
            "channelSlug",
        ] {
            payload.remove(key);
        }
    }
    if let Ok(value) = row.try_get::<String, _>("room_guild_slug")
        && !value.is_empty()
    {
        set_default_string(&mut payload, "guild_slug", &value);
        set_default_string(&mut payload, "guildSlug", &value);
    }
    if let Ok(value) = row.try_get::<String, _>("room_voice_channel_name")
        && !value.is_empty()
    {
        set_default_string(&mut payload, "voice_channel_name", &value);
        set_default_string(&mut payload, "channelName", &value);
    }
    if let Ok(value) = row.try_get::<String, _>("room_voice_channel_slug")
        && !value.is_empty()
    {
        set_default_string(&mut payload, "voice_channel_slug", &value);
        set_default_string(&mut payload, "channelSlug", &value);
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
