use super::*;

use serde::{Deserialize, Serialize};

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
    pub db_path: PathBuf,
    pub fts_enabled: bool,
}

impl TimelineStore {
    pub fn new(root: Option<PathBuf>) -> Result<Self> {
        let configured = std::env::var("CLAWCORD_VOICE_MEMORY_ROOT")
            .or_else(|_| std::env::var("VOICE_MEMORY_ROOT"))
            .unwrap_or_default();
        let root = root.unwrap_or_else(|| {
            if !configured.trim().is_empty() {
                PathBuf::from(configured)
            } else {
                durable_dir().join("clawcord").join("voice")
            }
        });
        fs::create_dir_all(&root)?;
        let db_path = std::env::var("CLAWCORD_VOICE_SQLITE_PATH")
            .or_else(|_| std::env::var("VOICE_MEMORY_SQLITE_PATH"))
            .ok()
            .filter(|value| !value.trim().is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| root.join("voice.sqlite3"));
        if let Some(parent) = db_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut store = Self {
            root,
            db_path,
            fts_enabled: false,
        };
        store.initialize_database()?;
        Ok(store)
    }

    pub fn connect(&self) -> Result<Connection> {
        let db = Connection::open(&self.db_path)?;
        db.execute_batch("PRAGMA foreign_keys = ON; PRAGMA busy_timeout = 5000;")?;
        Ok(db)
    }
}

