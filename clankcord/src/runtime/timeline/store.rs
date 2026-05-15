use super::*;

use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

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
        let configured = std::env::var("CLANKCORD_VOICE_MEMORY_ROOT")
            .or_else(|_| std::env::var("VOICE_MEMORY_ROOT"))
            .unwrap_or_default();
        let root = root.unwrap_or_else(|| {
            if !configured.trim().is_empty() {
                PathBuf::from(configured)
            } else {
                durable_dir().join("clankcord").join("voice")
            }
        });
        fs::create_dir_all(&root)?;
        let database_url = database_url();
        let database_schema = database_schema();
        let mut pool_options = PgPoolOptions::new().max_connections(database_pool_size());
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

    pub async fn initialize(&self) -> Result<()> {
        sqlx::query("SELECT 1").execute(&self.pool).await?;
        self.create_tables().await?;
        self.create_indexes().await?;
        Ok(())
    }
}

fn database_url() -> String {
    std::env::var("CLANKCORD_POSTGRES_URL")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .unwrap_or_else(|_| "postgres://clankcord:clankcord@127.0.0.1:54329/clankcord".to_string())
}

fn database_schema() -> String {
    std::env::var("CLANKCORD_POSTGRES_SCHEMA").unwrap_or_else(|_| "public".to_string())
}

