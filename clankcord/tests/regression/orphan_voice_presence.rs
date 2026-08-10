//! Pins for orphaned voice-bot presence: stale rows reconcile, orphans
//! release, and restart sync does not re-join
//! (a961598, f571acd, f3ae9b9, 1ba9f3b).

use crate::support::automations::test_runtime;
use crate::support::automations::voice_state;
use crate::support::automations::voice_state_with_flags;
use crate::support::automations::{
    code_room, ready_bot, six_minutes_ago, test_pool_config, write_test_runtime_config_with_pool,
};
use crate::support::initialize_test_config;
use crate::support::rooms::{ready_bot_with, room_runtime, test_room};
use crate::support::test_state_dir;
use crate::support::test_store;
use clankcord::domain::rooms::RoomConfig;
use clankcord::domain::voice::VoiceCaptureSessionStatus;
use clankcord::model::job::Job;
use clankcord::model::job::JobKind;
use clankcord::model::job::JobState;
use clankcord::model::job::RoomAgentPlacementAction;
use clankcord::store::utc_now;
use serde_json::json;

#[tokio::test(flavor = "current_thread")]
async fn room_placement_builtin_automation_releases_orphan_voice_bot_presence() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    let mut bot = ready_bot();
    bot.current_guild_id = "guild".to_string();
    bot.current_channel_id = "code".to_string();
    store.upsert_voice_bot_state(&bot).await.unwrap();
    store
        .record_voice_state_update(None, voice_state("code", "user-a", "User A"))
        .await
        .unwrap();
    store
        .record_voice_state_update(None, voice_state("code", "user-b", "User B"))
        .await
        .unwrap();
    let runtime = test_runtime(store.clone());

    let result = clankcord::domain::automations::engine::run_automations(&runtime)
        .await
        .unwrap()
        .to_json();

    let created = result["createdJobs"].as_array().unwrap();
    assert_eq!(created.len(), 1);
    let job_id = created[0]["job"]["job_id"].as_str().unwrap();
    let job = store.get_job(job_id).await.unwrap();
    assert_eq!(job.kind, JobKind::DiscordVoiceLeave);
    assert_eq!(job.guild_id, "guild");
    assert_eq!(job.scope_id, "code");
    let payload = job.discord_voice_leave_payload().unwrap();
    assert_eq!(payload.session_id, "");
    assert_eq!(payload.reason, "orphan_voice_bot_presence");
}

#[tokio::test(flavor = "current_thread")]
async fn room_placement_builtin_automation_disables_single_deafened_release_at_zero_seconds() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    store.upsert_voice_bot_state(&ready_bot()).await.unwrap();
    let room = code_room();
    let mut pool = test_pool_config();
    pool.auto_leave_single_deafened_seconds = 0;
    write_test_runtime_config_with_pool(&store, std::slice::from_ref(&room), &pool).await;
    let assignment = store
        .claim_voice_assignment_for_room(&room, "auto_join")
        .await
        .unwrap()
        .unwrap();
    store
        .mark_voice_assignment_capturing(&assignment.assignment_id)
        .await
        .unwrap();
    let mut state = voice_state_with_flags("code", "user-a", "User A", false, false, false, true);
    state["updated_at"] = json!(six_minutes_ago());
    store.record_voice_state_update(None, state).await.unwrap();
    let runtime = test_runtime(store.clone());

    let result = clankcord::domain::automations::engine::run_automations(&runtime)
        .await
        .unwrap()
        .to_json();

    assert_eq!(result["createdJobs"], json!([]));
}

