use chrono::{SecondsFormat, Utc};
use serde_json::json;

use crate::support::sessions::{
    agent_thread_title_refresh_jobs, insert_active_thread_session, insert_completed_agent_response,
};
use clankcord::domain::Ctx;
use clankcord::model::agents::{AgentSessionRecord, AgentSessionRecordState, voice_route_key};
use clankcord::model::job::{
    AgentSessionStartPayload, CommandRequest, DiscordForumThreadCreateOutput,
    DiscordTextMessagePayload, Job, JobKind, JobOutput, JobPayload, JobState, TextDeliveryKind,
    TextDeliveryPayload, TextTarget, TextTargetKind,
};
use clankcord::model::scope::{RuntimeScope, RuntimeScopeKind};

#[tokio::test(flavor = "current_thread")]
async fn agent_session_records_route_by_voice_and_thread() {
    let raw = tempfile::tempdir().unwrap();
    let store = crate::support::test_store(&raw.path().join("voice")).await;
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
    let store = crate::support::test_store(&raw.path().join("voice")).await;
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
    let store = crate::support::test_store(&raw.path().join("voice")).await;
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
    crate::support::initialize_test_config(raw.path());
    let store = crate::support::test_store(&raw.path().join("voice")).await;
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
    let runtime = Ctx::new(store.clone());
    let created = store
        .create_job(Job::agent_session_retirement("maintenance"))
        .await
        .unwrap();
    let mut running = created.clone();
    running.mark_running();
    store.update_job(&running).await.unwrap();

    clankcord::engine::dispatcher::dispatch_claimed_job(
        &runtime,
        &clankcord::ports::discord::DiscordApiUnavailable,
        running,
    )
    .await
    .unwrap();

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
    crate::support::initialize_test_config(raw.path());
    let store = crate::support::test_store(&raw.path().join("voice")).await;
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
    let runtime = Ctx::new(store.clone());
    let created = store
        .create_job(Job::agent_session_retirement("maintenance"))
        .await
        .unwrap();
    let mut running = created.clone();
    running.mark_running();
    store.update_job(&running).await.unwrap();

    clankcord::engine::dispatcher::dispatch_claimed_job(
        &runtime,
        &clankcord::ports::discord::DiscordApiUnavailable,
        running,
    )
    .await
    .unwrap();

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
    crate::support::initialize_test_config(raw.path());
    let store = crate::support::test_store(&raw.path().join("voice")).await;
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
    let runtime = Ctx::new(store.clone());
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

    clankcord::engine::dispatcher::dispatch_claimed_job(
        &runtime,
        &clankcord::ports::discord::DiscordApiUnavailable,
        running,
    )
    .await
    .unwrap();

    let updated = store.get_agent_session_record("ags_sunset").await.unwrap();
    assert_eq!(updated.state, AgentSessionRecordState::Retired);
    assert_eq!(updated.retired_by_user_id, "user-a");
    assert_eq!(updated.retirement_reason, "user_sunset");
}

#[test]
fn agent_session_runtime_scope_covers_voice_dm_and_thread_routes() {
    let created_at = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    let max_active_until =
        (Utc::now() + chrono::Duration::hours(8)).to_rfc3339_opts(SecondsFormat::Millis, true);
    let voice = AgentSessionRecord::new_voice(
        "ags_voice_scope",
        "guild-a",
        "voice-a",
        "parent-a",
        "thread-a",
        created_at.clone(),
        max_active_until.clone(),
    );
    assert_eq!(
        voice.scope(),
        RuntimeScope::voice_channel("guild-a", "voice-a")
    );

    let dm = AgentSessionRecord::new_dm(
        "ags_dm_scope",
        "user-a",
        created_at.clone(),
        max_active_until.clone(),
    );
    assert_eq!(dm.scope(), RuntimeScope::dm("user-a"));

    let mut thread = voice;
    thread.route_kind = clankcord::model::agents::AgentSessionRouteKind::Thread;
    thread.discord_thread_id = "thread-a".to_string();
    assert_eq!(thread.scope(), RuntimeScope::thread("guild-a", "thread-a"));
}

