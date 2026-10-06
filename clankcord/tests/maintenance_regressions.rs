use chrono::{Duration, Utc};
use serde_json::json;

use clankcord::runtime::timeline::{TimelineStore, isoformat_z};
use clankcord::runtime::{
    DiscordVoiceStatusSnapshotOutput, Job, JobKind, JobOutput, JobState, RoomConfig, Runtime,
    VoiceBotStatus, VoiceCaptureSessionStatus,
};

mod common;
use common::{initialize_test_config, test_store};

#[tokio::test(flavor = "current_thread")]
async fn due_background_kinds_follow_oldest_ready_work() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    let now = Utc::now();
    for (mut job, age) in [
        (Job::runtime_maintenance(15_000), 30),
        (Job::voice_status_sync("old-tick"), 6 * 3600),
        (Job::agent_session_retirement("old-tick"), 5 * 3600),
        (Job::voice_status_sync("new-tick"), 10),
    ] {
        job.created_at = isoformat_z(Some(now - Duration::seconds(age)));
        job.next_run_at = Some(job.created_at.clone());
        store.create_job(job).await.unwrap();
    }
    let mut future = Job::ephemeral_job_gc("future-tick", 256);
    future.next_run_at = Some(isoformat_z(Some(now + Duration::hours(1))));
    store.create_job(future).await.unwrap();
    let mut complete = Job::automation_evaluation("finished-tick");
    complete.mark_complete();
    store.create_job(complete).await.unwrap();

    assert_eq!(
        store.due_job_kinds().await.unwrap(),
        vec![
            JobKind::VoiceStatusSync,
            JobKind::AgentSessionRetirement,
            JobKind::RuntimeMaintenance
        ]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn waiting_resolution_requeues_only_parents_with_finished_children() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    let blocked = store
        .create_job(Job::voice_status_sync("blocked-tick"))
        .await
        .unwrap();
    store
        .create_child_job(&blocked, Job::discord_voice_status_snapshot(&blocked.id))
        .await
        .unwrap();
    let ready = store
        .create_job(Job::voice_status_sync("ready-tick"))
        .await
        .unwrap();
    let mut child = Job::discord_voice_status_snapshot(&ready.id);
    child.mark_complete();
    store.create_child_job(&ready, child).await.unwrap();
    let mut orphan = Job::voice_status_sync("orphan-tick");
    orphan.mark_waiting();
    let orphan = store.create_job(orphan).await.unwrap();

    let resolved = store.resolve_waiting_jobs().await.unwrap();

    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0]["job_id"], ready.id);
    assert_eq!(
        store.get_job(&ready.id).await.unwrap().state,
        JobState::Queued
    );
    assert_eq!(
        store.get_job(&blocked.id).await.unwrap().state,
        JobState::Waiting
    );
    assert_eq!(
        store.get_job(&orphan.id).await.unwrap().state,
        JobState::Waiting
    );
}

#[tokio::test(flavor = "current_thread")]
async fn title_history_queries_preserve_session_scope_count_and_latest_tie() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    assert_eq!(
        store
            .agent_thread_title_refresh_attempt_count("guild", "code", "session")
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        store
            .latest_agent_thread_title("guild", "code", "session")
            .await
            .unwrap(),
        None
    );

    for (guild, channel, session, count) in [
        ("guild", "code", "session", 2),
        ("guild", "code", "session", 6),
        ("guild", "code", "other-session", 99),
        ("other-guild", "code", "session", 99),
        ("guild", "other-channel", "session", 99),
    ] {
        store
            .append_event(
                guild,
                channel,
                json!({
                    "event_kind": "agent_thread_title_refresh_attempted",
                    "agent_session_id": session, "response_count": count,
                }),
            )
            .await
            .unwrap();
    }
    for (count, title) in [(6, "first"), (6, " newest "), (3, "lower count"), (9, " ")] {
        store
            .append_event(
                "guild",
                "code",
                json!({
                    "event_kind": "agent_thread_titled", "agent_session_id": "session",
                    "response_count": count, "title": title,
                }),
            )
            .await
            .unwrap();
    }
    let forgotten = store
        .append_event(
            "guild",
            "code",
            json!({
                "event_kind": "agent_thread_title_refresh_attempted", "agent_session_id": "session",
                "response_count": 100,
            }),
        )
        .await
        .unwrap();
    sqlx::query("UPDATE timeline_events SET forgotten = TRUE WHERE event_id = $1")
        .bind(forgotten["event_id"].as_str().unwrap())
        .execute(&store.pool)
        .await
        .unwrap();
    store.append_event("guild", "code", json!({
        "event_kind": "transcript", "response_count": "unrelated", "text": "history".repeat(10000),
    })).await.unwrap();

    assert_eq!(
        store
            .agent_thread_title_refresh_attempt_count("guild", "code", "session")
            .await
            .unwrap(),
        6
    );
    assert_eq!(
        store
            .latest_agent_thread_title("guild", "code", "session")
            .await
            .unwrap(),
        Some("newest".to_string())
    );
}

#[tokio::test(flavor = "current_thread")]
async fn delayed_voice_snapshot_preserves_capture_started_after_observation() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(raw.path()).await;
    let observed_at = Utc::now() - Duration::seconds(1);
    let (assignment, _, _) = active_capture(&store).await;
    let runtime = Runtime::from_store(store.clone()).unwrap();

    runtime
        .sync_voice_adapter_status(observed_at, vec![], vec![], vec![], vec![])
        .await
        .unwrap();

    assert_eq!(
        store.list_active_voice_assignments().await.unwrap()[0].assignment_id,
        assignment
    );
    assert!(store.list_active_capture_sessions().await.unwrap()[0].active);
}