fn quote_identifier(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn database_pool_size() -> u32 {
    std::env::var("CLANKCORD_POSTGRES_POOL_SIZE")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(32)
        .clamp(4, 128)
}

impl TimelineStore {
    async fn create_tables(&self) -> Result<()> {
        sqlx::raw_sql(
            r#"
            CREATE TABLE IF NOT EXISTS voice_rooms (
              guild_id TEXT NOT NULL,
              voice_channel_id TEXT NOT NULL,
              guild_slug TEXT NOT NULL DEFAULT '',
              voice_channel_name TEXT NOT NULL DEFAULT '',
              voice_channel_slug TEXT NOT NULL DEFAULT '',
              updated_at_ms BIGINT NOT NULL,
              PRIMARY KEY (guild_id, voice_channel_id)
            );

            CREATE TABLE IF NOT EXISTS runtime_status (
              status_key TEXT PRIMARY KEY,
              updated_at_ms BIGINT NOT NULL,
              payload_json JSONB NOT NULL
            );

            CREATE TABLE IF NOT EXISTS bot_states (
              bot_id TEXT PRIMARY KEY,
              updated_at_ms BIGINT NOT NULL,
              payload_json JSONB NOT NULL
            );

            CREATE TABLE IF NOT EXISTS sessions (
              session_id TEXT PRIMARY KEY,
              guild_id TEXT NOT NULL DEFAULT '',
              voice_channel_id TEXT NOT NULL DEFAULT '',
              bot_id TEXT NOT NULL DEFAULT '',
              capture_run_id TEXT NOT NULL DEFAULT '',
              active BOOLEAN NOT NULL DEFAULT FALSE,
              started_at_ms BIGINT,
              ended_at_ms BIGINT,
              updated_at_ms BIGINT NOT NULL,
              payload_json JSONB NOT NULL
            );

            CREATE TABLE IF NOT EXISTS assignments (
              assignment_id TEXT PRIMARY KEY,
              guild_id TEXT NOT NULL DEFAULT '',
              voice_channel_id TEXT NOT NULL DEFAULT '',
              voice_bot_id TEXT NOT NULL DEFAULT '',
              capture_run_id TEXT NOT NULL DEFAULT '',
              state TEXT NOT NULL DEFAULT '',
              assigned_at_ms BIGINT,
              released_at_ms BIGINT,
              updated_at_ms BIGINT NOT NULL,
              payload_json JSONB NOT NULL
            );

            CREATE TABLE IF NOT EXISTS occupancy (
              guild_id TEXT NOT NULL,
              voice_channel_id TEXT NOT NULL,
              updated_at_ms BIGINT NOT NULL,
              payload_json JSONB NOT NULL,
              PRIMARY KEY (guild_id, voice_channel_id)
            );

            CREATE TABLE IF NOT EXISTS voice_states (
              guild_id TEXT NOT NULL,
              user_id TEXT NOT NULL,
              voice_channel_id TEXT NOT NULL,
              updated_at_ms BIGINT NOT NULL,
              payload_json JSONB NOT NULL,
              PRIMARY KEY (guild_id, user_id)
            );

            CREATE TABLE IF NOT EXISTS discord_member_cache_refreshes (
              guild_id TEXT PRIMARY KEY,
              refreshed_at_ms BIGINT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS discord_members (
              guild_id TEXT NOT NULL,
              user_id TEXT NOT NULL,
              username TEXT NOT NULL DEFAULT '',
              global_name TEXT NOT NULL DEFAULT '',
              nick TEXT NOT NULL DEFAULT '',
              display_name TEXT NOT NULL DEFAULT '',
              normalized_search TEXT NOT NULL DEFAULT '',
              updated_at_ms BIGINT NOT NULL,
              payload_json JSONB NOT NULL,
              PRIMARY KEY (guild_id, user_id)
            );

            CREATE TABLE IF NOT EXISTS capture_runs (
              capture_run_id TEXT PRIMARY KEY,
              guild_id TEXT NOT NULL,
              voice_channel_id TEXT NOT NULL,
              voice_bot_id TEXT NOT NULL DEFAULT '',
              started_at_ms BIGINT,
              ended_at_ms BIGINT,
              state TEXT NOT NULL DEFAULT '',
              mode TEXT NOT NULL DEFAULT '',
              updated_at_ms BIGINT NOT NULL,
              payload_json JSONB NOT NULL
            );

            CREATE TABLE IF NOT EXISTS timeline_events (
              sequence BIGSERIAL PRIMARY KEY,
              event_id TEXT NOT NULL UNIQUE,
              guild_id TEXT NOT NULL,
              voice_channel_id TEXT NOT NULL,
              event_kind TEXT NOT NULL,
              started_at_ms BIGINT,
              ended_at_ms BIGINT,
              created_at_ms BIGINT NOT NULL,
              capture_run_id TEXT NOT NULL DEFAULT '',
              conversation_id TEXT NOT NULL DEFAULT '',
              speaker_user_id TEXT NOT NULL DEFAULT '',
              speaker_label TEXT NOT NULL DEFAULT '',
              text TEXT NOT NULL DEFAULT '',
              forgotten BOOLEAN NOT NULL DEFAULT FALSE,
              payload_json JSONB NOT NULL
            );

            CREATE TABLE IF NOT EXISTS conversations (
              conversation_id TEXT PRIMARY KEY,
              guild_id TEXT NOT NULL,
              voice_channel_id TEXT NOT NULL,
              start_ms BIGINT,
              end_ms BIGINT,
              last_speech_at_ms BIGINT,
              state TEXT NOT NULL DEFAULT '',
              payload_json JSONB NOT NULL
            );

            CREATE TABLE IF NOT EXISTS windows (
              window_id TEXT PRIMARY KEY,
              guild_id TEXT NOT NULL,
              voice_channel_id TEXT NOT NULL,
              start_ms BIGINT,
              end_ms BIGINT,
              payload_json JSONB NOT NULL
            );

            CREATE TABLE IF NOT EXISTS publications (
              publication_id TEXT PRIMARY KEY,
              guild_id TEXT NOT NULL,
              voice_channel_id TEXT NOT NULL,
              window_id TEXT NOT NULL DEFAULT '',
              state TEXT NOT NULL DEFAULT '',
              created_at_ms BIGINT,
              updated_at_ms BIGINT NOT NULL,
              payload_json JSONB NOT NULL
            );

            CREATE TABLE IF NOT EXISTS authoritative_spans (
              span_id TEXT PRIMARY KEY,
              guild_id TEXT NOT NULL,
              voice_channel_id TEXT NOT NULL,
              window_id TEXT NOT NULL DEFAULT '',
              publication_id TEXT NOT NULL DEFAULT '',
              start_ms BIGINT,
              end_ms BIGINT,
              created_at_ms BIGINT NOT NULL,
              payload_json JSONB NOT NULL
            );

            CREATE TABLE IF NOT EXISTS runtime_metadata (
              key TEXT PRIMARY KEY,
              value TEXT NOT NULL,
              updated_at_ms BIGINT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS jobs (
              job_id TEXT PRIMARY KEY,
              guild_id TEXT NOT NULL,
              voice_channel_id TEXT NOT NULL,
              kind TEXT NOT NULL DEFAULT '',
              state TEXT NOT NULL DEFAULT '',
              terminal BOOLEAN NOT NULL DEFAULT FALSE,
              failed BOOLEAN NOT NULL DEFAULT FALSE,
              ephemeral BOOLEAN NOT NULL DEFAULT FALSE,
              cancellable BOOLEAN NOT NULL DEFAULT FALSE,
              lane TEXT NOT NULL DEFAULT '',
              ordering_key TEXT NOT NULL DEFAULT '',
              ready_at_ms BIGINT NOT NULL,
              created_at_ms BIGINT NOT NULL,
              updated_at_ms BIGINT NOT NULL,
              started_at_ms BIGINT,
              completed_at_ms BIGINT,
              gc_after_ms BIGINT,
              root_job_id TEXT NOT NULL DEFAULT '',
              parent_job_id TEXT,
              lineage_depth BIGINT NOT NULL DEFAULT 0,
              requested_by_user_id TEXT NOT NULL DEFAULT '',
              command_kind TEXT NOT NULL DEFAULT '',
              source_job_id TEXT NOT NULL DEFAULT '',
              stream_id TEXT NOT NULL DEFAULT '',
              target_job_id TEXT NOT NULL DEFAULT '',
              speaker_user_id TEXT NOT NULL DEFAULT '',
              segment_end_ms BIGINT
            );

            CREATE TABLE IF NOT EXISTS job_payloads (
              job_id TEXT PRIMARY KEY REFERENCES jobs(job_id) ON DELETE CASCADE,
              payload_blob BYTEA NOT NULL
            );

            CREATE TABLE IF NOT EXISTS job_dependencies (
              parent_job_id TEXT NOT NULL REFERENCES jobs(job_id) ON DELETE CASCADE,
              child_job_id TEXT NOT NULL REFERENCES jobs(job_id) ON DELETE CASCADE,
              dependency_kind TEXT NOT NULL DEFAULT 'required',
              created_at_ms BIGINT NOT NULL,
              resolution_policy TEXT NOT NULL DEFAULT 'parent_resumes',
              PRIMARY KEY (parent_job_id, child_job_id)
            );

            CREATE TABLE IF NOT EXISTS automations (
              automation_id TEXT PRIMARY KEY,
              guild_id TEXT NOT NULL,
              voice_channel_id TEXT NOT NULL,
              state TEXT NOT NULL DEFAULT '',
              idempotency_key TEXT NOT NULL DEFAULT '',
              created_at_ms BIGINT,
              updated_at_ms BIGINT NOT NULL,
              expires_at_ms BIGINT,
              fire_count BIGINT NOT NULL DEFAULT 0,
              max_fires BIGINT,
              payload_blob BYTEA NOT NULL
            );
            "#,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn create_indexes(&self) -> Result<()> {
        sqlx::raw_sql(
            r#"
            CREATE INDEX IF NOT EXISTS idx_timeline_room_time
              ON timeline_events(guild_id, voice_channel_id, started_at_ms, sequence);
            CREATE INDEX IF NOT EXISTS idx_timeline_room_kind_time
              ON timeline_events(guild_id, voice_channel_id, event_kind, started_at_ms, sequence);
            CREATE INDEX IF NOT EXISTS idx_timeline_capture_run_time
              ON timeline_events(capture_run_id, started_at_ms, sequence);
            CREATE INDEX IF NOT EXISTS idx_timeline_conversation_time
              ON timeline_events(conversation_id, started_at_ms, sequence);
            CREATE INDEX IF NOT EXISTS idx_timeline_speaker_time
              ON timeline_events(speaker_user_id, started_at_ms, sequence);
            CREATE INDEX IF NOT EXISTS idx_voice_states_room_updated
              ON voice_states(guild_id, voice_channel_id, updated_at_ms DESC);
            CREATE INDEX IF NOT EXISTS idx_discord_members_guild_normalized
              ON discord_members(guild_id, normalized_search);
            CREATE INDEX IF NOT EXISTS idx_timeline_kind_time
              ON timeline_events(event_kind, started_at_ms, sequence);
            CREATE INDEX IF NOT EXISTS idx_capture_runs_room_time
              ON capture_runs(guild_id, voice_channel_id, started_at_ms, ended_at_ms);
            CREATE INDEX IF NOT EXISTS idx_conversations_room_time
              ON conversations(guild_id, voice_channel_id, start_ms, end_ms);
            CREATE INDEX IF NOT EXISTS idx_spans_room_time
              ON authoritative_spans(guild_id, voice_channel_id, start_ms, end_ms);
            CREATE INDEX IF NOT EXISTS idx_publications_room_state
              ON publications(guild_id, voice_channel_id, state, created_at_ms);
            CREATE INDEX IF NOT EXISTS idx_sessions_active_room
              ON sessions(guild_id, voice_channel_id, active, updated_at_ms DESC);
            CREATE INDEX IF NOT EXISTS idx_jobs_due_kind
              ON jobs(kind, ready_at_ms, created_at_ms, job_id)
              WHERE state = 'queued';
            CREATE INDEX IF NOT EXISTS idx_jobs_active_ordering
              ON jobs(ordering_key)
              WHERE terminal = FALSE AND ordering_key <> '';
            CREATE INDEX IF NOT EXISTS idx_jobs_active_visible_scope
              ON jobs(guild_id, voice_channel_id, updated_at_ms DESC, job_id)
              WHERE terminal = FALSE AND ephemeral = FALSE;
            CREATE INDEX IF NOT EXISTS idx_jobs_recent_visible
              ON jobs(updated_at_ms DESC, job_id)
              WHERE ephemeral = FALSE;
            CREATE INDEX IF NOT EXISTS idx_jobs_failed_visible
              ON jobs(updated_at_ms DESC, job_id)
              WHERE failed = TRUE AND ephemeral = FALSE;
            CREATE INDEX IF NOT EXISTS idx_jobs_scope_state_kind_updated
              ON jobs(guild_id, voice_channel_id, state, kind, updated_at_ms DESC);
            CREATE INDEX IF NOT EXISTS idx_jobs_scope_kind_updated
              ON jobs(guild_id, voice_channel_id, kind, updated_at_ms DESC);
            CREATE INDEX IF NOT EXISTS idx_jobs_kind_updated
              ON jobs(kind, updated_at_ms DESC);
            CREATE INDEX IF NOT EXISTS idx_jobs_state_updated
              ON jobs(state, updated_at_ms DESC);
            CREATE INDEX IF NOT EXISTS idx_jobs_ephemeral_gc
              ON jobs(gc_after_ms, job_id)
              WHERE ephemeral = TRUE AND terminal = TRUE;
            CREATE INDEX IF NOT EXISTS idx_jobs_wake_stream_queued
              ON jobs(stream_id, ready_at_ms, created_at_ms, job_id)
              WHERE kind = 'wake_probe' AND state = 'queued';
            CREATE INDEX IF NOT EXISTS idx_jobs_response_source
              ON jobs(source_job_id, updated_at_ms DESC, job_id)
              WHERE kind = 'response';
            CREATE INDEX IF NOT EXISTS idx_jobs_audio_segment_pending_speaker
              ON jobs(guild_id, voice_channel_id, speaker_user_id, segment_end_ms, job_id)
              WHERE kind = 'audio_segment' AND terminal = FALSE;
            CREATE INDEX IF NOT EXISTS idx_job_dependencies_child
              ON job_dependencies(child_job_id, parent_job_id);
            CREATE INDEX IF NOT EXISTS idx_automations_scope_state
              ON automations(guild_id, voice_channel_id, state, expires_at_ms);
            CREATE INDEX IF NOT EXISTS idx_automations_idempotency
              ON automations(guild_id, voice_channel_id, idempotency_key, state);
            "#,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

impl TimelineStore {
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
}

impl TimelineStore {
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