#[tokio::test(flavor = "current_thread")]
async fn room_placement_builtin_automation_groups_orphan_voice_bot_presence_by_channel() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    let mut first_bot = ready_bot();
    first_bot.current_guild_id = "guild".to_string();
    first_bot.current_channel_id = "code".to_string();
    let mut second_bot = ready_bot();
    second_bot.bot_id = "clanky-vc2".to_string();
    second_bot.user_id = "bot-user-2".to_string();
    second_bot.current_guild_id = "guild".to_string();
    second_bot.current_channel_id = "code".to_string();
    store.upsert_voice_bot_state(&first_bot).await.unwrap();
    store.upsert_voice_bot_state(&second_bot).await.unwrap();
    let runtime = test_runtime(store.clone());

    let result = clankcord::domain::automations::engine::run_automations(&runtime)
        .await
        .unwrap()
        .to_json();

    let created = result["createdJobs"].as_array().unwrap();
    assert_eq!(created.len(), 1);
    let job_id = created[0]["job"]["job_id"].as_str().unwrap();
    let job = store.get_job(job_id).await.unwrap();
    assert_eq!(job.kind, JobKind::DiscordVoiceLeave);
    assert_eq!(job.guild_id, "guild");
    assert_eq!(job.scope_id, "code");
    let payload = job.discord_voice_leave_payload().unwrap();
    assert_eq!(payload.session_id, "");
    assert_eq!(payload.reason, "orphan_voice_bot_presence");
}

#[tokio::test(flavor = "current_thread")]
async fn room_placement_builtin_automation_releases_orphan_bot_outside_configured_rooms() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    let mut bot = ready_bot();
    bot.current_guild_id = "guild".to_string();
    bot.current_channel_id = "env".to_string();
    store.upsert_voice_bot_state(&bot).await.unwrap();
    store
        .record_voice_state_update(None, voice_state("code", "user-a", "User A"))
        .await
        .unwrap();
    store
        .record_voice_state_update(None, voice_state("code", "user-b", "User B"))
        .await
        .unwrap();
    let runtime = test_runtime(store.clone());

    let result = clankcord::domain::automations::engine::run_automations(&runtime)
        .await
        .unwrap()
        .to_json();

    let created = result["createdJobs"].as_array().unwrap();
    assert_eq!(created.len(), 1);
    let job_id = created[0]["job"]["job_id"].as_str().unwrap();
    let job = store.get_job(job_id).await.unwrap();
    assert_eq!(job.kind, JobKind::DiscordVoiceLeave);
    assert_eq!(job.guild_id, "guild");
    assert_eq!(job.scope_id, "env");
    let payload = job.discord_voice_leave_payload().unwrap();
    assert_eq!(payload.session_id, "");
    assert_eq!(payload.reason, "orphan_voice_bot_presence");
}

#[tokio::test(flavor = "current_thread")]
async fn room_placement_builtin_automation_uses_only_direct_leave_for_empty_orphan_room() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    let mut bot = ready_bot();
    bot.current_guild_id = "guild".to_string();
    bot.current_channel_id = "code".to_string();
    store.upsert_voice_bot_state(&bot).await.unwrap();
    store
        .record_voice_state_update(None, voice_state("code", "user-a", "User A"))
        .await
        .unwrap();
    let mut left = voice_state("", "user-a", "User A");
    left["updated_at"] = json!(six_minutes_ago());
    store.record_voice_state_update(None, left).await.unwrap();
    let runtime = test_runtime(store.clone());

    let result = clankcord::domain::automations::engine::run_automations(&runtime)
        .await
        .unwrap()
        .to_json();

    let created = result["createdJobs"].as_array().unwrap();
    assert_eq!(created.len(), 1);
    let job_id = created[0]["job"]["job_id"].as_str().unwrap();
    let job = store.get_job(job_id).await.unwrap();
    assert_eq!(job.kind, JobKind::DiscordVoiceLeave);
    let payload = job.discord_voice_leave_payload().unwrap();
    assert_eq!(payload.session_id, "");
    assert_eq!(payload.reason, "orphan_voice_bot_presence");
}