#[tokio::test(flavor = "current_thread")]
async fn dm_text_message_creates_dm_scoped_agent_task_and_event() {
    let raw = tempfile::tempdir().unwrap();
    crate::support::initialize_test_config(raw.path());
    let store = crate::support::test_store(&raw.path().join("voice")).await;
    let text = store
        .create_job(Job::discord_text_message(DiscordTextMessagePayload {
            guild_id: String::new(),
            channel_id: "dm-channel".to_string(),
            message_id: "message-1".to_string(),
            author_user_id: "user-a".to_string(),
            author_username: "will".to_string(),
            author_display_name: "Will".to_string(),
            content: "what did we decide?".to_string(),
            created_at: "2026-05-17T17:44:57.000Z".to_string(),
            referenced_message_id: String::new(),
        }))
        .await
        .unwrap();
    let mut running_text = text.clone();
    running_text.mark_running();
    store.update_job(&running_text).await.unwrap();
    let runtime = Ctx::new(store.clone());

    clankcord::engine::dispatcher::dispatch_claimed_job(
        &runtime,
        &clankcord::ports::discord::DiscordApiUnavailable,
        running_text,
    )
    .await
    .unwrap();

    let updated_text = store.get_job(&text.id).await.unwrap();
    assert_eq!(updated_text.state, JobState::Waiting);
    let children = store.list_child_jobs(&text.id).await.unwrap();
    assert_eq!(children.len(), 1);
    let agent_task = &children[0];
    assert_eq!(agent_task.kind, JobKind::AgentTask);
    assert_eq!(agent_task.scope_kind, RuntimeScopeKind::Dm);
    assert_eq!(agent_task.scope_id, "user-a");

    let events = store
        .load_scope_events(
            RuntimeScopeKind::Dm,
            "",
            "user-a",
            None,
            None,
            None,
            None,
            false,
        )
        .await
        .unwrap();
    assert!(events.iter().any(|event| {
        event.get("event_kind") == Some(&json!("discord_text_message"))
            && event.get("text") == Some(&json!("what did we decide?"))
            && event.get("speaker_user_id") == Some(&json!("user-a"))
    }));
}

