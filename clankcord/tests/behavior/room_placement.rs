//! The built-in room-placement automation: joins, releases, holds, grace timers, cooldowns.

use serde_json::json;

use clankcord::domain::rooms::RoomConfig;
use clankcord::model::job::RoomAgentPlacementAction;
use clankcord::store::TimelineStore;

use crate::support::automations::{
    code_room, ready_bot, six_minutes_ago, test_pool_config, write_test_runtime_config_with_pool,
};
use crate::support::automations::{test_runtime, voice_state, voice_state_with_flags};
use crate::support::test_store;

#[tokio::test(flavor = "current_thread")]
async fn room_placement_builtin_automation_joins_rooms_with_two_participants() {
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
    let runtime = test_runtime(store.clone());

    let result = clankcord::domain::automations::engine::run_automations(&runtime)
        .await
        .unwrap()
        .to_json();

    let created = result["createdJobs"].as_array().unwrap();
    assert_eq!(created.len(), 1);
    let job_id = created[0]["job"]["job_id"].as_str().unwrap();
    let job = store.get_job(job_id).await.unwrap();
    let payload = job.room_agent_placement_payload().unwrap();
    assert_eq!(payload.action, RoomAgentPlacementAction::Join);
    assert_eq!(payload.reason, "auto_join");
    assert_eq!(payload.cooldown_seconds, None);

    let result = clankcord::domain::automations::engine::run_automations(&runtime)
        .await
        .unwrap()
        .to_json();

    assert_eq!(result["createdJobs"], json!([]));
}

#[tokio::test(flavor = "current_thread")]
async fn room_placement_builtin_automation_uses_configured_join_threshold() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    let mut pool = test_pool_config();
    pool.auto_join_min_participants = 3;
    write_test_runtime_config_with_pool(&store, &[code_room()], &pool).await;
    store.upsert_voice_bot_state(&ready_bot()).await.unwrap();
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

    store
        .record_voice_state_update(None, voice_state("code", "user-c", "User C"))
        .await
        .unwrap();

    let result = clankcord::domain::automations::engine::run_automations(&runtime)
        .await
        .unwrap()
        .to_json();

    assert_eq!(result["createdJobs"].as_array().unwrap().len(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn room_placement_builtin_automation_skips_rooms_without_auto_join() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    let room = banned_meetings_room();
    write_test_runtime_config(&store, std::slice::from_ref(&room)).await;
    store.upsert_voice_bot_state(&ready_bot()).await.unwrap();
    store
        .record_voice_state_update(None, voice_state(&room.channel_id, "user-a", "User A"))
        .await
        .unwrap();
    store
        .record_voice_state_update(None, voice_state(&room.channel_id, "user-b", "User B"))
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
async fn room_placement_builtin_automation_respects_active_auto_join_suppression() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    store.upsert_voice_bot_state(&ready_bot()).await.unwrap();
    let room = code_room();
    let runtime = test_runtime(store.clone());
    clankcord::domain::rooms::control_state::suppress_room_auto_join(
        &runtime,
        &room,
        5 * 60,
        "auto_policy_empty",
        "",
        true,
    )
    .await
    .unwrap();
    store
        .record_voice_state_update(None, voice_state("code", "user-a", "User A"))
        .await
        .unwrap();
    store
        .record_voice_state_update(None, voice_state("code", "user-b", "User B"))
        .await
        .unwrap();

    let result = clankcord::domain::automations::engine::run_automations(&runtime)
        .await
        .unwrap()
        .to_json();

    assert_eq!(result["createdJobs"], json!([]));
}

#[tokio::test(flavor = "current_thread")]
async fn room_placement_builtin_automation_manual_leave_suppresses_auto_join() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    store.upsert_voice_bot_state(&ready_bot()).await.unwrap();
    let room = code_room();
    let runtime = test_runtime(store.clone());
    clankcord::domain::rooms::control_state::suppress_room_auto_join(
        &runtime,
        &room,
        60 * 60,
        "manual_leave",
        "user-a",
        true,
    )
    .await
    .unwrap();
    store
        .record_voice_state_update(None, voice_state("code", "user-a", "User A"))
        .await
        .unwrap();
    store
        .record_voice_state_update(None, voice_state("code", "user-b", "User B"))
        .await
        .unwrap();

    let result = clankcord::domain::automations::engine::run_automations(&runtime)
        .await
        .unwrap()
        .to_json();

    assert_eq!(result["createdJobs"], json!([]));
}