#[tokio::test(flavor = "current_thread")]
async fn older_voice_snapshot_cannot_overwrite_newer_observation() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(raw.path()).await;
    let (_, bot, session) = active_capture(&store).await;
    let runtime = Runtime::from_store(store.clone()).unwrap();
    let observed_at = Utc::now();
    runtime
        .sync_voice_adapter_status(observed_at, vec![bot], vec![session], vec![], vec![])
        .await
        .unwrap();
    runtime
        .sync_voice_adapter_status(
            observed_at - Duration::seconds(1),
            vec![],
            vec![],
            vec![],
            vec![],
        )
        .await
        .unwrap();

    assert_eq!(
        store.list_active_voice_assignments().await.unwrap().len(),
        1
    );
    assert_eq!(store.list_active_capture_sessions().await.unwrap().len(), 1);
    assert_eq!(
        store
            .voice_adapter_snapshot_observed_at()
            .await
            .unwrap()
            .unwrap()
            .timestamp_millis(),
        observed_at.timestamp_millis()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn expired_voice_snapshot_is_replaced_and_fresh_child_is_applied() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(raw.path()).await;
    let (_, bot, session) = active_capture(&store).await;
    let parent = store
        .create_job(Job::voice_status_sync("old-tick"))
        .await
        .unwrap();
    let mut expired = Job::discord_voice_status_snapshot(&parent.id);
    expired.mark_running();
    expired.started_at = Some(isoformat_z(Some(Utc::now() - Duration::minutes(2))));
    expired.mark_complete();
    expired.metadata.output = Some(JobOutput::DiscordVoiceStatusSnapshot(
        DiscordVoiceStatusSnapshotOutput {
            bots: vec![],
            sessions: vec![],
            voice_state_guild_ids: vec![],
            voice_states: vec![],
        },
    ));
    store.create_child_job(&parent, expired).await.unwrap();
    let mut runtime = Runtime::from_store(store.clone()).unwrap();
    let mut running = store.get_job(&parent.id).await.unwrap();
    running.mark_running();
    store.update_job(&running).await.unwrap();
    let result = runtime.dispatch_claimed_runtime_job(running).await.unwrap();
    let fresh_id = result["child_job_ids"][0].as_str().unwrap();
    assert_eq!(
        store.list_active_voice_assignments().await.unwrap().len(),
        1
    );
    assert!(
        store
            .voice_adapter_snapshot_observed_at()
            .await
            .unwrap()
            .is_none()
    );

    let mut fresh = store.get_job(fresh_id).await.unwrap();
    fresh.mark_running();
    fresh.mark_complete();
    fresh.metadata.output = Some(JobOutput::DiscordVoiceStatusSnapshot(
        DiscordVoiceStatusSnapshotOutput {
            bots: vec![bot],
            sessions: vec![session],
            voice_state_guild_ids: vec![],
            voice_states: vec![],
        },
    ));
    store.update_job(&fresh).await.unwrap();
    store.resolve_waiting_jobs().await.unwrap();
    let mut running = store.get_job(&parent.id).await.unwrap();
    running.mark_running();
    store.update_job(&running).await.unwrap();
    runtime.dispatch_claimed_runtime_job(running).await.unwrap();

    assert_eq!(
        store.get_job(&parent.id).await.unwrap().state,
        JobState::Complete
    );
    assert_eq!(
        store.list_active_voice_assignments().await.unwrap().len(),
        1
    );
    assert!(
        store
            .voice_adapter_snapshot_observed_at()
            .await
            .unwrap()
            .is_some()
    );
}

async fn active_capture(
    store: &TimelineStore,
) -> (String, VoiceBotStatus, VoiceCaptureSessionStatus) {
    let room = RoomConfig {
        room_id: "code-lounge".into(),
        guild_id: "guild".into(),
        guild_slug: "guild".into(),
        channel_id: "code".into(),
        channel_slug: "code-lounge".into(),
        channel_name: "Code Lounge".into(),
        auto_join: true,
    };
    let mut bot = VoiceBotStatus {
        bot_id: "voice-1".into(),
        user_id: "bot-user".into(),
        ready: true,
        gateway_running: true,
        ..VoiceBotStatus::default()
    };
    store.upsert_voice_bot_state(&bot).await.unwrap();
    let assignment = store
        .claim_voice_assignment_for_room(&room, "auto_join")
        .await
        .unwrap()
        .unwrap();
    store
        .mark_voice_assignment_capturing(&assignment.assignment_id)
        .await
        .unwrap();
    bot.current_guild_id = room.guild_id.clone();
    bot.current_channel_id = room.channel_id.clone();
    let session = VoiceCaptureSessionStatus {
        session_id: assignment.capture_run_id.clone(),
        capture_run_id: assignment.capture_run_id,
        assignment_id: assignment.assignment_id.clone(),
        bot_id: bot.bot_id.clone(),
        guild_id: room.guild_id,
        voice_channel_id: room.channel_id,
        active: true,
        started_at: assignment.assigned_at,
        ..VoiceCaptureSessionStatus::default()
    };
    store.upsert_capture_session_status(&session).await.unwrap();
    (assignment.assignment_id, bot, session)
}