#[tokio::test(flavor = "current_thread")]
async fn search_returns_retired_sessions_with_resume_command() {
    let raw = tempfile::tempdir().unwrap();
    let store = crate::support::test_store(&raw.path().join("voice")).await;
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
    let runtime = Ctx::new(store);

    let result = clankcord::domain::interactions::agent_sessions::agent_session_search(
        &runtime,
        "guild",
        "code",
        "retired",
        "floating point",
        "-1h",
        10,
    )
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

#[tokio::test(flavor = "current_thread")]
async fn agent_session_thread_uses_readable_default_name_and_intro() {
    let raw = tempfile::tempdir().unwrap();
    crate::support::initialize_test_config(raw.path());
    let store = crate::support::test_store(&raw.path().join("voice")).await;
    store
        .record_voice_state_update(None, voice_state("code", "user-a", "Will"))
        .await
        .unwrap();
    store
        .record_voice_state_update(None, voice_state("code", "user-b", "Nia"))
        .await
        .unwrap();
    let created_at = crate::support::dt(2026, 5, 17, 3, 28, 0);
    let max_active_until = created_at + chrono::Duration::hours(8);
    store
        .create_agent_session_record(AgentSessionRecord::new_voice_starting(
            "ags_intro",
            "guild",
            "code",
            "agent-threads",
            created_at.to_rfc3339_opts(SecondsFormat::Millis, true),
            max_active_until.to_rfc3339_opts(SecondsFormat::Millis, true),
        ))
        .await
        .unwrap();
    let start = store
        .create_job(Job::agent_session_start(
            "guild",
            "code",
            "user-a",
            AgentSessionStartPayload {
                agent_session_id: "ags_intro".to_string(),
                guild_id: "guild".to_string(),
                voice_channel_id: "code".to_string(),
                discord_parent_channel_id: "agent-threads".to_string(),
                requested_by_user_id: "user-a".to_string(),
                command: CommandRequest::agent_task("guild", "code", "user-a", "summarize"),
            },
        ))
        .await
        .unwrap();
    let mut running = start.clone();
    running.mark_running();
    store.update_job(&running).await.unwrap();
    let runtime = Ctx::new(store.clone());

    clankcord::engine::dispatcher::dispatch_claimed_job(
        &runtime,
        &clankcord::ports::discord::DiscordApiUnavailable,
        running,
    )
    .await
    .unwrap();

    let children = store.list_child_jobs(&start.id).await.unwrap();
    assert!(
        !children
            .iter()
            .any(|child| child.kind == JobKind::DiscordForumThreadCreate)
    );
    let agent_task = children
        .iter()
        .find(|child| child.kind == JobKind::AgentTask)
        .expect("agent task child");
    let delivery = store
        .create_job(Job::text_delivery(
            RuntimeScope::voice_channel("guild", "code"),
            "user-a",
            TextDeliveryPayload::new(
                TextDeliveryKind::Message,
                TextTarget {
                    kind: TextTargetKind::AgentSession,
                    channel_id: String::new(),
                    user_id: String::new(),
                },
                "ready",
                agent_task.id.clone(),
                "user-a",
                false,
            ),
        ))
        .await
        .unwrap();
    let mut running_delivery = delivery.clone();
    running_delivery.mark_running();
    store.update_job(&running_delivery).await.unwrap();

    clankcord::engine::dispatcher::dispatch_claimed_job(
        &runtime,
        &clankcord::ports::discord::DiscordApiUnavailable,
        running_delivery,
    )
    .await
    .unwrap();

    let children = store.list_child_jobs(&delivery.id).await.unwrap();
    let thread_create = children
        .iter()
        .find(|child| child.kind == JobKind::DiscordForumThreadCreate)
        .expect("thread creation child");
    let JobPayload::DiscordForumThreadCreate(payload) = &thread_create.payload else {
        panic!("expected forum thread create payload");
    };
    assert_eq!(payload.name, "Code Lounge 2026-05-17 03:28");
    assert!(payload.content.contains("- Voice channel: `Code Lounge`"));
    assert!(
        payload
            .content
            .contains("- Requested by: <@user-a> <@user-b>")
    );
    assert!(payload.content.contains("- Session: `ags_intro`"));
    assert!(!payload.content.contains("- Guild:"));
    assert!(!payload.content.contains("`code`"));

    let mut completed_thread = thread_create.clone();
    completed_thread.mark_complete();
    completed_thread.metadata.output = Some(JobOutput::DiscordForumThreadCreate(
        DiscordForumThreadCreateOutput {
            parent_channel_id: "agent-threads".to_string(),
            thread_id: "thread-intro".to_string(),
            name: payload.name.clone(),
            source_job_id: delivery.id.clone(),
        },
    ));
    store.update_job(&completed_thread).await.unwrap();
    let mut running_delivery = store.get_job(&delivery.id).await.unwrap();
    running_delivery.mark_running();
    store.update_job(&running_delivery).await.unwrap();

    clankcord::engine::dispatcher::dispatch_claimed_job(
        &runtime,
        &clankcord::ports::discord::DiscordApiUnavailable,
        running_delivery,
    )
    .await
    .unwrap();

    let updated = store.get_agent_session_record("ags_intro").await.unwrap();
    assert_eq!(updated.discord_thread_id, "thread-intro");
    assert_eq!(updated.text_target.channel_id, "thread-intro");
    let children = store.list_child_jobs(&delivery.id).await.unwrap();
    let send = children
        .iter()
        .find(|child| child.kind == JobKind::DiscordTextSend)
        .expect("discord send child");
    assert_eq!(
        send.payload.to_json()["target"]["channel_id"],
        "thread-intro"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn maintenance_does_not_requeue_thread_title_refresh_for_same_response_count() {
    let raw = tempfile::tempdir().unwrap();
    crate::support::initialize_test_config(raw.path());
    let store = crate::support::test_store(&raw.path().join("voice")).await;
    insert_active_thread_session(&store, "ags_title").await;
    insert_completed_agent_response(&store, "ags_title", "question one", "answer one", "user-a")
        .await;
    store
        .append_event(
            "guild",
            "code",
            json!({
                "event_kind": "agent_thread_title_refresh_attempted",
                "kind": "agent_thread_title_refresh_attempted",
                "agent_session_id": "ags_title",
                "discord_thread_id": "thread-1",
                "response_count": 1,
                "refresh_job_id": "job_previous",
            }),
        )
        .await
        .unwrap();
    let runtime = Ctx::new(store.clone());
    let maintenance = store
        .create_job(Job::runtime_maintenance(500))
        .await
        .unwrap();
    let mut running = maintenance;
    running.mark_running();
    store.update_job(&running).await.unwrap();

    clankcord::engine::dispatcher::dispatch_claimed_job(
        &runtime,
        &clankcord::ports::discord::DiscordApiUnavailable,
        running,
    )
    .await
    .unwrap();

    assert!(agent_thread_title_refresh_jobs(&store).await.is_empty());
}

fn voice_state(channel_id: &str, user_id: &str, display_name: &str) -> serde_json::Value {
    json!({
        "guild_id": "guild",
        "user_id": user_id,
        "voice_channel_id": channel_id,
        "display_name": display_name,
        "username": display_name,
    })
}