#[tokio::test(flavor = "current_thread")]
async fn room_placement_builtin_automation_manual_leave_releases_present_bot() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    store.upsert_voice_bot_state(&ready_bot()).await.unwrap();
    let room = code_room();
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
        .record_voice_state_update(None, voice_state("code", "user-a", "User A"))
        .await
        .unwrap();
    store
        .record_voice_state_update(None, voice_state("code", "user-b", "User B"))
        .await
        .unwrap();
    let runtime = test_runtime(store.clone());
    clankcord::domain::rooms::control_state::suppress_room_auto_join(
        &runtime,
        &room,
        60 * 60,
        "manual_leave",
        "user-a",
        true,
    )
    .await
    .unwrap();

    let result = clankcord::domain::automations::engine::run_automations(&runtime)
        .await
        .unwrap()
        .to_json();

    let created = result["createdJobs"].as_array().unwrap();
    assert_eq!(created.len(), 1);
    let job_id = created[0]["job"]["job_id"].as_str().unwrap();
    let job = store.get_job(job_id).await.unwrap();
    let payload = job.room_agent_placement_payload().unwrap();
    assert_eq!(payload.action, RoomAgentPlacementAction::Leave);
    assert_eq!(payload.reason, "manual_leave");
    assert_eq!(payload.cooldown_seconds, Some(60 * 60));
}

#[tokio::test(flavor = "current_thread")]
async fn room_placement_builtin_automation_manual_hold_joins_with_one_participant() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    store.upsert_voice_bot_state(&ready_bot()).await.unwrap();
    let mut room = code_room();
    room.auto_join = false;
    write_test_runtime_config(&store, std::slice::from_ref(&room)).await;
    let runtime = test_runtime(store.clone());
    clankcord::domain::rooms::control_state::set_room_manual_hold(
        &runtime,
        &room,
        60 * 60,
        "explicit_request",
        "user-a",
    )
    .await
    .unwrap();
    store
        .record_voice_state_update(None, voice_state("code", "user-a", "User A"))
        .await
        .unwrap();

    let result = clankcord::domain::automations::engine::run_automations(&runtime)
        .await
        .unwrap()
        .to_json();

    let created = result["createdJobs"].as_array().unwrap();
    assert_eq!(created.len(), 1);
    let job_id = created[0]["job"]["job_id"].as_str().unwrap();
    let job = store.get_job(job_id).await.unwrap();
    let payload = job.room_agent_placement_payload().unwrap();
    assert_eq!(payload.action, RoomAgentPlacementAction::Join);
    assert_eq!(payload.reason, "manual_hold");
}

#[tokio::test(flavor = "current_thread")]
async fn room_placement_builtin_automation_leaves_empty_rooms_after_grace() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    store.upsert_voice_bot_state(&ready_bot()).await.unwrap();
    let room = code_room();
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
    let payload = job.room_agent_placement_payload().unwrap();
    assert_eq!(payload.action, RoomAgentPlacementAction::Leave);
    assert_eq!(payload.reason, "auto_policy_empty");
    assert_eq!(payload.cooldown_seconds, Some(5 * 60));
}

#[tokio::test(flavor = "current_thread")]
async fn room_placement_builtin_automation_waits_for_configured_empty_release_seconds() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    store.upsert_voice_bot_state(&ready_bot()).await.unwrap();
    let room = code_room();
    let mut pool = test_pool_config();
    pool.auto_leave_empty_seconds = 7 * 60;
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

    assert_eq!(result["createdJobs"], json!([]));
}

#[tokio::test(flavor = "current_thread")]
async fn room_placement_builtin_automation_uses_configured_rejoin_cooldown() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    store.upsert_voice_bot_state(&ready_bot()).await.unwrap();
    let room = code_room();
    let mut pool = test_pool_config();
    pool.auto_rejoin_cooldown_seconds = 777;
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
    let payload = job.room_agent_placement_payload().unwrap();
    assert_eq!(payload.cooldown_seconds, Some(777));
}

