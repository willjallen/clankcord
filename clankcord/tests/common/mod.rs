#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use chrono::{TimeZone, Utc};
use serde_json::Value;
use sqlx::postgres::PgPoolOptions;

use clankcord::adapters::discord::voice::types::LiveVoiceSession;
use clankcord::config::{ControlConfig, GuildConfig, PoolConfig};
use clankcord::runtime::timeline::{SpeechEventInput, TimelineStore};
use clankcord::runtime::{RoomConfig, VoiceCaptureSessionStatus};

const LOCAL_TEST_POSTGRES_URL: &str =
    "postgres://clankcord_test:clankcord_test@127.0.0.1:54330/clankcord_test";

pub(crate) fn initialize_test_config(root: &Path) {
    static CONFIG_LOCK: Mutex<()> = Mutex::new(());
    let _guard = CONFIG_LOCK.lock().unwrap();
    let path = root.join("config");
    std::fs::create_dir_all(&path).unwrap();
    let config = include_str!("../../../config.ex.toml")
        .replace(
            "state_dir = \"/clankcord/state\"",
            &format!("state_dir = \"{}\"", root.join("state").display()),
        )
        .replace(
            "voice_memory_root = \"/clankcord/durable/clankcord/voice\"",
            &format!("voice_memory_root = \"{}\"", root.join("voice").display()),
        )
        .replace(
            "agent_workspaces_root = \"/clankcord/state/agent-workspaces\"",
            &format!(
                "agent_workspaces_root = \"{}\"",
                root.join("agent-workspaces").display()
            ),
        )
        .replace(
            "dir = \"/workspace/clankcord/res/prompts\"",
            &format!(
                "dir = \"{}\"",
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("res/prompts")
                    .display()
            ),
        );
    std::fs::write(path.join("config.toml"), config).unwrap();
    let original_dir = std::env::current_dir().unwrap();
    std::env::set_current_dir(&path).unwrap();
    let _ = clankcord::config::app_config();
    std::env::set_current_dir(original_dir).unwrap();
}

pub(crate) fn dt(
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
) -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(year, month, day, hour, minute, second)
        .unwrap()
}

pub(crate) async fn append_speech(
    store: &TimelineStore,
    raw_root: &Path,
    start: chrono::DateTime<Utc>,
    end: chrono::DateTime<Utc>,
    text: &str,
    segment_index: i64,
    source_audio_path: Option<PathBuf>,
) -> Value {
    store
        .append_speech_event(SpeechEventInput {
            guild_id: "guild".to_string(),
            guild_slug: "guild".to_string(),
            voice_channel_id: "code".to_string(),
            voice_channel_name: "Code Lounge".to_string(),
            voice_channel_slug: "code-lounge".to_string(),
            capture_run_id: "cap_test".to_string(),
            voice_bot_id: "clanky-vc1".to_string(),
            voice_bot_discord_user_id: "bot-user".to_string(),
            speaker_user_id: "user-a".to_string(),
            speaker_label: "Will".to_string(),
            speaker_username: "will".to_string(),
            segment_start_time: start,
            segment_end_time: end,
            text_draft: text.to_string(),
            source_audio_path: source_audio_path
                .unwrap_or_else(|| raw_root.join(format!("source-{segment_index}.wav"))),
            audio_checksum: "sha256:test".to_string(),
            segment_index,
            duration_ms: (end - start).num_milliseconds(),
            ..Default::default()
        })
        .await
        .unwrap()
}

