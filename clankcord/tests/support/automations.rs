//! Automation fixtures shared across category binaries.

use serde_json::{Value, json};

use clankcord::config::{ControlConfig, GuildConfig, PoolConfig};
use clankcord::domain::Ctx;
use clankcord::domain::automations::AutomationSpec;
use clankcord::domain::rooms::RoomConfig;
use clankcord::domain::voice::VoiceBotStatus;
use clankcord::model::job::{CommandRequest, Job};
use clankcord::model::scope::RuntimeScope;
use clankcord::store::{TimelineStore, isoformat_z, utc_now};

pub fn reminder_spec(idempotency_key: &str) -> AutomationSpec {
    AutomationSpec::from_json(&json!({
        "schema": "clankcord.automation.v0",
        "name": "remind blake",
        "idempotency_key": idempotency_key,
        "owner": {"kind": "agent", "user_id": "user-a", "source_job_id": "job_1"},
        "scope": {"scope_kind": "voice_channel", "guild_id": "guild", "scope_id": "code"},
        "trigger": {"kind": "event", "event_kinds": ["room.member_joined"]},
        "condition": {
            "kind": "predicate",
            "path": "event.speaker_user_id",
            "op": "eq",
            "value": "blake"
        },
        "actions": [{
            "kind": "response.send",
            "sink": {"kind": "agent_chat"},
            "content": "Blake joined."
        }]
    }))
    .unwrap()
}
pub fn test_runtime(timeline_store: TimelineStore) -> Ctx {
    Ctx::new(timeline_store)
}
pub async fn insert_agent_source_job(store: &TimelineStore) {
    let mut job = Job::agent_task_for_session(
        "ags_source",
        RuntimeScope::voice_channel("guild", "code"),
        "user-a",
        CommandRequest::agent_task("guild", "code", "user-a", "source request"),
    );
    job.id = "job_1".to_string();
    job.root_job_id = "job_1".to_string();
    store.create_job(job).await.unwrap();
}
pub fn voice_state(voice_channel_id: &str, user_id: &str, display_name: &str) -> Value {
    json!({
        "guild_id": "guild",
        "voice_channel_id": voice_channel_id,
        "user_id": user_id,
        "display_name": display_name,
        "member_display_name": display_name,
        "username": display_name.to_lowercase(),
        "mute": false,
        "deaf": false,
        "self_mute": false,
        "self_deaf": false,
        "self_stream": false,
        "self_video": false,
        "suppress": false,
    })
}
pub fn voice_state_with_flags(
    voice_channel_id: &str,
    user_id: &str,
    display_name: &str,
    mute: bool,
    deaf: bool,
    self_mute: bool,
    self_deaf: bool,
) -> Value {
    let mut state = voice_state(voice_channel_id, user_id, display_name);
    state["mute"] = json!(mute);
    state["deaf"] = json!(deaf);
    state["self_mute"] = json!(self_mute);
    state["self_deaf"] = json!(self_deaf);
    state
}

pub async fn write_test_runtime_config_with_pool(
    store: &TimelineStore,
    rooms: &[RoomConfig],
    pool: &PoolConfig,
) {
    store
        .write_runtime_config_snapshot(
            pool,
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
            rooms,
        )
        .await
        .unwrap();
}

pub fn test_pool_config() -> PoolConfig {
    PoolConfig {
        idle_channel_name: String::new(),
        auto_join_enabled: true,
        auto_join_min_participants: 2,
        auto_leave_empty_seconds: 5 * 60,
        auto_leave_single_deafened_seconds: 5 * 60,
        auto_rejoin_cooldown_seconds: 5 * 60,
        manual_override_seconds: 60 * 60,
        pause_release_seconds: 20 * 60,
    }
}

pub fn code_room() -> RoomConfig {
    RoomConfig {
        room_id: "code-lounge".to_string(),
        guild_id: "guild".to_string(),
        guild_slug: "guild".to_string(),
        channel_id: "code".to_string(),
        channel_slug: "code-lounge".to_string(),
        channel_name: "Code Lounge".to_string(),
        auto_join: true,
    }
}

pub fn six_minutes_ago() -> String {
    isoformat_z(Some(utc_now() - chrono::Duration::minutes(6)))
}

pub fn ready_bot() -> VoiceBotStatus {
    VoiceBotStatus {
        bot_id: "clanky-vc1".to_string(),
        ready: true,
        current_guild_id: String::new(),
        current_channel_id: String::new(),
        last_error: String::new(),
        pending_disconnect_events: 0,
        pending_disconnect_until: 0,
        user_id: "bot-user".to_string(),
        username: "Clanky".to_string(),
        gateway_running: true,
        receive_backend: "songbird".to_string(),
    }
}

pub fn spec_value(overrides: Value) -> Value {
    let mut base = json!({
        "schema": "clankcord.automation.v0",
        "name": "test automation",
        "idempotency_key": "test:auto",
        "owner": {"kind": "agent", "user_id": "user-a", "source_job_id": "job_1"},
        "scope": {"scope_kind": "voice_channel", "guild_id": "guild", "scope_id": "code"},
        "trigger": {"kind": "event", "event_kinds": ["room.member_joined"]},
        "condition": {"kind": "true"},
        "actions": [{
            "kind": "response.send",
            "sink": {"kind": "agent_chat"},
            "content": "hello"
        }]
    });
    merge_json(&mut base, overrides);
    base
}

pub fn merge_json(base: &mut Value, overrides: Value) {
    match (base, overrides) {
        (Value::Object(base), Value::Object(overrides)) => {
            for (key, value) in overrides {
                if let Some(existing) = base.get_mut(&key) {
                    merge_json(existing, value);
                } else {
                    base.insert(key, value);
                }
            }
        }
        (base, value) => *base = value,
    }
}
