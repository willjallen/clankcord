//! Pins for the session-thread fixes: threads allocate on delivery, resume
//! from managed messages, survive deletion, and title once
//! (2d0c193, 2203812, 238aebf, 9e4c0f3).

use crate::support::dt;
use crate::support::sessions::{
    agent_thread_title_refresh_jobs, insert_active_thread_session, insert_completed_agent_response,
};
use crate::support::test_store;
use crate::support::wake::{append_event, string_field, test_runtime};
use chrono::SecondsFormat;
use chrono::Utc;
use clankcord::domain::Ctx;
use clankcord::domain::voice::capture::wake_activations::execute;
use clankcord::domain::voice::capture::wake_activations::schedule_from_wake_event;
use clankcord::model::agents::AgentSessionRecord;
use clankcord::model::agents::AgentSessionRecordState;
use clankcord::model::agents::dm_route_key;
use clankcord::model::agents::voice_route_key;
use clankcord::model::job::AgentSessionStartOutput;
use clankcord::model::job::AgentSessionStartPayload;
use clankcord::model::job::BinaryPayload;
use clankcord::model::job::CommandRequest;
use clankcord::model::job::DiscordForumThreadCreateOutput;
use clankcord::model::job::DiscordForumThreadRenamePayload;
use clankcord::model::job::DiscordTextMessagePayload;
use clankcord::model::job::DiscordTextSendPayload;
use clankcord::model::job::Job;
use clankcord::model::job::JobKind;
use clankcord::model::job::JobOutput;
use clankcord::model::job::JobPayload;
use clankcord::model::job::JobState;
use clankcord::model::job::TextDeliveryKind;
use clankcord::model::job::TextDeliveryPayload;
use clankcord::model::job::TextTarget;
use clankcord::model::job::TextTargetKind;
use clankcord::model::scope::RuntimeScope;
use serde_json::json;

