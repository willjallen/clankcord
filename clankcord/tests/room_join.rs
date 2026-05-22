use clankcord::runtime::timeline::utc_now;
use clankcord::runtime::{
    DiscordVoiceJoinOutput, DiscordVoiceJoinPayload, DiscordVoicePlaybackCue, Job, JobKind,
    JobOutput, JobState, RoomAgentPlacementAction, RoomConfig, Runtime, VoiceBotStatus,
    VoiceCaptureSessionStatus,
};

mod common;
use common::{initialize_test_config, test_state_dir, test_store};

#[tokio::test(flavor = "current_thread")]
async fn join_room_placement_creates_discord_voice_join_child_job() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let _state = test_state_dir(raw.path()).await;
    let store = test_store(raw.path()).await;
    let room = test_room();
    let mut runtime = test_runtime(store.clone(), room.clone());
    store.upsert_voice_bot_state(&ready_bot()).await.unwrap();
    let mut placement = Job::room_agent_placement(
        &room.guild_id,
        &room.channel_id,
        &room.room_id,
        RoomAgentPlacementAction::Join,
        "auto_join",
        "test-placement",
        None,
    );
    placement.requested_by_user_id = "user-a".to_string();
    let parent = store.create_job(placement).await.unwrap();

    let result = runtime.dispatch_claimed_runtime_job(parent).await.unwrap();

    let child_ids = result["child_job_ids"].as_array().unwrap();
    assert_eq!(child_ids.len(), 1);
    let child = store.get_job(child_ids[0].as_str().unwrap()).await.unwrap();
    assert_eq!(child.kind, JobKind::DiscordVoiceJoin);
    let payload = child.discord_voice_join_payload().unwrap();
    assert_eq!(payload.room, room);
    assert_eq!(payload.bot_id, "clanky-vc1");
    assert_eq!(payload.requested_by_user_id, "user-a");
    assert!(!payload.capture_run_id.trim().is_empty());
    let assignments = store.list_active_voice_assignments().await.unwrap();
    assert_eq!(assignments.len(), 1);
    assert_eq!(assignments[0].state, "joining");
    assert_eq!(assignments[0].voice_bot_id, "clanky-vc1");
    assert_eq!(assignments[0].capture_run_id, payload.capture_run_id);
}

#[tokio::test(flavor = "current_thread")]
async fn join_room_placement_treats_pending_voice_join_as_channel_reservation() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let _state = test_state_dir(raw.path()).await;
    let store = test_store(raw.path()).await;
    let room = test_room();
    let pending = Job::discord_voice_join(DiscordVoiceJoinPayload {
        room: room.clone(),
        bot_id: "clanky-vc1".to_string(),
        capture_run_id: "cap_joining".to_string(),
        assignment_id: "assign_joining".to_string(),
        started_at: utc_now(),
        session_dir: raw.path().join("joining"),
        requested_by_user_id: "user-a".to_string(),
        reason: "explicit_request".to_string(),
    });
    store.create_job(pending).await.unwrap();
    let mut runtime = test_runtime(store.clone(), room.clone());
    store
        .upsert_voice_bot_state(&ready_bot_with("clanky-vc2", "bot-user-2"))
        .await
        .unwrap();
    let parent = store
        .create_job(Job::room_agent_placement(
            &room.guild_id,
            &room.channel_id,
            &room.room_id,
            RoomAgentPlacementAction::Join,
            "explicit_request",
            "test-placement",
            None,
        ))
        .await
        .unwrap();

    runtime
        .dispatch_claimed_runtime_job(parent.clone())
        .await
        .unwrap();

    let parent = store.get_job(&parent.id).await.unwrap();
    let Some(JobOutput::RoomAgentPlacement(output)) = parent.metadata.output else {
        panic!("expected room placement output");
    };
    assert_eq!(output.status, "already_joining");
    assert_eq!(output.bot_id, "clanky-vc1");
    assert_eq!(output.capture_run_id, "cap_joining");
    assert!(
        store
            .list_active_voice_assignments()
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn duplicate_voice_bot_sessions_for_room_returns_all_but_oldest_session() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let _state = test_state_dir(raw.path()).await;
    let store = test_store(raw.path()).await;
    let room = test_room();
    store
        .upsert_capture_session_status(&VoiceCaptureSessionStatus {
            session_id: "cap_newer".to_string(),
            guild_id: room.guild_id.clone(),
            voice_channel_id: room.channel_id.clone(),
            bot_id: "clanky-vc2".to_string(),
            started_at: "2026-05-15T00:00:02.000Z".to_string(),
            active: true,
            ..VoiceCaptureSessionStatus::default()
        })
        .await
        .unwrap();
    store
        .upsert_capture_session_status(&VoiceCaptureSessionStatus {
            session_id: "cap_older".to_string(),
            guild_id: room.guild_id.clone(),
            voice_channel_id: room.channel_id.clone(),
            bot_id: "clanky-vc1".to_string(),
            started_at: "2026-05-15T00:00:01.000Z".to_string(),
            active: true,
            ..VoiceCaptureSessionStatus::default()
        })
        .await
        .unwrap();

    let sessions = store
        .list_active_capture_sessions_for_room(&room.guild_id, &room.channel_id)
        .await
        .unwrap();
    let duplicates = sessions.iter().skip(1).collect::<Vec<_>>();

    assert_eq!(duplicates.len(), 1);
    assert_eq!(duplicates[0].session_id, "cap_newer");
    assert_eq!(sessions[0].session_id, "cap_older");
}

