//! The built-in room-placement automation: joins, releases, holds, grace timers, cooldowns.

use serde_json::json;

use clankcord::config::{ControlConfig, GuildConfig, PoolConfig};
use clankcord::domain::rooms::RoomConfig;
use clankcord::domain::voice::VoiceBotStatus;
use clankcord::model::job::{
    Job, JobKind, JobState, RoomAgentPlacementAction,
};
use clankcord::store::{TimelineStore, isoformat_z, utc_now};

use crate::support::test_store;
use crate::support::automations::{test_runtime, voice_state, voice_state_with_flags};

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
async fn write_test_runtime_config_with_pool(
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
fn test_pool_config() -> PoolConfig {
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
fn code_room() -> RoomConfig {
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
fn six_minutes_ago() -> String {
    isoformat_z(Some(utc_now() - chrono::Duration::minutes(6)))
}
fn ready_bot() -> VoiceBotStatus {
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
