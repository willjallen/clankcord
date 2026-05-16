mod agent_sessions;
mod events;
mod jobs;
mod maintenance;
mod members;
mod room_controls;
mod runtime_config;
mod transcripts;
mod voice_state;

use crate::config;

use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

pub use jobs::JobVisibility;

pub(crate) use std::collections::{BTreeMap, BTreeSet};
pub(crate) use std::fs;
pub(crate) use std::path::{Path, PathBuf};

pub(crate) use anyhow::Context;
pub(crate) use chrono::{DateTime, SecondsFormat, Utc};
pub(crate) use regex::Regex;
pub(crate) use serde_json::{Map, Value};
pub(crate) use sqlx::postgres::PgRow;
pub(crate) use sqlx::{Postgres, QueryBuilder, Row as SqlxRow};

pub(crate) use crate::Result;
pub(crate) use crate::runtime::Job;
pub(crate) use crate::runtime::util::{first_value_string, non_empty, slugify, string_field};

pub(crate) use super::util::{
    SPEECH_KINDS, compact_timeline_payload, event_ended_ms, event_started_ms, excerpt,
    first_string, json_value, round3, set, set_default_string, sorted_unique, string_field_map,
    timeline_event_payload, update_value_object,
};
pub use super::util::{
    event_end, event_speaker, event_start, event_text, format_timestamp_local, instant_ms_dt,
    instant_ms_str, isoformat_z, ms_to_datetime, new_id, overlaps, parse_duration, parse_instant,
    read_json_file, read_wav_mono, resolve_time_reference, sha256_file, utc_now, write_json_file,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenderedTranscript {
    pub window: Value,
    pub events: Vec<Value>,
    pub spans: Vec<Value>,
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct SpeechEventInput {
    pub guild_id: String,
    pub guild_slug: String,
    pub voice_channel_id: String,
    pub voice_channel_name: String,
    pub voice_channel_slug: String,
    pub capture_run_id: String,
    pub voice_bot_id: String,
    pub voice_bot_discord_user_id: String,
    pub speaker_user_id: String,
    pub speaker_label: String,
    pub speaker_username: String,
    pub segment_start_time: DateTime<Utc>,
    pub segment_end_time: DateTime<Utc>,
    pub text_draft: String,
    pub source_audio_path: PathBuf,
    pub audio_checksum: String,
    pub segment_index: i64,
    pub duration_ms: i64,
    pub stt_provider: String,
    pub stt_model: String,
    pub stt_metadata: Value,
    pub wake_metadata: Value,
}

impl Default for SpeechEventInput {
    fn default() -> Self {
        Self {
            guild_id: String::new(),
            guild_slug: String::new(),
            voice_channel_id: String::new(),
            voice_channel_name: String::new(),
            voice_channel_slug: String::new(),
            capture_run_id: String::new(),
            voice_bot_id: String::new(),
            voice_bot_discord_user_id: String::new(),
            speaker_user_id: String::new(),
            speaker_label: String::new(),
            speaker_username: String::new(),
            segment_start_time: utc_now(),
            segment_end_time: utc_now(),
            text_draft: String::new(),
            source_audio_path: PathBuf::new(),
            audio_checksum: String::new(),
            segment_index: 0,
            duration_ms: 0,
            stt_provider: "local".to_string(),
            stt_model: String::new(),
            stt_metadata: Value::Object(Map::new()),
            wake_metadata: Value::Object(Map::new()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct CaptureRunInput {
    pub guild_id: String,
    pub guild_slug: String,
    pub voice_channel_id: String,
    pub voice_channel_name: String,
    pub voice_channel_slug: String,
    pub voice_bot_id: String,
    pub voice_bot_discord_user_id: String,
    pub started_at: Option<DateTime<Utc>>,
    pub mode: String,
    pub reason: String,
    pub retention_policy: Option<Value>,
}

impl Default for CaptureRunInput {
    fn default() -> Self {
        Self {
            guild_id: String::new(),
            guild_slug: String::new(),
            voice_channel_id: String::new(),
            voice_channel_name: String::new(),
            voice_channel_slug: String::new(),
            voice_bot_id: String::new(),
            voice_bot_discord_user_id: String::new(),
            started_at: None,
            mode: "local_buffering".to_string(),
            reason: "explicit_request".to_string(),
            retention_policy: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TimelineStore {
    pub root: PathBuf,
    pub database_url: String,
    pub pool: PgPool,
}

impl TimelineStore {
    pub fn new(root: Option<PathBuf>) -> Result<Self> {
        Self::new_with_database(root, config::database_url()?, config::database_schema())
    }

    pub fn new_with_database(
        root: Option<PathBuf>,
        database_url: String,
        database_schema: String,
    ) -> Result<Self> {
        let root = root.unwrap_or_else(config::voice_memory_root);
        fs::create_dir_all(&root)?;
        let mut pool_options = PgPoolOptions::new().max_connections(config::database_pool_size());
        if !database_schema.trim().is_empty() && database_schema != "public" {
            let schema = quote_identifier(&database_schema);
            pool_options = pool_options.after_connect(move |connection, _metadata| {
                let statement = format!("SET search_path TO {schema}");
                Box::pin(async move {
                    sqlx::query(&statement).execute(connection).await?;
                    Ok(())
                })
            });
        }
        let store = Self {
            root,
            database_url: database_url.clone(),
            pool: pool_options.connect_lazy(&database_url)?,
        };
        Ok(store)
    }

    pub fn pool_dir(&self) -> PathBuf {
        self.root.join("pool")
    }

    pub fn ephemeral_dir(&self) -> PathBuf {
        self.root.join("ephemeral")
    }

    pub fn durable_publications_dir(&self) -> PathBuf {
        self.root.join("durable").join("publications")
    }

    pub fn guild_dir(&self, guild_id: &str) -> PathBuf {
        self.ephemeral_dir().join(format!("guild-{guild_id}"))
    }

    pub fn channel_dir(&self, guild_id: &str, voice_channel_id: &str) -> PathBuf {
        self.guild_dir(guild_id)
            .join(format!("channel-{voice_channel_id}"))
    }

    pub fn source_audio_path(
        &self,
        guild_id: &str,
        voice_channel_id: &str,
        capture_run_id: &str,
        speaker_user_id: &str,
        segment_id: &str,
        suffix: &str,
    ) -> PathBuf {
        let safe_speaker = {
            let slug = slugify(speaker_user_id);
            if slug.is_empty() {
                speaker_user_id.to_string()
            } else {
                slug
            }
        };
        self.channel_dir(guild_id, voice_channel_id)
            .join("audio")
            .join(capture_run_id)
            .join(format!("speaker-{safe_speaker}"))
            .join(format!("{segment_id}{suffix}"))
    }

    pub(crate) async fn get_payload_by_id(
        &self,
        table: &str,
        id_column: &str,
        id_value: &str,
    ) -> Result<Value> {
        if id_value.is_empty() {
            return Ok(serde_json::json!({}));
        }
        let sql = format!("SELECT payload_json FROM {table} WHERE {id_column} = $1");
        let row = sqlx::query(&sql)
            .bind(id_value)
            .fetch_optional(&self.pool)
            .await?;
        Ok(match row {
            Some(row) => json_value(&row, "payload_json")?,
            None => serde_json::json!({}),
        })
    }
}

fn quote_identifier(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}