impl TimelineStore {
    pub fn initialize_database(&mut self) -> Result<()> {
        let db = self.connect()?;
        db.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;")?;
        db.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS voice_rooms (
              guild_id TEXT NOT NULL,
              voice_channel_id TEXT NOT NULL,
              guild_slug TEXT NOT NULL DEFAULT '',
              voice_channel_name TEXT NOT NULL DEFAULT '',
              voice_channel_slug TEXT NOT NULL DEFAULT '',
              updated_at_ms INTEGER NOT NULL,
              PRIMARY KEY (guild_id, voice_channel_id)
            );

            CREATE TABLE IF NOT EXISTS bot_states (
              bot_id TEXT PRIMARY KEY,
              updated_at_ms INTEGER NOT NULL,
              payload_json TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS assignments (
              assignment_id TEXT PRIMARY KEY,
              guild_id TEXT NOT NULL DEFAULT '',
              voice_channel_id TEXT NOT NULL DEFAULT '',
              voice_bot_id TEXT NOT NULL DEFAULT '',
              capture_run_id TEXT NOT NULL DEFAULT '',
              state TEXT NOT NULL DEFAULT '',
              assigned_at_ms INTEGER,
              released_at_ms INTEGER,
              updated_at_ms INTEGER NOT NULL,
              payload_json TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS occupancy (
              guild_id TEXT NOT NULL,
              voice_channel_id TEXT NOT NULL,
              updated_at_ms INTEGER NOT NULL,
              payload_json TEXT NOT NULL,
              PRIMARY KEY (guild_id, voice_channel_id)
            );

            CREATE TABLE IF NOT EXISTS capture_runs (
              capture_run_id TEXT PRIMARY KEY,
              guild_id TEXT NOT NULL,
              voice_channel_id TEXT NOT NULL,
              voice_bot_id TEXT NOT NULL DEFAULT '',
              started_at_ms INTEGER,
              ended_at_ms INTEGER,
              state TEXT NOT NULL DEFAULT '',
              mode TEXT NOT NULL DEFAULT '',
              updated_at_ms INTEGER NOT NULL,
              payload_json TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS timeline_events (
              sequence INTEGER PRIMARY KEY AUTOINCREMENT,
              event_id TEXT NOT NULL UNIQUE,
              guild_id TEXT NOT NULL,
              voice_channel_id TEXT NOT NULL,
              event_kind TEXT NOT NULL,
              started_at_ms INTEGER,
              ended_at_ms INTEGER,
              created_at_ms INTEGER NOT NULL,
              capture_run_id TEXT NOT NULL DEFAULT '',
              conversation_id TEXT NOT NULL DEFAULT '',
              speaker_user_id TEXT NOT NULL DEFAULT '',
              speaker_label TEXT NOT NULL DEFAULT '',
              text TEXT NOT NULL DEFAULT '',
              forgotten INTEGER NOT NULL DEFAULT 0,
              payload_json TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS conversations (
              conversation_id TEXT PRIMARY KEY,
              guild_id TEXT NOT NULL,
              voice_channel_id TEXT NOT NULL,
              start_ms INTEGER,
              end_ms INTEGER,
              last_speech_at_ms INTEGER,
              state TEXT NOT NULL DEFAULT '',
              payload_json TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS windows (
              window_id TEXT PRIMARY KEY,
              guild_id TEXT NOT NULL,
              voice_channel_id TEXT NOT NULL,
              start_ms INTEGER,
              end_ms INTEGER,
              payload_json TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS publications (
              publication_id TEXT PRIMARY KEY,
              guild_id TEXT NOT NULL,
              voice_channel_id TEXT NOT NULL,
              window_id TEXT NOT NULL DEFAULT '',
              state TEXT NOT NULL DEFAULT '',
              created_at_ms INTEGER,
              updated_at_ms INTEGER NOT NULL,
              payload_json TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS authoritative_spans (
              span_id TEXT PRIMARY KEY,
              guild_id TEXT NOT NULL,
              voice_channel_id TEXT NOT NULL,
              window_id TEXT NOT NULL DEFAULT '',
              publication_id TEXT NOT NULL DEFAULT '',
              start_ms INTEGER,
              end_ms INTEGER,
              created_at_ms INTEGER NOT NULL,
              payload_json TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS transcript_jobs (
              job_id TEXT PRIMARY KEY,
              guild_id TEXT NOT NULL,
              voice_channel_id TEXT NOT NULL,
              kind TEXT NOT NULL DEFAULT '',
              state TEXT NOT NULL DEFAULT '',
              created_at_ms INTEGER,
              updated_at_ms INTEGER NOT NULL,
              next_run_at_ms INTEGER,
              payload_blob BLOB NOT NULL
            );

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
            CREATE INDEX IF NOT EXISTS idx_timeline_kind_time
              ON timeline_events(event_kind, started_at_ms, sequence);
            CREATE INDEX IF NOT EXISTS idx_capture_runs_room_time
              ON capture_runs(guild_id, voice_channel_id, started_at_ms, ended_at_ms);
            CREATE INDEX IF NOT EXISTS idx_conversations_room_time
              ON conversations(guild_id, voice_channel_id, start_ms, end_ms);
            CREATE INDEX IF NOT EXISTS idx_spans_room_time
              ON authoritative_spans(guild_id, voice_channel_id, start_ms, end_ms);
            CREATE INDEX IF NOT EXISTS idx_jobs_state_next
              ON transcript_jobs(state, next_run_at_ms, updated_at_ms);
            CREATE INDEX IF NOT EXISTS idx_publications_room_state
              ON publications(guild_id, voice_channel_id, state, created_at_ms);
            "#,
        )?;
        self.fts_enabled = db
            .execute_batch(
                "CREATE VIRTUAL TABLE IF NOT EXISTS transcript_events_fts \
                 USING fts5(event_id UNINDEXED, guild_id UNINDEXED, voice_channel_id UNINDEXED, speaker_label, text);",
            )
            .is_ok();
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
    pub(crate) fn get_payload_by_id(
        &self,
        table: &str,
        id_column: &str,
        id_value: &str,
    ) -> Result<Value> {
        if id_value.is_empty() {
            return Ok(serde_json::json!({}));
        }
        let db = self.connect()?;
        let sql = format!("SELECT payload_json FROM {table} WHERE {id_column} = ?1");
        let row: Option<String> = db
            .query_row(&sql, params![id_value], |row| row.get(0))
            .optional()?;
        Ok(row
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
            .unwrap_or_else(|| serde_json::json!({})))
    }
}
