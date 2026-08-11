//! Pure value rendering shared by every view module: dashboard value compaction, job category and duration, and scope-label lookup.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value, json};
use sqlx::{Postgres, QueryBuilder, Row};

use crate::Result;
use crate::domain::Ctx;
use crate::model::job::Job;
use crate::time::parse_instant;

const DASHBOARD_VALUE_MAX_STRING_CHARS: usize = 4000;
const DASHBOARD_VALUE_MAX_ARRAY_ITEMS: usize = 100;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ScopeKey {
    pub(crate) kind: String,
    pub(crate) guild_id: String,
    pub(crate) id: String,
}

pub(crate) fn dashboard_job_value(job: &Job) -> Value {
    let mut value = compact_dashboard_value(job.to_value(), 5);
    if let Value::Object(object) = &mut value {
        let command_kind = job.command_kind();
        if !command_kind.trim().is_empty() {
            object.insert("command_kind".to_string(), json!(command_kind));
        }
    }
    value
}

pub(crate) fn compact_dashboard_event(event: Value) -> Value {
    let compact = compact_dashboard_value(event, 4);
    if let Value::Object(object) = &compact {
        let mut result = Map::new();
        for key in [
            "event_id",
            "event_kind",
            "kind",
            "scope_kind",
            "scope_id",
            "guild_id",
            "guild_slug",
            "voice_channel_id",
            "voice_channel_name",
            "voice_channel_slug",
            "speaker_user_id",
            "speaker_label",
            "speaker_username",
            "created_at",
            "startedAt",
            "endedAt",
            "text",
            "feedback_message",
            "reason",
            "state",
            "quality",
            "job_id",
            "job_kind",
            "command_kind",
            "command_name",
            "options",
            "conversation_id",
            "capture_run_id",
            "segment_index",
            "duration_ms",
            "referenced_message_id",
            "discord_message_id",
            "discord_channel_id",
            "agent_session_id",
        ] {
            if let Some(value) = object.get(key).filter(|value| !value.is_null()) {
                result.insert(key.to_string(), value.clone());
            }
        }
        for key in ["result", "command_result", "command_response"] {
            if let Some(value) = object.get(key).filter(|value| !value.is_null()) {
                result.insert(key.to_string(), compact_dashboard_value(value.clone(), 2));
            }
        }
        return Value::Object(result);
    }
    compact
}

fn compact_dashboard_value(value: Value, depth: usize) -> Value {
    match value {
        Value::Object(object) => {
            if depth == 0 {
                return json!({"truncated": true, "fields": object.len()});
            }
            let mut compact = Map::new();
            for (key, value) in object {
                if omit_dashboard_key(&key) {
                    continue;
                }
                compact.insert(key, compact_dashboard_value(value, depth - 1));
            }
            Value::Object(compact)
        }
        Value::Array(values) => {
            if depth == 0 {
                return json!({"truncated": true, "items": values.len()});
            }
            let original_len = values.len();
            let mut compact = values
                .into_iter()
                .take(DASHBOARD_VALUE_MAX_ARRAY_ITEMS)
                .map(|value| compact_dashboard_value(value, depth - 1))
                .collect::<Vec<_>>();
            if original_len > compact.len() {
                compact.push(json!({"truncated": true, "remaining": original_len - compact.len()}));
            }
            Value::Array(compact)
        }
        Value::String(value) => Value::String(truncate_dashboard_string(value)),
        value => value,
    }
}

fn omit_dashboard_key(key: &str) -> bool {
    matches!(
        key,
        "stt"
            | "wake_metadata"
            | "wakeMetadata"
            | "token_logprobs"
            | "tokenLogprobs"
            | "logprobs"
            | "audio_bytes"
            | "audioBytes"
            | "audio_checksum"
            | "audioChecksum"
            | "source_audio_path"
            | "sourceAudioPath"
            | "local"
            | "artifacts"
    )
}

fn truncate_dashboard_string(value: String) -> String {
    if value.chars().count() <= DASHBOARD_VALUE_MAX_STRING_CHARS {
        return value;
    }
    value
        .chars()
        .take(DASHBOARD_VALUE_MAX_STRING_CHARS)
        .collect::<String>()
        + "...[truncated]"
}

pub(crate) fn dashboard_job_duration_ms(job: &Job) -> i64 {
    let started = job
        .started_at
        .as_deref()
        .and_then(parse_instant)
        .or_else(|| parse_instant(&job.created_at));
    let ended = job
        .completed_at
        .as_deref()
        .and_then(parse_instant)
        .or_else(|| parse_instant(&job.updated_at));
    started
        .zip(ended)
        .map(|(started, ended)| (ended - started).num_milliseconds().max(0))
        .unwrap_or_default()
}