#[tokio::test(flavor = "current_thread")]
async fn room_placement_builtin_automation_waits_for_pending_orphan_disconnect() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    let mut bot = ready_bot();
    bot.current_guild_id = "guild".to_string();
    bot.current_channel_id = "code".to_string();
    bot.pending_disconnect_events = 1;
    bot.pending_disconnect_until = utc_now().timestamp_millis() + 60_000;
    store.upsert_voice_bot_state(&bot).await.unwrap();
    store
        .record_voice_state_update(None, voice_state("code", "user-a", "User A"))
        .await
        .unwrap();
    store
        .record_voice_state_update(None, voice_state("code", "user-b", "User B"))
        .await
        .unwrap();
    let runtime = test_runtime(store.clone());

    let result = clankcord::domain::automations::engine::run_automations(&runtime)
        .await
        .unwrap()
        .to_json();

    assert_eq!(result["createdJobs"], json!([]));
}

#[tokio::test(flavor = "current_thread")]
async fn room_placement_restart_sync_prevents_stale_voice_rows_from_triggering_auto_join() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    store.upsert_voice_bot_state(&ready_bot()).await.unwrap();
    store
        .record_voice_state_update(None, voice_state("code", "user-a", "User A"))
        .await
        .unwrap();
    store
        .record_voice_state_update(None, voice_state("code", "user-b", "User B"))
        .await
        .unwrap();
    let mut stale_parent = Job::room_agent_placement(
        "guild",
        "code",
        "code-lounge",
        RoomAgentPlacementAction::Join,
        "auto_join",
        "stuck-join-cue",
        None,
    );
    stale_parent.mark_waiting();
    let mut stale_parent = store.create_job(stale_parent).await.unwrap();
    stale_parent.set_state(JobState::FailedTimeout);
    store.update_job(&stale_parent).await.unwrap();
    let restarted = test_runtime(store.clone());
    clankcord::domain::maintenance::voice_status::sync_voice_adapter_status(
        &restarted,
        vec![ready_bot()],
        Vec::new(),
        vec!["guild".to_string()],
        Vec::new(),
    )
    .await
    .unwrap();

    let result = clankcord::domain::automations::engine::run_automations(&restarted)
        .await
        .unwrap()
        .to_json();

    assert!(
        store
            .room_occupants("guild", "code")
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .list_active_voice_assignments()
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(result["createdJobs"], json!([]));
}

#[tokio::test(flavor = "current_thread")]
async fn room_placement_restart_with_recorded_empty_room_does_not_rejoin_after_parent_clear() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    store.upsert_voice_bot_state(&ready_bot()).await.unwrap();
    store
        .record_voice_state_update(None, voice_state("code", "user-a", "User A"))
        .await
        .unwrap();
    store
        .record_voice_state_update(None, voice_state("code", "user-b", "User B"))
        .await
        .unwrap();
    let mut stale_parent = Job::room_agent_placement(
        "guild",
        "code",
        "code-lounge",
        RoomAgentPlacementAction::Join,
        "auto_join",
        "stuck-join-cue",
        None,
    );
    stale_parent.mark_waiting();
    let mut stale_parent = store.create_job(stale_parent).await.unwrap();
    stale_parent.set_state(JobState::FailedTimeout);
    store.update_job(&stale_parent).await.unwrap();
    store
        .record_voice_state_update(None, voice_state("", "user-a", "User A"))
        .await
        .unwrap();
    store
        .record_voice_state_update(None, voice_state("", "user-b", "User B"))
        .await
        .unwrap();
    let restarted = test_runtime(store.clone());

    let result = clankcord::domain::automations::engine::run_automations(&restarted)
        .await
        .unwrap()
        .to_json();

    assert!(
        store
            .room_occupants("guild", "code")
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(result["createdJobs"], json!([]));
}