#[tokio::test(flavor = "current_thread")]
async fn resume_reactivates_retired_dm_session() {
    let raw = tempfile::tempdir().unwrap();
    crate::support::initialize_test_config(raw.path());
    let store = crate::support::test_store(&raw.path().join("voice")).await;
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
    let runtime = Ctx::new(store.clone());

    clankcord::engine::dispatcher::dispatch_claimed_job(
        &runtime,
        &clankcord::ports::discord::DiscordApiUnavailable,
        job,
    )
    .await
    .unwrap();

    let resumed = store.get_agent_session_record("ags_source").await.unwrap();
    assert_eq!(resumed.state, AgentSessionRecordState::Active);
    assert_eq!(resumed.retired_at, "");
    assert_eq!(resumed.retirement_reason, "");
    assert_eq!(resumed.resumed_from_agent_session_id, "");
    assert_eq!(resumed.codex_session_id, "codex-session");
    assert_eq!(resumed.route_key, dm_route_key("user-a"));
    let event_scope = sqlx::query(
        r#"
        SELECT scope_kind, guild_id, scope_id, payload_json
        FROM timeline_events
        WHERE event_kind = 'agent_session_resumed'
        ORDER BY sequence DESC
        LIMIT 1
        "#,
    )
    .fetch_one(&store.pool)
    .await
    .unwrap();
    assert_eq!(
        sqlx::Row::try_get::<String, _>(&event_scope, "scope_kind").unwrap(),
        "dm"
    );
    assert_eq!(
        sqlx::Row::try_get::<String, _>(&event_scope, "guild_id").unwrap(),
        ""
    );
    assert_eq!(
        sqlx::Row::try_get::<String, _>(&event_scope, "scope_id").unwrap(),
        "user-a"
    );
    let event_payload =
        sqlx::Row::try_get::<serde_json::Value, _>(&event_scope, "payload_json").unwrap();
    assert_eq!(event_payload["scope_kind"], json!("dm"));
    assert_eq!(event_payload["scope_id"], json!("user-a"));
    assert!(event_payload.get("voice_channel_id").is_none());
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
async fn retired_start_session_does_not_spawn_agent_task() {
    let raw = tempfile::tempdir().unwrap();
    crate::support::initialize_test_config(raw.path());
    let store = crate::support::test_store(&raw.path().join("voice")).await;
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
    let runtime = Ctx::new(store.clone());

    clankcord::engine::dispatcher::dispatch_claimed_job(
        &runtime,
        &clankcord::ports::discord::DiscordApiUnavailable,
        running,
    )
    .await
    .unwrap();

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
async fn session_response_reroutes_after_resume_takeover() {
    let raw = tempfile::tempdir().unwrap();
    crate::support::initialize_test_config(raw.path());
    let store = crate::support::test_store(&raw.path().join("voice")).await;
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
    let runtime = Ctx::new(store.clone());

    clankcord::engine::dispatcher::dispatch_claimed_job(
        &runtime,
        &clankcord::ports::discord::DiscordApiUnavailable,
        running,
    )
    .await
    .unwrap();

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
async fn voice_resume_reactivates_source_thread_and_takes_over_active_route() {
    let raw = tempfile::tempdir().unwrap();
    crate::support::initialize_test_config(raw.path());
    let store = crate::support::test_store(&raw.path().join("voice")).await;
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
    let runtime = Ctx::new(store.clone());

    clankcord::engine::dispatcher::dispatch_claimed_job(
        &runtime,
        &clankcord::ports::discord::DiscordApiUnavailable,
        running,
    )
    .await
    .unwrap();

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
    crate::support::initialize_test_config(raw.path());
    let store = crate::support::test_store(&raw.path().join("voice")).await;
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
    clankcord::engine::dispatcher::dispatch_claimed_job(
        &runtime,
        &clankcord::ports::discord::DiscordApiUnavailable,
        running_resume,
    )
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
async fn session_text_delivery_reopens_deleted_stored_thread() {
    let raw = tempfile::tempdir().unwrap();
    crate::support::initialize_test_config(raw.path());
    let store = crate::support::test_store(&raw.path().join("voice")).await;
    let created_at = crate::support::dt(2026, 5, 17, 3, 28, 0);
    let max_active_until = created_at + chrono::Duration::hours(8);
    store
        .create_agent_session_record(AgentSessionRecord::new_voice(
            "ags_deleted_thread",
            "guild",
            "code",
            "agent-threads",
            "150000000000000001",
            created_at.to_rfc3339_opts(SecondsFormat::Millis, true),
            max_active_until.to_rfc3339_opts(SecondsFormat::Millis, true),
        ))
        .await
        .unwrap();
    let agent_task = store
        .create_job(Job::agent_task_for_session(
            "ags_deleted_thread",
            RuntimeScope::voice_channel("guild", "code"),
            "user-a",
            CommandRequest::agent_task("guild", "code", "user-a", "answer this"),
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
                "ready",
                agent_task.id.clone(),
                "user-a",
                false,
            ),
        ))
        .await
        .unwrap();
    let failed_send = store
        .create_child_job(
            &delivery,
            Job::discord_text_send(
                RuntimeScope::voice_channel("guild", "code"),
                "user-a",
                DiscordTextSendPayload {
                    intent: TextDeliveryKind::Message,
                    target: TextTarget {
                        kind: TextTargetKind::Channel,
                        channel_id: "150000000000000001".to_string(),
                        user_id: String::new(),
                    },
                    content: "ready".to_string(),
                    source_job_id: agent_task.id.clone(),
                    requested_by_user_id: "user-a".to_string(),
                    allowed_mentions: BinaryPayload::empty(),
                    components: BinaryPayload::empty(),
                    attachments: Vec::new(),
                },
            ),
        )
        .await
        .unwrap();
    let mut failed_send = failed_send;
    failed_send.set_state(JobState::Failed);
    failed_send.metadata.error = deleted_thread_error("150000000000000001", "messages");
    store.update_job(&failed_send).await.unwrap();
    store.resolve_waiting_jobs().await.unwrap();

    let runtime = Ctx::new(store.clone());
    let mut running = store.get_job(&delivery.id).await.unwrap();
    running.mark_running();
    store.update_job(&running).await.unwrap();
    clankcord::engine::dispatcher::dispatch_claimed_job(
        &runtime,
        &clankcord::ports::discord::DiscordApiUnavailable,
        running,
    )
    .await
    .unwrap();

    let cleared = store
        .get_agent_session_record("ags_deleted_thread")
        .await
        .unwrap();
    assert_eq!(cleared.discord_thread_id, "");
    assert_eq!(cleared.text_target.channel_id, "");
    let children = store.list_child_jobs(&delivery.id).await.unwrap();
    let thread_create = children
        .iter()
        .find(|child| child.kind == JobKind::DiscordForumThreadCreate)
        .expect("thread creation child");

    let mut completed_thread = thread_create.clone();
    completed_thread.mark_complete();
    completed_thread.metadata.output = Some(JobOutput::DiscordForumThreadCreate(
        DiscordForumThreadCreateOutput {
            parent_channel_id: "agent-threads".to_string(),
            thread_id: "150000000000000002".to_string(),
            name: "replacement".to_string(),
            source_job_id: delivery.id.clone(),
        },
    ));
    store.update_job(&completed_thread).await.unwrap();
    store.resolve_waiting_jobs().await.unwrap();

    let mut running = store.get_job(&delivery.id).await.unwrap();
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
        .get_agent_session_record("ags_deleted_thread")
        .await
        .unwrap();
    assert_eq!(updated.discord_thread_id, "150000000000000002");
    assert_eq!(updated.text_target.channel_id, "150000000000000002");
    let children = store.list_child_jobs(&delivery.id).await.unwrap();
    let replacement_send = children
        .iter()
        .find(|child| child.kind == JobKind::DiscordTextSend && child.id != failed_send.id)
        .expect("replacement send child");
    assert_eq!(
        replacement_send.payload.to_json()["target"]["channel_id"],
        "150000000000000002"
    );
    let events = store
        .load_events("guild", "code", None, None, None, None, false)
        .await
        .unwrap();
    assert!(events.iter().any(|event| {
        event["event_kind"] == json!("agent_session_thread_unavailable")
            && event["discord_thread_id"] == json!("150000000000000001")
    }));
}

#[tokio::test(flavor = "current_thread")]
async fn thread_title_refresh_marks_deleted_thread_unavailable() {
    let raw = tempfile::tempdir().unwrap();
    crate::support::initialize_test_config(raw.path());
    let store = crate::support::test_store(&raw.path().join("voice")).await;
    insert_active_thread_session(&store, "ags_title_deleted").await;
    let refresh = store
        .create_job(Job::agent_thread_title_refresh(
            "job_maintenance",
            "ags_title_deleted",
            "guild",
            "code",
            "thread-1",
            "Code Lounge",
            1,
        ))
        .await
        .unwrap();
    let rename = store
        .create_child_job(
            &refresh,
            Job::discord_forum_thread_rename(
                RuntimeScope::voice_channel("guild", "code"),
                "runtime",
                DiscordForumThreadRenamePayload {
                    thread_id: "thread-1".to_string(),
                    name: "New title".to_string(),
                    source_job_id: refresh.id.clone(),
                },
            ),
        )
        .await
        .unwrap();
    let mut failed_rename = rename;
    failed_rename.set_state(JobState::Failed);
    failed_rename.metadata.error = deleted_thread_error("thread-1", "");
    store.update_job(&failed_rename).await.unwrap();
    store.resolve_waiting_jobs().await.unwrap();

    let runtime = Ctx::new(store.clone());
    let mut running = store.get_job(&refresh.id).await.unwrap();
    running.mark_running();
    store.update_job(&running).await.unwrap();
    clankcord::engine::dispatcher::dispatch_claimed_job(
        &runtime,
        &clankcord::ports::discord::DiscordApiUnavailable,
        running,
    )
    .await
    .unwrap();

    let session = store
        .get_agent_session_record("ags_title_deleted")
        .await
        .unwrap();
    assert_eq!(session.discord_thread_id, "");
    assert_eq!(session.text_target.channel_id, "");
    let completed = store.get_job(&refresh.id).await.unwrap();
    assert_eq!(completed.state, JobState::Complete);
    assert_eq!(
        completed.metadata.output.unwrap().to_json()["status"],
        "skipped_unavailable_session_thread"
    );
    let events = store
        .load_events("guild", "code", None, None, None, None, false)
        .await
        .unwrap();
    assert!(events.iter().any(|event| {
        event["event_kind"] == json!("agent_thread_title_skipped")
            && event["status"] == json!("skipped_unavailable_session_thread")
    }));
}

#[tokio::test(flavor = "current_thread")]
async fn maintenance_queues_one_thread_title_refresh_after_one_visible_agent_response() {
    let raw = tempfile::tempdir().unwrap();
    crate::support::initialize_test_config(raw.path());
    let store = crate::support::test_store(&raw.path().join("voice")).await;
    insert_active_thread_session(&store, "ags_title").await;
    insert_completed_agent_response(
        &store,
        "ags_title",
        "explain gRPC",
        "gRPC uses HTTP/2 streams for service calls.",
        "user-a",
    )
    .await;
    let runtime = Ctx::new(store.clone());
    let maintenance = store
        .create_job(Job::runtime_maintenance(500))
        .await
        .unwrap();
    let mut running = maintenance.clone();
    running.mark_running();
    store.update_job(&running).await.unwrap();

    clankcord::engine::dispatcher::dispatch_claimed_job(
        &runtime,
        &clankcord::ports::discord::DiscordApiUnavailable,
        running,
    )
    .await
    .unwrap();

    let title_jobs = agent_thread_title_refresh_jobs(&store).await;
    assert_eq!(title_jobs.len(), 1);
    let JobPayload::AgentThreadTitleRefresh(payload) = &title_jobs[0].payload else {
        panic!("expected thread-title payload");
    };
    assert_eq!(payload.agent_session_id, "ags_title");
    assert_eq!(payload.discord_thread_id, "thread-1");
    assert_eq!(payload.response_count, 1);
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

fn deleted_thread_error(thread_id: &str, suffix: &str) -> String {
    let suffix = if suffix.is_empty() {
        String::new()
    } else {
        format!("/{suffix}")
    };
    format!(
        "discord api POST /channels/{thread_id}{suffix} failed (404): {{\"message\":\"Unknown Channel\",\"code\":10003}}"
    )
}

#[tokio::test(flavor = "current_thread")]
async fn wake_activation_reuses_active_session_without_thread() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    let runtime = test_runtime(store);
    let created_at = dt(2026, 5, 12, 15, 0, 0);
    runtime
        .store
        .create_agent_session_record(AgentSessionRecord::new_voice(
            "ags_active",
            "guild",
            "code",
            "agent-threads",
            "",
            created_at.to_rfc3339_opts(SecondsFormat::Millis, true),
            (Utc::now() + chrono::Duration::hours(8)).to_rfc3339_opts(SecondsFormat::Millis, true),
        ))
        .await
        .unwrap();
    let start = dt(2026, 5, 12, 16, 0, 0);
    let wake = append_event(
        &runtime.store,
        start,
        start + chrono::Duration::seconds(1),
        "Will",
        "user-a",
        "Hey Clanky",
        json!({"wake": true, "score": 0.88}),
        2,
    )
    .await;
    append_event(
        &runtime.store,
        start + chrono::Duration::seconds(3),
        start + chrono::Duration::seconds(4),
        "Will",
        "user-a",
        "say hello",
        json!({}),
        3,
    )
    .await;

    let scheduled = schedule_from_wake_event(&runtime, &wake).await.unwrap();
    let activation_job_id = string_field(&scheduled["job"], "job_id");
    let activation_job = runtime.store.get_job(&activation_job_id).await.unwrap();
    let payload = activation_job.wake_activation_payload().cloned().unwrap();
    let result = execute(&runtime, &activation_job, &payload).await.unwrap();

    assert_eq!(result["status"], json!("dispatched"));
    let task_job_id = string_field(&result["created"]["job"], "job_id");
    let task_job = runtime.store.get_job(&task_job_id).await.unwrap();
    assert_eq!(task_job.kind, JobKind::AgentTask);
    let JobPayload::AgentTask(payload) = &task_job.payload else {
        panic!("expected agent task payload");
    };
    assert_eq!(payload.agent_session_id, "ags_active");
    let jobs = runtime.store.list_jobs(Some("guild"), None).await.unwrap();
    assert!(
        !jobs
            .iter()
            .any(|job| job.kind == JobKind::AgentSessionStart)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn wake_activation_treats_resume_text_as_agent_request() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    let runtime = test_runtime(store);
    let start = dt(2026, 5, 12, 16, 0, 0);
    let wake = append_event(
        &runtime.store,
        start,
        start + chrono::Duration::seconds(1),
        "Will",
        "user-a",
        "Hey Clanky",
        json!({"wake": true, "score": 0.88}),
        2,
    )
    .await;
    append_event(
        &runtime.store,
        start + chrono::Duration::seconds(3),
        start + chrono::Duration::seconds(4),
        "Will",
        "user-a",
        "resume the session about banking",
        json!({}),
        3,
    )
    .await;

    let scheduled = schedule_from_wake_event(&runtime, &wake).await.unwrap();
    let activation_job_id = string_field(&scheduled["job"], "job_id");
    let activation_job = runtime.store.get_job(&activation_job_id).await.unwrap();
    let payload = activation_job.wake_activation_payload().cloned().unwrap();
    let result = execute(&runtime, &activation_job, &payload).await.unwrap();

    assert_eq!(result["status"], json!("dispatched"));
    let start_job_id = string_field(&result["created"]["job"], "job_id");
    let start_job = runtime.store.get_job(&start_job_id).await.unwrap();
    assert_eq!(start_job.kind, JobKind::AgentSessionStart);
    let JobPayload::AgentSessionStart(payload) = &start_job.payload else {
        panic!("expected agent-session start payload");
    };
    assert_eq!(
        payload.command.arguments.request,
        "resume the session about banking"
    );
    let jobs = runtime.store.list_jobs(Some("guild"), None).await.unwrap();
    assert!(
        !jobs
            .iter()
            .any(|job| job.kind == JobKind::DiscordForumThreadCreate)
    );
}