pub(crate) fn dashboard_job_category(kind: &str) -> &'static str {
    match kind.parse::<crate::model::job::JobKind>() {
        Ok(kind) => crate::model::job::spec::spec(kind).dashboard.as_str(),
        Err(_) => "other",
    }
}

pub(crate) async fn dashboard_scope_labels(
    ctx: &Ctx,
    keys: &[ScopeKey],
) -> Result<BTreeMap<ScopeKey, String>> {
    if keys.is_empty() {
        return Ok(BTreeMap::new());
    }
    let wanted = keys.iter().cloned().collect::<BTreeSet<_>>();
    let mut labels = BTreeMap::new();

    let voice_keys = keys
        .iter()
        .filter(|key| key.kind == "voice_channel")
        .cloned()
        .collect::<Vec<_>>();
    if !voice_keys.is_empty() {
        let mut query = QueryBuilder::<Postgres>::new(
            r#"
            SELECT guild_id, voice_channel_id,
                   COALESCE(NULLIF(voice_channel_name, ''), NULLIF(voice_channel_slug, ''), '') AS label
            FROM voice_rooms
            WHERE FALSE
            "#,
        );
        for key in &voice_keys {
            query
                .push(" OR (guild_id = ")
                .push_bind(key.guild_id.clone())
                .push(" AND voice_channel_id = ")
                .push_bind(key.id.clone())
                .push(")");
        }
        for row in query.build().fetch_all(&ctx.store.pool).await? {
            let key = ScopeKey {
                kind: "voice_channel".to_string(),
                guild_id: row.try_get("guild_id")?,
                id: row.try_get("voice_channel_id")?,
            };
            let label: String = row.try_get("label")?;
            if !label.is_empty() {
                labels.insert(key, label);
            }
        }
    }

    let dm_user_ids = keys
        .iter()
        .filter(|key| key.kind == "dm")
        .map(|key| key.id.clone())
        .collect::<BTreeSet<_>>();
    if !dm_user_ids.is_empty() {
        let mut query = QueryBuilder::<Postgres>::new(
            r#"
            SELECT DISTINCT ON (user_id) user_id,
                   COALESCE(NULLIF(display_name, ''), NULLIF(global_name, ''), NULLIF(username, ''), '') AS label
            FROM discord_members
            WHERE user_id IN (
            "#,
        );
        {
            let mut ids = query.separated(", ");
            for user_id in &dm_user_ids {
                ids.push_bind(user_id.clone());
            }
            ids.push_unseparated(")");
        }
        query.push(
            r#"
            ORDER BY user_id, updated_at_ms DESC
            "#,
        );
        for row in query.build().fetch_all(&ctx.store.pool).await? {
            let user_id: String = row.try_get("user_id")?;
            let label: String = row.try_get("label")?;
            if label.is_empty() {
                continue;
            }
            for key in keys
                .iter()
                .filter(|key| key.kind == "dm" && key.id == user_id)
            {
                labels.entry(key.clone()).or_insert_with(|| label.clone());
            }
        }
    }

    let unresolved = wanted
        .iter()
        .filter(|key| !labels.contains_key(*key))
        .cloned()
        .collect::<Vec<_>>();
    if !unresolved.is_empty() {
        let mut query = QueryBuilder::<Postgres>::new(
            r#"
            SELECT DISTINCT ON (scope_kind, guild_id, scope_id)
                   scope_kind, guild_id, scope_id, payload_json
            FROM timeline_events
            WHERE forgotten = FALSE AND (FALSE
            "#,
        );
        push_scope_key_predicates(&mut query, &unresolved, "");
        query.push(
            r#")
            ORDER BY scope_kind, guild_id, scope_id, started_at_ms DESC, sequence DESC
            "#,
        );
        for row in query.build().fetch_all(&ctx.store.pool).await? {
            let key = ScopeKey {
                kind: row.try_get("scope_kind")?,
                guild_id: row.try_get("guild_id")?,
                id: row.try_get("scope_id")?,
            };
            let payload: Value = row.try_get("payload_json")?;
            if let Some(label) = payload_scope_label(&key.kind, &payload) {
                labels.insert(key, label);
            }
        }
    }

    let unresolved = wanted
        .iter()
        .filter(|key| !labels.contains_key(*key))
        .cloned()
        .collect::<Vec<_>>();
    if !unresolved.is_empty() {
        let mut query = QueryBuilder::<Postgres>::new(
            r#"
            SELECT DISTINCT ON (j.scope_kind, j.guild_id, j.scope_id)
                   j.scope_kind, j.guild_id, j.scope_id, p.payload_blob
            FROM jobs j
            JOIN job_payloads p ON p.job_id = j.job_id
            WHERE FALSE
            "#,
        );
        push_scope_key_predicates(&mut query, &unresolved, "j.");
        query.push(
            r#"
            ORDER BY j.scope_kind, j.guild_id, j.scope_id, j.updated_at_ms DESC, j.job_id DESC
            "#,
        );
        for row in query.build().fetch_all(&ctx.store.pool).await? {
            let key = ScopeKey {
                kind: row.try_get("scope_kind")?,
                guild_id: row.try_get("guild_id")?,
                id: row.try_get("scope_id")?,
            };
            let blob: Vec<u8> = row.try_get("payload_blob")?;
            let job = Job::decode(&blob)?;
            if let Some(label) = payload_scope_label(&key.kind, &job.payload_value()) {
                labels.insert(key, label);
            }
        }
    }
    Ok(labels)
}

pub(crate) async fn dashboard_scope_label_batch(
    ctx: &Ctx,
    scopes: &[(String, String, String)],
) -> Result<BTreeMap<(String, String, String), String>> {
    let keys = scopes
        .iter()
        .map(|(kind, guild_id, id)| ScopeKey {
            kind: kind.clone(),
            guild_id: guild_id.clone(),
            id: id.clone(),
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let labels = dashboard_scope_labels(ctx, &keys).await?;
    Ok(keys
        .into_iter()
        .map(|key| {
            let tuple = (key.kind.clone(), key.guild_id.clone(), key.id.clone());
            (tuple, scope_label(&key, labels.get(&key)))
        })
        .collect())
}

fn push_scope_key_predicates(
    query: &mut QueryBuilder<'_, Postgres>,
    keys: &[ScopeKey],
    prefix: &str,
) {
    for key in keys {
        query
            .push(" OR (")
            .push(prefix)
            .push("scope_kind = ")
            .push_bind(key.kind.clone())
            .push(" AND ")
            .push(prefix)
            .push("guild_id = ")
            .push_bind(key.guild_id.clone())
            .push(" AND ")
            .push(prefix)
            .push("scope_id = ")
            .push_bind(key.id.clone())
            .push(")");
    }
}

pub(crate) fn payload_scope_label(scope_kind: &str, payload: &Value) -> Option<String> {
    let keys: &[&str] = match scope_kind {
        "voice_channel" => &[
            "voice_channel_name",
            "channelName",
            "voice_channel_slug",
            "channelSlug",
            "target_room_name",
        ],
        "dm" => &[
            "display_name",
            "member_display_name",
            "global_name",
            "recipient_name",
            "target_user_name",
            "username",
        ],
        "text_channel" => &["channel_name", "channelName", "channel_slug", "channelSlug"],
        "thread" => &["thread_name", "threadName"],
        _ => return None,
    };
    find_payload_label(payload, keys, 0)
}

fn find_payload_label(payload: &Value, keys: &[&str], depth: usize) -> Option<String> {
    if depth > 6 {
        return None;
    }
    match payload {
        Value::Object(object) => {
            for key in keys {
                if let Some(label) = object
                    .get(*key)
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|label| !label.is_empty())
                {
                    return Some(label.to_string());
                }
            }
            object
                .values()
                .find_map(|value| find_payload_label(value, keys, depth + 1))
        }
        Value::Array(values) => values
            .iter()
            .find_map(|value| find_payload_label(value, keys, depth + 1)),
        _ => None,
    }
}

pub(crate) fn scope_label(key: &ScopeKey, candidate: Option<&String>) -> String {
    if let Some(candidate) = candidate.filter(|candidate| !candidate.trim().is_empty()) {
        return match key.kind.as_str() {
            "dm" => format!("Direct message with {}", candidate.trim()),
            _ => candidate.trim().to_string(),
        };
    }
    match key.kind.as_str() {
        "voice_channel" => "Unconfigured voice room".to_string(),
        "dm" => "Direct message".to_string(),
        "text_channel" => "Text channel".to_string(),
        "thread" => "Thread".to_string(),
        "runtime" => "Ctx".to_string(),
        value if !value.is_empty() => value.replace('_', " "),
        _ => "Unknown scope".to_string(),
    }
}