#[tokio::test(flavor = "current_thread")]
async fn leave_room_placement_disconnects_orphan_voice_bot_presence() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let _state = test_state_dir(raw.path()).await;
    let store = test_store(raw.path()).await;
    let room = test_room();
    let mut bot = ready_bot_with("clanky-vc1", "bot-user");
    bot.current_guild_id = room.guild_id.clone();
    bot.current_channel_id = room.channel_id.clone();
    store.upsert_voice_bot_state(&bot).await.unwrap();
    let runtime = room_runtime(store.clone(), room.clone());
    let parent = store
        .create_job(Job::room_agent_placement(
            &room.guild_id,
            &room.channel_id,
            &room.room_id,
            RoomAgentPlacementAction::Leave,
            "orphan_voice_bot_presence",
            "test-placement",
            Some(0),
        ))
        .await
        .unwrap();

    let result = clankcord::engine::dispatcher::dispatch_claimed_runtime_job(
        &runtime,
        &clankcord::ports::discord::DiscordApiUnavailable,
        parent,
    )
    .await
    .unwrap();

    let child_ids = result["child_job_ids"].as_array().unwrap();
    assert_eq!(child_ids.len(), 1);
    let child = store.get_job(child_ids[0].as_str().unwrap()).await.unwrap();
    assert_eq!(child.kind, JobKind::DiscordVoiceLeave);
    assert_eq!(child.guild_id, room.guild_id);
    assert_eq!(child.scope_id, room.channel_id);
    let payload = child.discord_voice_leave_payload().unwrap();
    assert_eq!(payload.session_id, "");
    assert_eq!(payload.reason, "orphan_voice_bot_presence");
}

#[tokio::test(flavor = "current_thread")]
async fn voice_assignment_claim_requires_idle_bot() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let _state = test_state_dir(raw.path()).await;
    let store = test_store(raw.path()).await;
    let room = test_room();
    let mut bot = ready_bot_with("clanky-vc1", "bot-user");
    bot.current_guild_id = room.guild_id.clone();
    bot.current_channel_id = "other-room".to_string();
    store.upsert_voice_bot_state(&bot).await.unwrap();

    let assignment = store
        .claim_voice_assignment_for_room(&room, "auto_join")
        .await
        .unwrap();

    assert!(assignment.is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn voice_status_sync_keeps_capturing_assignment_with_matching_bot_and_session() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let _state = test_state_dir(raw.path()).await;
    let store = test_store(raw.path()).await;
    let room = test_room();
    store
        .upsert_voice_bot_state(&ready_bot_with("clanky-vc1", "bot-user"))
        .await
        .unwrap();
    let assignment = store
        .claim_voice_assignment_for_room(&room, "auto_join")
        .await
        .unwrap()
        .unwrap();
    store
        .mark_voice_assignment_capturing(&assignment.assignment_id)
        .await
        .unwrap();
    let session = capture_session_for_assignment(&room, &assignment);
    let mut bot = ready_bot_with("clanky-vc1", "bot-user");
    bot.current_guild_id = room.guild_id.clone();
    bot.current_channel_id = room.channel_id.clone();
    let runtime = room_runtime(store.clone(), room.clone());

    clankcord::domain::maintenance::voice_status::sync_voice_adapter_status(
        &runtime,
        vec![bot],
        vec![session],
        Vec::new(),
        Vec::new(),
    )
    .await
    .unwrap();

    let assignments = store.list_active_voice_assignments().await.unwrap();
    assert_eq!(assignments.len(), 1);
    assert_eq!(assignments[0].state, "capturing");
    let sessions = store.list_active_capture_sessions().await.unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].session_id, assignment.capture_run_id);
}

#[tokio::test(flavor = "current_thread")]
async fn voice_status_sync_keeps_joining_assignment_while_presence_is_pending() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let _state = test_state_dir(raw.path()).await;
    let store = test_store(raw.path()).await;
    let room = test_room();
    store
        .upsert_voice_bot_state(&ready_bot_with("clanky-vc1", "bot-user"))
        .await
        .unwrap();
    let assignment = store
        .claim_voice_assignment_for_room(&room, "auto_join")
        .await
        .unwrap()
        .unwrap();
    store
        .upsert_capture_session_status(&capture_session_for_assignment(&room, &assignment))
        .await
        .unwrap();
    let runtime = room_runtime(store.clone(), room.clone());

    clankcord::domain::maintenance::voice_status::sync_voice_adapter_status(
        &runtime,
        vec![ready_bot_with("clanky-vc1", "bot-user")],
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )
    .await
    .unwrap();

    let assignments = store.list_active_voice_assignments().await.unwrap();
    assert_eq!(assignments.len(), 1);
    assert_eq!(assignments[0].state, "joining");
    let sessions = store.list_active_capture_sessions().await.unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].session_id, assignment.capture_run_id);
}