#[tokio::test(flavor = "current_thread")]
async fn room_placement_builtin_automation_leaves_single_deafened_participant_after_grace() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    store.upsert_voice_bot_state(&ready_bot()).await.unwrap();
    let room = code_room();
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

    let created = result["createdJobs"].as_array().unwrap();
    assert_eq!(created.len(), 1);
    let job_id = created[0]["job"]["job_id"].as_str().unwrap();
    let job = store.get_job(job_id).await.unwrap();
    let payload = job.room_agent_placement_payload().unwrap();
    assert_eq!(payload.action, RoomAgentPlacementAction::Leave);
    assert_eq!(payload.reason, "auto_policy_single_deafened");
    assert_eq!(payload.cooldown_seconds, Some(5 * 60));
}

#[tokio::test(flavor = "current_thread")]
async fn room_placement_builtin_automation_waits_for_configured_deafened_release_seconds() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    store.upsert_voice_bot_state(&ready_bot()).await.unwrap();
    let room = code_room();
    let mut pool = test_pool_config();
    pool.auto_leave_single_deafened_seconds = 7 * 60;
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
async fn room_placement_builtin_automation_keeps_manual_join_hold_with_deafened_participant() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    store.upsert_voice_bot_state(&ready_bot()).await.unwrap();
    let room = code_room();
    let assignment = store
        .claim_voice_assignment_for_room(&room, "explicit_request")
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
    clankcord::domain::rooms::control_state::set_room_manual_hold(
        &runtime,
        &room,
        60 * 60,
        "explicit_request",
        "user-a",
    )
    .await
    .unwrap();

    let result = clankcord::domain::automations::engine::run_automations(&runtime)
        .await
        .unwrap()
        .to_json();

    assert_eq!(result["createdJobs"], json!([]));
}

#[tokio::test(flavor = "current_thread")]
async fn room_placement_builtin_automation_empty_room_overrides_manual_hold() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    store.upsert_voice_bot_state(&ready_bot()).await.unwrap();
    let room = code_room();
    let assignment = store
        .claim_voice_assignment_for_room(&room, "explicit_request")
        .await
        .unwrap()
        .unwrap();
    store
        .mark_voice_assignment_capturing(&assignment.assignment_id)
        .await
        .unwrap();
    store
        .record_voice_state_update(None, voice_state("code", "user-a", "User A"))
        .await
        .unwrap();
    let mut left = voice_state("", "user-a", "User A");
    left["updated_at"] = json!(six_minutes_ago());
    store.record_voice_state_update(None, left).await.unwrap();
    let runtime = test_runtime(store.clone());
    clankcord::domain::rooms::control_state::set_room_manual_hold(
        &runtime,
        &room,
        60 * 60,
        "explicit_request",
        "user-a",
    )
    .await
    .unwrap();

    let result = clankcord::domain::automations::engine::run_automations(&runtime)
        .await
        .unwrap()
        .to_json();

    let created = result["createdJobs"].as_array().unwrap();
    assert_eq!(created.len(), 1);
    let job_id = created[0]["job"]["job_id"].as_str().unwrap();
    let job = store.get_job(job_id).await.unwrap();
    let payload = job.room_agent_placement_payload().unwrap();
    assert_eq!(payload.action, RoomAgentPlacementAction::Leave);
    assert_eq!(payload.reason, "auto_policy_empty");
}

async fn write_test_runtime_config(store: &TimelineStore, rooms: &[RoomConfig]) {
    write_test_runtime_config_with_pool(store, rooms, &test_pool_config()).await;
}

fn banned_meetings_room() -> RoomConfig {
    RoomConfig {
        room_id: "meetings".to_string(),
        guild_id: "guild".to_string(),
        guild_slug: "guild".to_string(),
        channel_id: "553025625737396288".to_string(),
        channel_slug: "meetings".to_string(),
        channel_name: "meetings".to_string(),
        auto_join: false,
    }
}