pub(crate) async fn test_store(root: &Path) -> TimelineStore {
    let schema = test_schema_name();
    let database_url = std::env::var("CLANKCORD_TEST_POSTGRES_URL")
        .unwrap_or_else(|_| LOCAL_TEST_POSTGRES_URL.to_string());
    assert_test_database_url(&database_url);
    let admin_pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .unwrap();
    sqlx::query(&format!("CREATE SCHEMA {}", quote_identifier(&schema)))
        .execute(&admin_pool)
        .await
        .unwrap();
    admin_pool.close().await;
    let store =
        TimelineStore::new_with_database(Some(root.to_path_buf()), database_url, schema).unwrap();
    store.initialize().await.unwrap();
    store
        .write_runtime_config_snapshot(
            &PoolConfig {
                idle_channel_name: String::new(),
                auto_join_enabled: true,
                manual_leave_cooldown_seconds: 20 * 60,
                manual_join_hold_seconds: 60 * 60,
                pause_release_seconds: 20 * 60,
            },
            &ControlConfig {
                guild_id: "guild".to_string(),
                guild_slug: "guild".to_string(),
                default_voice_room_id: "code-lounge".to_string(),
                bots_channel_id: "bots".to_string(),
                agent_threads_channel_id: "agent-threads".to_string(),
                transcripts_forum_id: "transcripts".to_string(),
                thread_auto_archive_minutes: 1440,
            },
            &[GuildConfig {
                guild_id: "guild".to_string(),
                guild_slug: "guild".to_string(),
                idle_channel_id: String::new(),
                idle_channel_name: String::new(),
            }],
            &[RoomConfig {
                room_id: "code-lounge".to_string(),
                guild_id: "guild".to_string(),
                guild_slug: "guild".to_string(),
                channel_id: "code".to_string(),
                channel_slug: "code-lounge".to_string(),
                channel_name: "Code Lounge".to_string(),
                auto_join: true,
            }],
        )
        .await
        .unwrap();
    store
}

pub(crate) async fn test_state_dir(root: &Path) -> tokio::sync::MutexGuard<'static, ()> {
    static ENV_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    let guard = ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    unsafe {
        std::env::set_var("CLANKCORD_STATE_DIR", root.join("state"));
    }
    guard
}

fn test_schema_name() -> String {
    static NEXT_SCHEMA: AtomicU64 = AtomicU64::new(1);
    format!(
        "clankcord_test_{}_{}",
        std::process::id(),
        NEXT_SCHEMA.fetch_add(1, Ordering::Relaxed)
    )
}

fn quote_identifier(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn assert_test_database_url(value: &str) {
    let parsed = url::Url::parse(value).expect("CLANKCORD_TEST_POSTGRES_URL must be a URL");
    let database_name = parsed.path().trim_start_matches('/');
    assert!(
        database_name.contains("test"),
        "CLANKCORD_TEST_POSTGRES_URL must point to a test database, got `{database_name}`"
    );
}

pub(crate) fn test_voice_session(raw_root: &Path) -> LiveVoiceSession {
    LiveVoiceSession {
        session_id: "cap_test".to_string(),
        room: RoomConfig {
            room_id: "code-lounge".to_string(),
            guild_id: "guild".to_string(),
            guild_slug: "guild".to_string(),
            channel_id: "code".to_string(),
            channel_slug: "code-lounge".to_string(),
            channel_name: "Code Lounge".to_string(),
            auto_join: true,
        },
        bot_id: "clanky-vc1".to_string(),
        bot_user_id: "bot-user".to_string(),
        thread_id: String::new(),
        thread_name: String::new(),
        started_at: dt(2026, 5, 12, 16, 0, 0),
        session_dir: raw_root.join("session"),
        minute_message_ids: Default::default(),
        participants: Default::default(),
        buffers: Default::default(),
        packet_debug: clankcord::adapters::discord::voice::diagnostics::default_packet_debug(),
        debug_notes: Default::default(),
        segment_counter: 0,
        audio_segments: Vec::new(),
        transcription_task_ids: Default::default(),
        finalizing: false,
        ended_at: None,
        voice_channel_id: "code".to_string(),
        transcript_event_count: 0,
        last_pcm_at: None,
        last_transcript_at: None,
        last_pcm_monotonic: 0.0,
        last_transcript_monotonic: 0.0,
        last_stall_log_monotonic: 0.0,
        voice_client_debug: Default::default(),
        capture_run_id: "cap_test".to_string(),
        assignment_id: String::new(),
        mode: "local_buffering".to_string(),
    }
}

pub(crate) fn merge_json(base: &Value, extra: Value) -> Value {
    let mut map = base.as_object().unwrap().clone();
    map.extend(extra.as_object().unwrap().clone());
    Value::Object(map)
}

#[allow(dead_code)]
pub(crate) fn ended(session: &mut VoiceCaptureSessionStatus) {
    session.mark_ended(clankcord::runtime::timeline::isoformat_z(None));
}
