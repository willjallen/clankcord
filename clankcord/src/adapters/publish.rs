use chrono::NaiveDate;
use serde_json::Value;

use crate::Result;
use crate::adapters::discord::api::{
    delete_message, edit_message, maybe_unarchive_thread, send_message,
};
use crate::config::{
    publish_state_path, read_json, split_message_chunks, summary_root, transcript_root, write_json,
};

pub const DEFAULT_FORUM_AUTO_ARCHIVE_MINUTES: i64 = 1440;

pub fn load_publish_state() -> Value {
    let mut state = read_json(
        &publish_state_path(),
        serde_json::json!({"threads": {}, "hour_messages": {}, "summary_messages": {}}),
    );
    if !state.is_object() {
        state = serde_json::json!({});
    }
    let map = state.as_object_mut().unwrap();
    map.entry("threads".to_string())
        .or_insert_with(|| serde_json::json!({}));
    map.entry("hour_messages".to_string())
        .or_insert_with(|| serde_json::json!({}));
    map.entry("summary_messages".to_string())
        .or_insert_with(|| serde_json::json!({}));
    state
}

pub fn save_publish_state(payload: &Value) -> Result<()> {
    write_json(&publish_state_path(), payload)
}

pub fn thread_state_key(room: &Value, day_value: NaiveDate) -> String {
    format!(
        "{}:{}:{}",
        string_field(room, "guildSlug"),
        string_field(room, "channelSlug"),
        day_value
    )
}

pub fn day_directory_for_room(room: &Value, day_value: NaiveDate) -> std::path::PathBuf {
    transcript_root()
        .join(string_field(room, "guildSlug"))
        .join(string_field(room, "channelSlug"))
        .join("sessions")
        .join(day_value.format("%Y/%m/%d").to_string())
}

pub fn summary_directory_for_room(room: &Value, day_value: NaiveDate) -> std::path::PathBuf {
    summary_root()
        .join(string_field(room, "guildSlug"))
        .join(string_field(room, "channelSlug"))
        .join(day_value.format("%Y/%m/%d").to_string())
}

pub fn summary_markdown_path(room: &Value, day_value: NaiveDate) -> std::path::PathBuf {
    summary_directory_for_room(room, day_value).join("summary.md")
}

pub fn sync_message_chunks(
    channel_id: &str,
    chunks: &[String],
    existing_message_ids: &[String],
) -> Result<Vec<String>> {
    let mut message_ids = existing_message_ids.to_vec();
    maybe_unarchive_thread(channel_id)?;
    for (index, chunk) in chunks.iter().enumerate() {
        if index < message_ids.len() {
            edit_message(channel_id, &message_ids[index], chunk)?;
        } else {
            let created = send_message(channel_id, chunk)?;
            message_ids.push(string_field(&created, "id"));
        }
    }
    for extra_id in message_ids.iter().skip(chunks.len()) {
        if !extra_id.is_empty() {
            delete_message(channel_id, extra_id)?;
        }
    }
    Ok(message_ids
        .into_iter()
        .take(chunks.len())
        .filter(|id| !id.is_empty())
        .collect())
}

pub fn split_publish_message_chunks(content: &str) -> Vec<String> {
    split_message_chunks(content, crate::config::MESSAGE_CHUNK_LIMIT)
}

fn string_field(value: &Value, key: &str) -> String {
    match value.get(key) {
        Some(Value::String(text)) => text.trim().to_string(),
        Some(Value::Number(number)) => number.to_string(),
        Some(Value::Bool(boolean)) => boolean.to_string(),
        _ => String::new(),
    }
}
