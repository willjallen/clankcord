use chrono::{SecondsFormat, Utc};
use serde_json::json;

mod common;

use clankcord::runtime::{
    AgentSessionRecord, AgentSessionRecordState, DiscordTextMessagePayload, Job, JobKind,
    JobPayload, Runtime, TextTargetKind, dm_route_key, voice_route_key,
};

#[tokio::test(flavor = "current_thread")]
async fn agent_session_records_route_by_voice_and_thread() {
    let raw = tempfile::tempdir().unwrap();
    let store = common::test_store(&raw.path().join("voice")).await;
    let created_at = Utc::now();
    let max_active_until = created_at + chrono::Duration::hours(8);
    let record = AgentSessionRecord::new_voice(
        "ags_test",
        "guild",
        "code",
        "agent-threads",
        "thread-1",
        created_at.to_rfc3339_opts(SecondsFormat::Millis, true),
        max_active_until.to_rfc3339_opts(SecondsFormat::Millis, true),
    );

    store
        .create_agent_session_record(record.clone())
        .await
        .unwrap();

    let by_route = store
        .active_agent_session_for_route(&voice_route_key("guild", "code"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(by_route.agent_session_id, "ags_test");
    assert_eq!(by_route.text_target.kind, TextTargetKind::Channel);
    assert_eq!(by_route.text_target.channel_id, "thread-1");

    let by_thread = store
        .agent_session_for_thread("thread-1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(by_thread.route_key, voice_route_key("guild", "code"));
}

#[tokio::test(flavor = "current_thread")]
async fn retired_agent_sessions_stop_matching_active_route() {
    let raw = tempfile::tempdir().unwrap();
    let store = common::test_store(&raw.path().join("voice")).await;
    let created_at = Utc::now();
    let max_active_until = created_at + chrono::Duration::hours(8);
    let mut record = AgentSessionRecord::new_voice(
        "ags_retired",
        "guild",
        "code",
        "agent-threads",
        "thread-1",
        created_at.to_rfc3339_opts(SecondsFormat::Millis, true),
        max_active_until.to_rfc3339_opts(SecondsFormat::Millis, true),
    );
    record.state = AgentSessionRecordState::Retired;
    store.create_agent_session_record(record).await.unwrap();

    let by_route = store
        .active_agent_session_for_route(&voice_route_key("guild", "code"))
        .await
        .unwrap();
    assert!(by_route.is_none());

    let by_thread = store
        .agent_session_for_thread("thread-1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(by_thread.state, AgentSessionRecordState::Retired);
}

#[tokio::test(flavor = "current_thread")]
async fn active_route_excludes_sessions_at_eight_hour_cap() {
    let raw = tempfile::tempdir().unwrap();
    let store = common::test_store(&raw.path().join("voice")).await;
    let created_at = Utc::now() - chrono::Duration::hours(9);
    let max_active_until = created_at + chrono::Duration::hours(8);
    let record = AgentSessionRecord::new_voice(
        "ags_capped",
        "guild",
        "code",
        "agent-threads",
        "thread-1",
        created_at.to_rfc3339_opts(SecondsFormat::Millis, true),
        max_active_until.to_rfc3339_opts(SecondsFormat::Millis, true),
    );
    store.create_agent_session_record(record).await.unwrap();

    let by_route = store
        .active_agent_session_for_route(&voice_route_key("guild", "code"))
        .await
        .unwrap();
    assert!(by_route.is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn maintenance_retires_capped_agent_sessions() {
    let raw = tempfile::tempdir().unwrap();
    common::initialize_test_config(raw.path());
    let store = common::test_store(&raw.path().join("voice")).await;
    let created_at = Utc::now() - chrono::Duration::hours(9);
    let max_active_until = created_at + chrono::Duration::hours(8);
    let record = AgentSessionRecord::new_voice(
        "ags_capped",
        "guild",
        "code",
        "agent-threads",
        "thread-1",
        created_at.to_rfc3339_opts(SecondsFormat::Millis, true),
        max_active_until.to_rfc3339_opts(SecondsFormat::Millis, true),
    );
    store.create_agent_session_record(record).await.unwrap();
    let mut runtime = Runtime::from_store(store.clone()).unwrap();
    let created = store
        .create_job(Job::agent_session_retirement("maintenance"))
        .await
        .unwrap();
    let mut running = created.clone();
    running.mark_running();
    store.update_job(&running).await.unwrap();

    runtime.dispatch_claimed_runtime_job(running).await.unwrap();

    let updated = store.get_agent_session_record("ags_capped").await.unwrap();
    assert_eq!(updated.state, AgentSessionRecordState::Retired);
    assert_eq!(updated.retirement_reason, "max_duration");
    let events = store
        .load_events("guild", "code", None, None, None, None, false)
        .await
        .unwrap();
    assert!(events.iter().any(|event| {
        event.get("event_kind") == Some(&json!("agent_session_retired"))
            && event.get("retirement_reason") == Some(&json!("max_duration"))
    }));
}

#[tokio::test(flavor = "current_thread")]
async fn maintenance_retires_sessions_when_bound_voice_session_ended() {
    let raw = tempfile::tempdir().unwrap();
    common::initialize_test_config(raw.path());
    let store = common::test_store(&raw.path().join("voice")).await;
    let created_at = Utc::now();
    let max_active_until = created_at + chrono::Duration::hours(8);
    let mut record = AgentSessionRecord::new_voice(
        "ags_voice_done",
        "guild",
        "code",
        "agent-threads",
        "thread-1",
        created_at.to_rfc3339_opts(SecondsFormat::Millis, true),
        max_active_until.to_rfc3339_opts(SecondsFormat::Millis, true),
    );
    record.voice_capture_session_id = "cap_test".to_string();
    store.create_agent_session_record(record).await.unwrap();
    let mut runtime = Runtime::from_store(store.clone()).unwrap();
    let created = store
        .create_job(Job::agent_session_retirement("maintenance"))
        .await
        .unwrap();
    let mut running = created.clone();
    running.mark_running();
    store.update_job(&running).await.unwrap();

    runtime.dispatch_claimed_runtime_job(running).await.unwrap();

    let updated = store
        .get_agent_session_record("ags_voice_done")
        .await
        .unwrap();
    assert_eq!(updated.state, AgentSessionRecordState::Retired);
    assert_eq!(updated.retirement_reason, "voice_session_ended");
}

#[tokio::test(flavor = "current_thread")]
async fn user_sunset_retires_session() {
    let raw = tempfile::tempdir().unwrap();
    common::initialize_test_config(raw.path());
    let store = common::test_store(&raw.path().join("voice")).await;
    let created_at = Utc::now();
    let max_active_until = created_at + chrono::Duration::hours(8);
    let record = AgentSessionRecord::new_voice(
        "ags_sunset",
        "guild",
        "code",
        "agent-threads",
        "thread-1",
        created_at.to_rfc3339_opts(SecondsFormat::Millis, true),
        max_active_until.to_rfc3339_opts(SecondsFormat::Millis, true),
    );
    store.create_agent_session_record(record).await.unwrap();
    let mut runtime = Runtime::from_store(store.clone()).unwrap();
    let created = store
        .create_job(Job::agent_session_sunset(
            "ags_sunset",
            "user-a",
            "user_sunset",
        ))
        .await
        .unwrap();
    let mut running = created.clone();
    running.mark_running();
    store.update_job(&running).await.unwrap();

    runtime.dispatch_claimed_runtime_job(running).await.unwrap();

    let updated = store.get_agent_session_record("ags_sunset").await.unwrap();
    assert_eq!(updated.state, AgentSessionRecordState::Retired);
    assert_eq!(updated.retired_by_user_id, "user-a");
    assert_eq!(updated.retirement_reason, "user_sunset");
}

#[tokio::test(flavor = "current_thread")]
async fn resume_creates_linked_active_dm_session() {
    let raw = tempfile::tempdir().unwrap();
    common::initialize_test_config(raw.path());
    let store = common::test_store(&raw.path().join("voice")).await;
    let created_at = Utc::now() - chrono::Duration::hours(1);
    let max_active_until = created_at + chrono::Duration::hours(8);
    let mut source = AgentSessionRecord::new_dm(
        "ags_source",
        "user-a",
        created_at.to_rfc3339_opts(SecondsFormat::Millis, true),
        max_active_until.to_rfc3339_opts(SecondsFormat::Millis, true),
    );
    source.state = AgentSessionRecordState::Retired;
    source.codex_session_id = "codex-session".to_string();
    source.retired_at = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    source.retirement_reason = "user_sunset".to_string();
    store.create_agent_session_record(source).await.unwrap();

    let mut job = Job::agent_session_resume("ags_source", "dm", "", "", "user-a", "user-a", "");
    let new_id = match &job.payload {
        JobPayload::AgentSessionResume(payload) => payload.new_agent_session_id.clone(),
        _ => unreachable!(),
    };
    job = store.create_job(job).await.unwrap();
    let mut running = job.clone();
    running.mark_running();
    store.update_job(&running).await.unwrap();
    let mut runtime = Runtime::from_store(store.clone()).unwrap();

    runtime.dispatch_claimed_runtime_job(running).await.unwrap();

    let resumed = store.get_agent_session_record(&new_id).await.unwrap();
    assert_eq!(resumed.state, AgentSessionRecordState::Active);
    assert_eq!(resumed.resumed_from_agent_session_id, "ags_source");
    assert_eq!(resumed.codex_session_id, "codex-session");
    assert_eq!(resumed.route_key, dm_route_key("user-a"));
}

#[tokio::test(flavor = "current_thread")]
async fn search_returns_retired_sessions_with_resume_command() {
    let raw = tempfile::tempdir().unwrap();
    let store = common::test_store(&raw.path().join("voice")).await;
    let created_at = Utc::now() - chrono::Duration::minutes(10);
    let max_active_until = created_at + chrono::Duration::hours(8);
    let mut record = AgentSessionRecord::new_voice(
        "ags_search",
        "guild",
        "code",
        "agent-threads",
        "thread-1",
        created_at.to_rfc3339_opts(SecondsFormat::Millis, true),
        max_active_until.to_rfc3339_opts(SecondsFormat::Millis, true),
    );
    record.state = AgentSessionRecordState::Retired;
    record.retired_at = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    record.retirement_reason = "voice_session_ended".to_string();
    store.create_agent_session_record(record).await.unwrap();
    store
        .append_event(
            "guild",
            "code",
            json!({
                "event_kind": "discord_text_message",
                "kind": "discord_text_message",
                "created_at": (created_at + chrono::Duration::minutes(1))
                    .to_rfc3339_opts(SecondsFormat::Millis, true),
                "text": "floating point discussion",
            }),
        )
        .await
        .unwrap();
    let runtime = Runtime::from_store(store).unwrap();

    let result = runtime
        .agent_session_search("guild", "code", "retired", "floating point", "-1h", 10)
        .await
        .unwrap();

    assert_eq!(result["count"], json!(1));
    assert_eq!(result["hits"][0]["agent_session_id"], json!("ags_search"));
    assert!(
        result["hits"][0]["resume_command"]
            .as_str()
            .unwrap()
            .contains("clankcord agent-sessions resume ags_search")
    );
}

#[test]
fn discord_text_message_job_round_trips() {
    let job = Job::discord_text_message(DiscordTextMessagePayload {
        guild_id: "guild".to_string(),
        channel_id: "thread-1".to_string(),
        message_id: "message-1".to_string(),
        author_user_id: "user-a".to_string(),
        author_username: "will".to_string(),
        author_display_name: "Will".to_string(),
        content: "follow up".to_string(),
        created_at: "2026-05-15T10:00:00.000Z".to_string(),
        referenced_message_id: String::new(),
    });

    let decoded = Job::decode(&job.encode().unwrap()).unwrap();
    assert_eq!(decoded.kind, JobKind::DiscordTextMessage);
    assert_eq!(decoded.requested_by_user_id, "user-a");
    assert_eq!(decoded.payload.to_json()["content"], "follow up");
}