#[tokio::test(flavor = "current_thread")]
async fn room_placement_resume_commits_discord_voice_join_output() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let _state = test_state_dir(raw.path()).await;
    let store = test_store(raw.path()).await;
    let room = test_room();
    let mut runtime = test_runtime(store.clone(), room.clone());
    store.upsert_voice_bot_state(&ready_bot()).await.unwrap();
    let assignment = store
        .claim_voice_assignment_for_room(&room, "auto_join")
        .await
        .unwrap()
        .unwrap();
    let parent = store
        .create_job(Job::room_agent_placement(
            &room.guild_id,
            &room.channel_id,
            &room.room_id,
            RoomAgentPlacementAction::Join,
            "auto_join",
            "test-placement",
            None,
        ))
        .await
        .unwrap();
    let join_payload = DiscordVoiceJoinPayload {
        room: room.clone(),
        bot_id: assignment.voice_bot_id.clone(),
        capture_run_id: assignment.capture_run_id.clone(),
        assignment_id: assignment.assignment_id.clone(),
        started_at: utc_now(),
        session_dir: raw.path().join("session"),
        requested_by_user_id: "user-a".to_string(),
        reason: "auto_join".to_string(),
    };
    let child = store
        .create_child_job(&parent, Job::discord_voice_join(join_payload))
        .await
        .unwrap();
    let mut completed_child = store.get_job(&child.id).await.unwrap();
    completed_child.mark_complete();
    completed_child.metadata.output = Some(JobOutput::DiscordVoiceJoin(DiscordVoiceJoinOutput {
        status: "assigned".to_string(),
        session: Some(VoiceCaptureSessionStatus {
            session_id: assignment.capture_run_id.clone(),
            room_id: room.room_id.clone(),
            guild_id: room.guild_id.clone(),
            channel_id: room.channel_id.clone(),
            voice_channel_id: room.channel_id.clone(),
            channel_name: room.channel_name.clone(),
            bot_id: "clanky-vc1".to_string(),
            capture_run_id: assignment.capture_run_id.clone(),
            assignment_id: assignment.assignment_id.clone(),
            active: true,
            ..VoiceCaptureSessionStatus::default()
        }),
        bot_status: Some(VoiceBotStatus {
            bot_id: "clanky-vc1".to_string(),
            ready: true,
            ..VoiceBotStatus::default()
        }),
        message: String::new(),
    }));
    store.update_job(&completed_child).await.unwrap();

    let result = runtime
        .dispatch_claimed_runtime_job(parent.clone())
        .await
        .unwrap();

    let playback_ids = result["child_job_ids"].as_array().unwrap();
    assert_eq!(playback_ids.len(), 1);
    let playback_child = store
        .get_job(playback_ids[0].as_str().unwrap())
        .await
        .unwrap();
    assert_eq!(playback_child.kind, JobKind::DiscordVoicePlayback);
    assert_eq!(
        playback_child.discord_voice_playback_payload().unwrap().cue,
        DiscordVoicePlaybackCue::Join
    );
    let mut completed_playback = playback_child;
    completed_playback.set_state(JobState::Failed);
    completed_playback.metadata.error = "missing cue asset".to_string();
    store.update_job(&completed_playback).await.unwrap();

    runtime
        .dispatch_claimed_runtime_job(parent.clone())
        .await
        .unwrap();

    let parent = store.get_job(&parent.id).await.unwrap();
    let Some(JobOutput::RoomAgentPlacement(output)) = parent.metadata.output else {
        panic!("expected room placement output");
    };
    assert_eq!(output.status, "assigned");
    let sessions = store.list_active_capture_sessions().await.unwrap();
    assert_eq!(sessions[0].channel_id, room.channel_id);
    let assignments = store.list_active_voice_assignments().await.unwrap();
    assert_eq!(assignments[0].state, "capturing");
    assert_eq!(assignments[0].capture_run_id, assignment.capture_run_id);
}