#[tokio::test(flavor = "current_thread")]
async fn voice_status_sync_releases_capturing_assignment_when_bot_is_absent() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let _state = test_state_dir(raw.path()).await;
    let store = test_store(raw.path()).await;
    let room = test_room();
    store
        .upsert_voice_bot_state(&ready_bot_with("clanky-vc1", "bot-user"))
        .await
        .unwrap();
    let assignment = store
        .claim_voice_assignment_for_room(&room, "auto_join")
        .await
        .unwrap()
        .unwrap();
    store
        .mark_voice_assignment_capturing(&assignment.assignment_id)
        .await
        .unwrap();
    store
        .upsert_capture_session_status(&capture_session_for_assignment(&room, &assignment))
        .await
        .unwrap();
    let mut stale_bot = ready_bot_with("clanky-vc1", "bot-user");
    stale_bot.current_guild_id = room.guild_id.clone();
    stale_bot.current_channel_id = room.channel_id.clone();
    store.upsert_voice_bot_state(&stale_bot).await.unwrap();
    let runtime = room_runtime(store.clone(), room.clone());

    clankcord::domain::maintenance::voice_status::sync_voice_adapter_status(
        &runtime,
        vec![ready_bot_with("clanky-vc1", "bot-user")],
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )
    .await
    .unwrap();

    assert!(
        store
            .list_active_voice_assignments()
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .list_active_capture_sessions()
            .await
            .unwrap()
            .is_empty()
    );
    let released = store
        .get_voice_assignment(&assignment.assignment_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(released.state, "ended");
    assert_eq!(released.release_reason, "adapter_sync_missing");
    assert!(!released.released_at.trim().is_empty());
    let session = store
        .get_capture_session_status(&assignment.capture_run_id)
        .await
        .unwrap()
        .unwrap();
    assert!(!session.active);
    assert!(!session.ended_at.trim().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn voice_assignment_claim_skips_pending_disconnect_bot() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let _state = test_state_dir(raw.path()).await;
    let store = test_store(raw.path()).await;
    let room = test_room();
    let mut bot = ready_bot_with("clanky-vc1", "bot-user");
    bot.pending_disconnect_events = 1;
    bot.pending_disconnect_until = utc_now().timestamp_millis() + 60_000;
    store.upsert_voice_bot_state(&bot).await.unwrap();

    let assignment = store
        .claim_voice_assignment_for_room(&room, "auto_join")
        .await
        .unwrap();

    assert!(assignment.is_none());
}

fn capture_session_for_assignment(
    room: &RoomConfig,
    assignment: &clankcord::domain::voice::VoiceAssignment,
) -> VoiceCaptureSessionStatus {
    VoiceCaptureSessionStatus {
        session_id: assignment.capture_run_id.clone(),
        room_id: room.room_id.clone(),
        guild_id: room.guild_id.clone(),
        channel_id: room.channel_id.clone(),
        voice_channel_id: room.channel_id.clone(),
        channel_name: room.channel_name.clone(),
        bot_id: assignment.voice_bot_id.clone(),
        bot_user_id: assignment.voice_bot_discord_user_id.clone(),
        capture_run_id: assignment.capture_run_id.clone(),
        assignment_id: assignment.assignment_id.clone(),
        mode: "local_buffering".to_string(),
        started_at: assignment.assigned_at.clone(),
        active: true,
        ..VoiceCaptureSessionStatus::default()
    }
}
