use serde_json::json;

use clankcord::runtime::timeline::TimelineStore;
use clankcord::runtime::{
    CommandKind, CommandRequest, Job, JobKind, RoomConfig, Runtime, RuntimeScope,
    VoiceCaptureSessionStatus,
};

mod common;
use common::{initialize_test_config, test_store};

#[tokio::test(flavor = "current_thread")]
async fn pause_and_resume_room_controls_are_timeline_store_state() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(raw.path()).await;
    let room = test_room();
    let mut runtime = test_runtime(store.clone(), room.clone());

    runtime.pause_room(&room, 60, "user-a").await.unwrap();

    let stored = store
        .get_room_control(&room.guild_id, &room.channel_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.voice_channel_id, room.channel_id);
    assert_eq!(
        stored.listening_pause_reason.as_deref(),
        Some("manual_pause")
    );
    assert_eq!(
        stored.listening_paused_by_user_id.as_deref(),
        Some("user-a")
    );
    assert!(stored.listening_paused_until.is_some());

    let fresh_runtime = test_runtime(store.clone(), room.clone());
    let status = fresh_runtime.room_control_status(&room).await.unwrap();
    assert_eq!(status["listeningPaused"], json!(true));
    assert!(
        fresh_runtime
            .room_controls_json()
            .await
            .unwrap()
            .contains_key(&room.channel_id)
    );

    runtime.resume_room(&room, "user-a").await.unwrap();

    assert!(
        store
            .get_room_control(&room.guild_id, &room.channel_id)
            .await
            .unwrap()
            .is_none()
    );
    let fresh_runtime = test_runtime(store, room.clone());
    let status = fresh_runtime.room_control_status(&room).await.unwrap();
    assert_eq!(status["listeningPaused"], json!(false));
    assert_eq!(status["control"], json!({}));
}

#[tokio::test(flavor = "current_thread")]
async fn deafen_and_undeafen_commands_create_discord_deafen_jobs() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(raw.path()).await;
    let room = test_room();
    let mut runtime = test_runtime(store.clone(), room.clone());
    store
        .upsert_capture_session_status(&VoiceCaptureSessionStatus {
            session_id: "cap_1".to_string(),
            guild_id: room.guild_id.clone(),
            voice_channel_id: room.channel_id.clone(),
            bot_id: "clanky-vc1".to_string(),
            active: true,
            started_at: "2026-05-15T00:00:00.000Z".to_string(),
            mode: "local_buffering".to_string(),
            ..VoiceCaptureSessionStatus::default()
        })
        .await
        .unwrap();

    let deafen = command_job(&room, CommandKind::DeafenListening);
    let deafen_id = deafen.id.clone();
    let deafen = store.create_job(deafen).await.unwrap();
    runtime.dispatch_claimed_runtime_job(deafen).await.unwrap();

    let deafen_jobs = store
        .list_jobs_by_scope_kind(
            &room.guild_id,
            &room.channel_id,
            JobKind::DiscordVoiceDeafen,
        )
        .await
        .unwrap();
    assert_eq!(deafen_jobs.len(), 1);
    let deafen_payload = deafen_jobs[0].discord_voice_deafen_payload().unwrap();
    assert_eq!(deafen_payload.session_id, "cap_1");
    assert!(deafen_payload.deafened);
    assert_eq!(deafen_payload.source_job_id, deafen_id);

    let placement_jobs = store
        .list_jobs_by_scope_kind(
            &room.guild_id,
            &room.channel_id,
            JobKind::RoomAgentPlacement,
        )
        .await
        .unwrap();
    assert!(placement_jobs.is_empty());

    let undeafen = command_job(&room, CommandKind::ResumeListening);
    let undeafen = store.create_job(undeafen).await.unwrap();
    runtime
        .dispatch_claimed_runtime_job(undeafen)
        .await
        .unwrap();

    let deafen_jobs = store
        .list_jobs_by_scope_kind(
            &room.guild_id,
            &room.channel_id,
            JobKind::DiscordVoiceDeafen,
        )
        .await
        .unwrap();
    assert_eq!(deafen_jobs.len(), 2);
    let undeafen_payload = deafen_jobs
        .iter()
        .find_map(|job| {
            let payload = job.discord_voice_deafen_payload()?;
            (!payload.deafened).then_some(payload)
        })
        .expect("undeafen job");
    assert_eq!(undeafen_payload.session_id, "cap_1");
}

fn test_runtime(timeline_store: TimelineStore, _room: RoomConfig) -> Runtime {
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

fn command_job(room: &RoomConfig, command_kind: CommandKind) -> Job {
    Job::command_request(
        RuntimeScope::voice_channel(&room.guild_id, &room.channel_id),
        "user-a",
        CommandRequest::from_json(&json!({
            "action": "dispatch_now",
            "command_kind": command_kind.as_str(),
            "guild_id": room.guild_id,
            "scope_id": room.channel_id,
            "requested_by_user_id": "user-a",
            "arguments": {
                "channel": "",
                "target_channel": "",
            },
        }))
        .unwrap(),
    )
}
