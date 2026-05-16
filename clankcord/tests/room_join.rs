use std::collections::BTreeMap;

use clankcord::runtime::timeline::utc_now;
use clankcord::runtime::{
    AgentRuntime, ControlConfig, DiscordVoiceJoinOutput, DiscordVoiceJoinPayload,
    DiscordVoicePlaybackCue, Job, JobKind, JobOutput, JobState, RoomAgentPlacementAction,
    RoomConfig, Runtime, VoiceBotStatus, VoiceCaptureSessionStatus,
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
    runtime.bots.insert("clanky-vc1".to_string(), ready_bot());
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
    assert_eq!(
        runtime.bots.get("clanky-vc1").unwrap().joining_session_id,
        payload.capture_run_id
    );
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
    runtime.bots.insert(
        "clanky-vc2".to_string(),
        ready_bot_with("clanky-vc2", "bot-user-2"),
    );
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
    assert!(runtime.bots["clanky-vc2"].joining_session_id.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn duplicate_voice_bot_sessions_for_room_returns_all_but_oldest_session() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let _state = test_state_dir(raw.path()).await;
    let store = test_store(raw.path()).await;
    let room = test_room();
    let mut runtime = test_runtime(store, room.clone());
    runtime.sessions.insert(
        "cap_newer".to_string(),
        VoiceCaptureSessionStatus {
            session_id: "cap_newer".to_string(),
            guild_id: room.guild_id.clone(),
            voice_channel_id: room.channel_id.clone(),
            bot_id: "clanky-vc2".to_string(),
            started_at: "2026-05-15T00:00:02.000Z".to_string(),
            active: true,
            ..VoiceCaptureSessionStatus::default()
        },
    );
    runtime.sessions.insert(
        "cap_older".to_string(),
        VoiceCaptureSessionStatus {
            session_id: "cap_older".to_string(),
            guild_id: room.guild_id.clone(),
            voice_channel_id: room.channel_id.clone(),
            bot_id: "clanky-vc1".to_string(),
            started_at: "2026-05-15T00:00:01.000Z".to_string(),
            active: true,
            ..VoiceCaptureSessionStatus::default()
        },
    );

    let duplicates = runtime.duplicate_voice_bot_sessions_for_room(&room);

    assert_eq!(duplicates.len(), 1);
    assert_eq!(duplicates[0].session_id, "cap_newer");
    assert_eq!(
        runtime.active_session_id_for_room(&room).unwrap(),
        "cap_older"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn room_placement_resume_commits_discord_voice_join_output() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let _state = test_state_dir(raw.path()).await;
    let store = test_store(raw.path()).await;
    let room = test_room();
    let mut runtime = test_runtime(store.clone(), room.clone());
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
        bot_id: "clanky-vc1".to_string(),
        capture_run_id: "cap_1".to_string(),
        assignment_id: "assign_1".to_string(),
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
            session_id: "cap_1".to_string(),
            room_id: room.room_id.clone(),
            guild_id: room.guild_id.clone(),
            channel_id: room.channel_id.clone(),
            voice_channel_id: room.channel_id.clone(),
            channel_name: room.channel_name.clone(),
            bot_id: "clanky-vc1".to_string(),
            capture_run_id: "cap_1".to_string(),
            assignment_id: "assign_1".to_string(),
            active: true,
            ..VoiceCaptureSessionStatus::default()
        }),
        bot_status: Some(VoiceBotStatus {
            bot_id: "clanky-vc1".to_string(),
            ready: true,
            assigned_session_id: "cap_1".to_string(),
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
    assert_eq!(runtime.sessions["cap_1"].channel_id, room.channel_id);
    assert_eq!(runtime.bots["clanky-vc1"].assigned_session_id, "cap_1");
}

fn test_runtime(
    timeline_store: clankcord::runtime::timeline::TimelineStore,
    room: RoomConfig,
) -> Runtime {
    Runtime {
        started_at: utc_now(),
        guilds: BTreeMap::new(),
        rooms: BTreeMap::from([(room.room_id.clone(), room)]),
        control_config: ControlConfig::default(),
        sessions: BTreeMap::new(),
        bots: BTreeMap::new(),
        agents: AgentRuntime::default(),
        automations: BTreeMap::new(),
        timeline_store,
        auto_join_enabled: true,
        manual_leave_cooldown_seconds: 20 * 60,
        manual_join_hold_seconds: 60 * 60,
        pause_release_seconds: 20 * 60,
    }
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