#[tokio::test(flavor = "current_thread")]
async fn voice_status_sync_releases_capturing_assignment_when_bot_is_absent() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let _state = test_state_dir(raw.path()).await;
    let store = test_store(raw.path()).await;
    let room = test_room();
    store.upsert_voice_bot_state(&ready_bot()).await.unwrap();
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
    let mut stale_bot = ready_bot();
    stale_bot.current_guild_id = room.guild_id.clone();
    stale_bot.current_channel_id = room.channel_id.clone();
    store.upsert_voice_bot_state(&stale_bot).await.unwrap();
    let runtime = test_runtime(store.clone(), room.clone());

    runtime
        .sync_voice_adapter_status(vec![ready_bot()], Vec::new())
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
async fn voice_status_sync_keeps_joining_assignment_while_presence_is_pending() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let _state = test_state_dir(raw.path()).await;
    let store = test_store(raw.path()).await;
    let room = test_room();
    store.upsert_voice_bot_state(&ready_bot()).await.unwrap();
    let assignment = store
        .claim_voice_assignment_for_room(&room, "auto_join")
        .await
        .unwrap()
        .unwrap();
    store
        .upsert_capture_session_status(&capture_session_for_assignment(&room, &assignment))
        .await
        .unwrap();
    let runtime = test_runtime(store.clone(), room.clone());

    runtime
        .sync_voice_adapter_status(vec![ready_bot()], Vec::new())
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
async fn voice_status_sync_keeps_capturing_assignment_with_matching_bot_and_session() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let _state = test_state_dir(raw.path()).await;
    let store = test_store(raw.path()).await;
    let room = test_room();
    store.upsert_voice_bot_state(&ready_bot()).await.unwrap();
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
    let mut bot = ready_bot();
    bot.current_guild_id = room.guild_id.clone();
    bot.current_channel_id = room.channel_id.clone();
    let runtime = test_runtime(store.clone(), room.clone());

    runtime
        .sync_voice_adapter_status(vec![bot], vec![session])
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
async fn voice_assignment_claim_requires_idle_bot() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let _state = test_state_dir(raw.path()).await;
    let store = test_store(raw.path()).await;
    let room = test_room();
    let mut bot = ready_bot();
    bot.current_guild_id = room.guild_id.clone();
    bot.current_channel_id = "other-room".to_string();
    store.upsert_voice_bot_state(&bot).await.unwrap();

    let assignment = store
        .claim_voice_assignment_for_room(&room, "auto_join")
        .await
        .unwrap();

    assert!(assignment.is_none());
}

fn test_runtime(
    timeline_store: clankcord::runtime::timeline::TimelineStore,
    _room: RoomConfig,
) -> Runtime {
    Runtime::from_store(timeline_store).unwrap()
}

fn test_room() -> RoomConfig {
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

fn ready_bot() -> VoiceBotStatus {
    ready_bot_with("clanky-vc1", "bot-user")
}

fn ready_bot_with(bot_id: &str, user_id: &str) -> VoiceBotStatus {
    VoiceBotStatus {
        bot_id: bot_id.to_string(),
        ready: true,
        user_id: user_id.to_string(),
        username: bot_id.to_string(),
        ..VoiceBotStatus::default()
    }
}

fn capture_session_for_assignment(
    room: &RoomConfig,
    assignment: &clankcord::runtime::VoiceAssignment,
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
