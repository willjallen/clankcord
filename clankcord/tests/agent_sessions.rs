use chrono::{SecondsFormat, Utc};
use serde_json::json;

mod common;

use clankcord::runtime::timeline::{JobVisibility, TimelineStore};
use clankcord::runtime::{
    AgentSessionRecord, AgentSessionRecordState, AgentSessionStartOutput, AgentSessionStartPayload,
    CommandRequest, DiscordForumThreadCreateOutput, DiscordTextMessagePayload, Job, JobKind,
    JobOutput, JobPayload, JobState, Runtime, RuntimeScope, RuntimeScopeKind, TextDeliveryKind,
    TextDeliveryOutput, TextDeliveryPayload, TextTarget, TextTargetKind, dm_route_key,
    voice_route_key,
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
async fn agent_session_payload_blob_uses_current_envelope() {
    let raw = tempfile::tempdir().unwrap();
    let store = common::test_store(&raw.path().join("voice")).await;
    let created_at = Utc::now();
    let max_active_until = created_at + chrono::Duration::hours(8);
    let record = AgentSessionRecord::new_voice(
        "ags_blob",
        "guild",
        "code",
        "agent-threads",
        "thread-blob",
        created_at.to_rfc3339_opts(SecondsFormat::Millis, true),
        max_active_until.to_rfc3339_opts(SecondsFormat::Millis, true),
    );

    store
        .create_agent_session_record(record.clone())
        .await
        .unwrap();
    let row = sqlx::query("SELECT payload_blob FROM agent_sessions WHERE agent_session_id = $1")
        .bind("ags_blob")
        .fetch_one(&store.pool)
        .await
        .unwrap();
    let payload_blob: Vec<u8> = sqlx::Row::try_get(&row, "payload_blob").unwrap();
    assert_eq!(&payload_blob[..8], b"CLANKAGS");
    assert_eq!(u16::from_le_bytes([payload_blob[8], payload_blob[9]]), 1);

    sqlx::query("UPDATE agent_sessions SET payload_blob = $1 WHERE agent_session_id = $2")
        .bind(bincode::serialize(&record).unwrap())
        .bind("ags_blob")
        .execute(&store.pool)
        .await
        .unwrap();
    let error = store
        .get_agent_session_record("ags_blob")
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("invalid blob envelope"));
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
async fn resume_reactivates_retired_dm_session() {
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

    let job = Job::agent_session_resume("ags_source", "dm", "", "user-a", "user-a", "");
    match &job.payload {
        JobPayload::AgentSessionResume(payload) => {
            assert_eq!(payload.new_agent_session_id, "ags_source");
        }
        _ => unreachable!(),
    }
    let mut job = store.create_job(job).await.unwrap();
    job.mark_running();
    store.update_job(&job).await.unwrap();
    let mut runtime = Runtime::from_store(store.clone()).unwrap();

    runtime.dispatch_claimed_runtime_job(job).await.unwrap();

    let resumed = store.get_agent_session_record("ags_source").await.unwrap();
    assert_eq!(resumed.state, AgentSessionRecordState::Active);
    assert_eq!(resumed.retired_at, "");
    assert_eq!(resumed.retirement_reason, "");
    assert_eq!(resumed.resumed_from_agent_session_id, "");
    assert_eq!(resumed.codex_session_id, "codex-session");
    assert_eq!(resumed.route_key, dm_route_key("user-a"));
    assert_eq!(
        store
            .list_agent_session_records("dm", "user-a", "", 500)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dm_text_message_creates_dm_scoped_agent_task_and_event() {
    let raw = tempfile::tempdir().unwrap();
    common::initialize_test_config(raw.path());
    let store = common::test_store(&raw.path().join("voice")).await;
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
    let mut runtime = Runtime::from_store(store.clone()).unwrap();

    runtime
        .dispatch_claimed_runtime_job(running_text)
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
async fn voice_resume_reactivates_source_thread_and_takes_over_active_route() {
    let raw = tempfile::tempdir().unwrap();
    common::initialize_test_config(raw.path());
    let store = common::test_store(&raw.path().join("voice")).await;
    let created_at = Utc::now() - chrono::Duration::hours(1);
    let max_active_until = created_at + chrono::Duration::hours(8);
    let mut source = AgentSessionRecord::new_voice(
        "ags_source",
        "guild",
        "code",
        "agent-threads",
        "source-thread",
        created_at.to_rfc3339_opts(SecondsFormat::Millis, true),
        max_active_until.to_rfc3339_opts(SecondsFormat::Millis, true),
    );
    source.state = AgentSessionRecordState::Retired;
    source.codex_session_id = "codex-session".to_string();
    source.retired_at = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    source.retirement_reason = "user_sunset".to_string();
    store.create_agent_session_record(source).await.unwrap();
    let active = AgentSessionRecord::new_voice(
        "ags_active",
        "guild",
        "code",
        "agent-threads",
        "active-thread",
        Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
        (Utc::now() + chrono::Duration::hours(8)).to_rfc3339_opts(SecondsFormat::Millis, true),
    );
    store.create_agent_session_record(active).await.unwrap();
    let starting = AgentSessionRecord::new_voice_starting(
        "ags_starting",
        "guild",
        "code",
        "agent-threads",
        Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
        (Utc::now() + chrono::Duration::hours(8)).to_rfc3339_opts(SecondsFormat::Millis, true),
    );
    store.create_agent_session_record(starting).await.unwrap();

    let mut job = Job::agent_session_resume("ags_source", "voice", "guild", "code", "user-a", "");
    match &job.payload {
        JobPayload::AgentSessionResume(payload) => {
            assert_eq!(payload.new_agent_session_id, "ags_source");
        }
        _ => unreachable!(),
    }
    job = store.create_job(job).await.unwrap();
    let mut running = job.clone();
    running.mark_running();
    store.update_job(&running).await.unwrap();
    let mut runtime = Runtime::from_store(store.clone()).unwrap();

    runtime.dispatch_claimed_runtime_job(running).await.unwrap();

    let retired = store.get_agent_session_record("ags_active").await.unwrap();
    assert_eq!(retired.state, AgentSessionRecordState::Retired);
    assert_eq!(
        retired.retirement_reason,
        "agent_session_resume_route_takeover"
    );
    assert_eq!(retired.retired_by_user_id, "user-a");
    let retired_starting = store
        .get_agent_session_record("ags_starting")
        .await
        .unwrap();
    assert_eq!(retired_starting.state, AgentSessionRecordState::Retired);
    assert_eq!(
        retired_starting.retirement_reason,
        "agent_session_resume_route_takeover"
    );
    assert_eq!(retired_starting.retired_by_user_id, "user-a");
    let resumed = store.get_agent_session_record("ags_source").await.unwrap();
    assert_eq!(resumed.state, AgentSessionRecordState::Active);
    assert_eq!(resumed.discord_thread_id, "source-thread");
    assert_eq!(resumed.text_target.channel_id, "source-thread");
    assert_eq!(resumed.retired_at, "");
    assert_eq!(resumed.retirement_reason, "");
    assert_eq!(resumed.resumed_from_agent_session_id, "");
    assert_eq!(resumed.codex_session_id, "codex-session");
    assert_eq!(resumed.route_key, voice_route_key("guild", "code"));
    let active = store
        .active_agent_session_for_route(&voice_route_key("guild", "code"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(active.agent_session_id, "ags_source");
    assert!(store.list_child_jobs(&job.id).await.unwrap().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn discord_thread_message_resumes_retired_voice_session() {
    let raw = tempfile::tempdir().unwrap();
    common::initialize_test_config(raw.path());
    let store = common::test_store(&raw.path().join("voice")).await;
    let created_at = Utc::now() - chrono::Duration::hours(1);
    let max_active_until = created_at + chrono::Duration::hours(8);
    let mut source = AgentSessionRecord::new_voice(
        "ags_source",
        "guild",
        "code",
        "agent-threads",
        "thread-source",
        created_at.to_rfc3339_opts(SecondsFormat::Millis, true),
        max_active_until.to_rfc3339_opts(SecondsFormat::Millis, true),
    );
    source.state = AgentSessionRecordState::Retired;
    source.codex_session_id = "codex-session".to_string();
    source.retired_at = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    source.retirement_reason = "voice_session_ended".to_string();
    store.create_agent_session_record(source).await.unwrap();
    let active = AgentSessionRecord::new_voice(
        "ags_active",
        "guild",
        "code",
        "agent-threads",
        "thread-active",
        Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
        (Utc::now() + chrono::Duration::hours(8)).to_rfc3339_opts(SecondsFormat::Millis, true),
    );
    store.create_agent_session_record(active).await.unwrap();
    let text = store
        .create_job(Job::discord_text_message(DiscordTextMessagePayload {
            guild_id: "guild".to_string(),
            channel_id: "thread-source".to_string(),
            message_id: "message-1".to_string(),
            author_user_id: "user-a".to_string(),
            author_username: "will".to_string(),
            author_display_name: "Will".to_string(),
            content: "follow up in the old thread".to_string(),
            created_at: "2026-05-17T17:44:57.000Z".to_string(),
            referenced_message_id: String::new(),
        }))
        .await
        .unwrap();
    let mut running_text = text.clone();
    running_text.mark_running();
    store.update_job(&running_text).await.unwrap();
    let mut runtime = Runtime::from_store(store.clone()).unwrap();

    runtime
        .dispatch_claimed_runtime_job(running_text)
        .await
        .unwrap();

    let updated_text = store.get_job(&text.id).await.unwrap();
    assert_eq!(updated_text.state, JobState::Waiting);
    let children = store.list_child_jobs(&text.id).await.unwrap();
    assert_eq!(children.len(), 1);
    let resume = &children[0];
    assert_eq!(resume.kind, JobKind::AgentSessionResume);
    let JobPayload::AgentSessionResume(payload) = &resume.payload else {
        panic!("expected agent session resume payload");
    };
    assert_eq!(payload.source_agent_session_id, "ags_source");
    assert_eq!(payload.route_kind, "voice");
    assert_eq!(payload.guild_id, "guild");
    assert_eq!(payload.voice_channel_id, "code");
    assert_eq!(payload.message, "follow up in the old thread");

    let events = store
        .load_events("guild", "code", None, None, None, None, false)
        .await
        .unwrap();
    assert!(events.iter().any(|event| {
        event.get("event_kind") == Some(&json!("discord_text_message"))
            && event.get("agent_session_id") == Some(&json!("ags_source"))
            && event.get("discord_channel_id") == Some(&json!("thread-source"))
            && event.get("text") == Some(&json!("follow up in the old thread"))
    }));

    let mut running_resume = resume.clone();
    running_resume.mark_running();
    store.update_job(&running_resume).await.unwrap();
    runtime
        .dispatch_claimed_runtime_job(running_resume)
        .await
        .unwrap();

    let resumed = store.get_agent_session_record("ags_source").await.unwrap();
    assert_eq!(resumed.state, AgentSessionRecordState::Active);
    assert_eq!(resumed.codex_session_id, "codex-session");
    assert_eq!(resumed.discord_thread_id, "thread-source");
    assert_eq!(resumed.text_target.channel_id, "thread-source");
    let retired = store.get_agent_session_record("ags_active").await.unwrap();
    assert_eq!(retired.state, AgentSessionRecordState::Retired);
    assert_eq!(
        retired.retirement_reason,
        "agent_session_resume_route_takeover"
    );
    let resume_children = store.list_child_jobs(&resume.id).await.unwrap();
    assert_eq!(resume_children.len(), 1);
    let task = &resume_children[0];
    assert_eq!(task.kind, JobKind::AgentTask);
    let JobPayload::AgentTask(payload) = &task.payload else {
        panic!("expected agent task payload");
    };
    assert_eq!(payload.agent_session_id, "ags_source");
    assert_eq!(
        payload.command.arguments.request_text(),
        "follow up in the old thread"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn retired_start_session_does_not_spawn_agent_task() {
    let raw = tempfile::tempdir().unwrap();
    common::initialize_test_config(raw.path());
    let store = common::test_store(&raw.path().join("voice")).await;
    let created_at = Utc::now();
    let max_active_until = created_at + chrono::Duration::hours(8);
    let mut record = AgentSessionRecord::new_voice_starting(
        "ags_starting",
        "guild",
        "code",
        "agent-threads",
        created_at.to_rfc3339_opts(SecondsFormat::Millis, true),
        max_active_until.to_rfc3339_opts(SecondsFormat::Millis, true),
    );
    record.state = AgentSessionRecordState::Retired;
    record.retired_at = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    record.retirement_reason = "agent_session_resume_route_takeover".to_string();
    store.create_agent_session_record(record).await.unwrap();
    let start = store
        .create_job(Job::agent_session_start(
            "guild",
            "code",
            "user-a",
            AgentSessionStartPayload {
                agent_session_id: "ags_starting".to_string(),
                guild_id: "guild".to_string(),
                voice_channel_id: "code".to_string(),
                discord_parent_channel_id: "agent-threads".to_string(),
                requested_by_user_id: "user-a".to_string(),
                command: CommandRequest::agent_task("guild", "code", "user-a", "resume old"),
            },
        ))
        .await
        .unwrap();
    let mut running = store.get_job(&start.id).await.unwrap();
    running.mark_running();
    store.update_job(&running).await.unwrap();
    let mut runtime = Runtime::from_store(store.clone()).unwrap();

    runtime.dispatch_claimed_runtime_job(running).await.unwrap();

    let updated = store
        .get_agent_session_record("ags_starting")
        .await
        .unwrap();
    assert_eq!(updated.state, AgentSessionRecordState::Retired);
    assert_eq!(updated.discord_thread_id, "");
    let completed = store.get_job(&start.id).await.unwrap();
    assert_eq!(completed.state, JobState::Complete);
    assert_eq!(
        completed.metadata.output.unwrap(),
        JobOutput::AgentSessionStart(AgentSessionStartOutput {
            agent_session_id: "ags_starting".to_string(),
            status: "retired".to_string(),
            agent_task_job_id: String::new(),
        })
    );
    assert!(store.list_child_jobs(&start.id).await.unwrap().is_empty());
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

#[tokio::test(flavor = "current_thread")]
async fn agent_session_thread_uses_readable_default_name_and_intro() {
    let raw = tempfile::tempdir().unwrap();
    common::initialize_test_config(raw.path());
    let store = common::test_store(&raw.path().join("voice")).await;
    store
        .record_voice_state_update(None, voice_state("code", "user-a", "Will"))
        .await
        .unwrap();
    store
        .record_voice_state_update(None, voice_state("code", "user-b", "Nia"))
        .await
        .unwrap();
    let created_at = common::dt(2026, 5, 17, 3, 28, 0);
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
    let mut runtime = Runtime::from_store(store.clone()).unwrap();

    runtime.dispatch_claimed_runtime_job(running).await.unwrap();

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

    runtime
        .dispatch_claimed_runtime_job(running_delivery)
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

    runtime
        .dispatch_claimed_runtime_job(running_delivery)
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
async fn session_response_reroutes_after_resume_takeover() {
    let raw = tempfile::tempdir().unwrap();
    common::initialize_test_config(raw.path());
    let store = common::test_store(&raw.path().join("voice")).await;
    let created_at = Utc::now() - chrono::Duration::minutes(10);
    let max_active_until = created_at + chrono::Duration::hours(8);
    let mut source = AgentSessionRecord::new_voice(
        "ags_source",
        "guild",
        "code",
        "agent-threads",
        "thread-source",
        created_at.to_rfc3339_opts(SecondsFormat::Millis, true),
        max_active_until.to_rfc3339_opts(SecondsFormat::Millis, true),
    );
    source.state = AgentSessionRecordState::Retired;
    source.retired_at =
        (created_at + chrono::Duration::minutes(5)).to_rfc3339_opts(SecondsFormat::Millis, true);
    source.retirement_reason = "agent_session_resume_route_takeover".to_string();
    store.create_agent_session_record(source).await.unwrap();
    let active = AgentSessionRecord::new_voice(
        "ags_active",
        "guild",
        "code",
        "agent-threads",
        "thread-active",
        (created_at + chrono::Duration::minutes(6)).to_rfc3339_opts(SecondsFormat::Millis, true),
        max_active_until.to_rfc3339_opts(SecondsFormat::Millis, true),
    );
    store.create_agent_session_record(active).await.unwrap();
    let task = store
        .create_job(Job::agent_task_for_session(
            "ags_source",
            RuntimeScope::voice_channel("guild", "code"),
            "user-a",
            CommandRequest::agent_task("guild", "code", "user-a", "resume session"),
        ))
        .await
        .unwrap();
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
                "resumed",
                task.id,
                "user-a",
                false,
            ),
        ))
        .await
        .unwrap();
    let mut running = delivery.clone();
    running.mark_running();
    store.update_job(&running).await.unwrap();
    let mut runtime = Runtime::from_store(store.clone()).unwrap();

    runtime.dispatch_claimed_runtime_job(running).await.unwrap();

    let children = store.list_child_jobs(&delivery.id).await.unwrap();
    assert!(
        !children
            .iter()
            .any(|child| child.kind == JobKind::DiscordForumThreadCreate)
    );
    let send = children
        .iter()
        .find(|child| child.kind == JobKind::DiscordTextSend)
        .expect("discord send child");
    assert_eq!(
        send.payload.to_json()["target"]["channel_id"],
        "thread-active"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn maintenance_queues_one_thread_title_refresh_after_two_visible_agent_responses() {
    let raw = tempfile::tempdir().unwrap();
    common::initialize_test_config(raw.path());
    let store = common::test_store(&raw.path().join("voice")).await;
    insert_active_thread_session(&store, "ags_title").await;
    insert_completed_agent_response(
        &store,
        "ags_title",
        "explain gRPC",
        "gRPC uses HTTP/2 streams for service calls.",
        "user-a",
    )
    .await;
    insert_completed_agent_response(
        &store,
        "ags_title",
        "compare with REST",
        "REST exposes resources with request methods.",
        "user-b",
    )
    .await;
    let mut runtime = Runtime::from_store(store.clone()).unwrap();
    let maintenance = store
        .create_job(Job::runtime_maintenance(500))
        .await
        .unwrap();
    let mut running = maintenance.clone();
    running.mark_running();
    store.update_job(&running).await.unwrap();

    runtime.dispatch_claimed_runtime_job(running).await.unwrap();

    let title_jobs = agent_thread_title_refresh_jobs(&store).await;
    assert_eq!(title_jobs.len(), 1);
    let JobPayload::AgentThreadTitleRefresh(payload) = &title_jobs[0].payload else {
        panic!("expected thread-title payload");
    };
    assert_eq!(payload.agent_session_id, "ags_title");
    assert_eq!(payload.discord_thread_id, "thread-1");
    assert_eq!(payload.response_count, 2);
    assert_eq!(payload.current_thread_name, "Code Lounge 2026-05-17 03:28");

    let completed = store.get_job(&maintenance.id).await.unwrap();
    let output = completed.metadata.output.unwrap().to_json();
    assert!(
        output["submitted_jobs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|job| {
                job["definition"] == json!("agent_thread_title_refresh")
                    && job["job_kind"] == json!("agent_thread_title_refresh")
            })
    );
}

#[tokio::test(flavor = "current_thread")]
async fn maintenance_does_not_requeue_thread_title_refresh_for_same_response_count() {
    let raw = tempfile::tempdir().unwrap();
    common::initialize_test_config(raw.path());
    let store = common::test_store(&raw.path().join("voice")).await;
    insert_active_thread_session(&store, "ags_title").await;
    insert_completed_agent_response(&store, "ags_title", "question one", "answer one", "user-a")
        .await;
    insert_completed_agent_response(&store, "ags_title", "question two", "answer two", "user-b")
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
                "response_count": 2,
                "refresh_job_id": "job_previous",
            }),
        )
        .await
        .unwrap();
    let mut runtime = Runtime::from_store(store.clone()).unwrap();
    let maintenance = store
        .create_job(Job::runtime_maintenance(500))
        .await
        .unwrap();
    let mut running = maintenance;
    running.mark_running();
    store.update_job(&running).await.unwrap();

    runtime.dispatch_claimed_runtime_job(running).await.unwrap();

    assert!(agent_thread_title_refresh_jobs(&store).await.is_empty());
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

fn voice_state(channel_id: &str, user_id: &str, display_name: &str) -> serde_json::Value {
    json!({
        "guild_id": "guild",
        "user_id": user_id,
        "voice_channel_id": channel_id,
        "display_name": display_name,
        "username": display_name,
    })
}

async fn insert_active_thread_session(store: &TimelineStore, id: &str) {
    let created_at = common::dt(2026, 5, 17, 3, 28, 0);
    let max_active_until = created_at + chrono::Duration::hours(8);
    store
        .create_agent_session_record(AgentSessionRecord::new_voice(
            id,
            "guild",
            "code",
            "agent-threads",
            "thread-1",
            created_at.to_rfc3339_opts(SecondsFormat::Millis, true),
            max_active_until.to_rfc3339_opts(SecondsFormat::Millis, true),
        ))
        .await
        .unwrap();
}

async fn insert_completed_agent_response(
    store: &TimelineStore,
    agent_session_id: &str,
    request: &str,
    response: &str,
    requested_by_user_id: &str,
) {
    let mut task = Job::agent_task_for_session(
        agent_session_id,
        RuntimeScope::voice_channel("guild", "code"),
        requested_by_user_id,
        CommandRequest::agent_task("guild", "code", requested_by_user_id, request),
    );
    task.mark_complete();
    let task = store.create_job(task).await.unwrap();
    let target = TextTarget {
        kind: TextTargetKind::Channel,
        channel_id: "thread-1".to_string(),
        user_id: String::new(),
    };
    let mut delivery = Job::text_delivery(
        RuntimeScope::voice_channel("guild", "code"),
        requested_by_user_id,
        TextDeliveryPayload::new(
            TextDeliveryKind::Message,
            target.clone(),
            response,
            task.id.clone(),
            requested_by_user_id,
            false,
        ),
    );
    delivery.mark_complete();
    delivery.metadata.output = Some(JobOutput::TextDelivery(TextDeliveryOutput {
        intent: TextDeliveryKind::Message.as_str().to_string(),
        target,
        source_job_id: task.id,
        discord_post: None,
    }));
    store.create_job(delivery).await.unwrap();
}

async fn agent_thread_title_refresh_jobs(store: &TimelineStore) -> Vec<Job> {
    store
        .list_jobs_with_visibility(None, None, JobVisibility::IncludeEphemeral)
        .await
        .unwrap()
        .into_iter()
        .filter(|job| job.kind == JobKind::AgentThreadTitleRefresh)
        .collect()
}
