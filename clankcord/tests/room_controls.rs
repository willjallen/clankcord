use std::fs;
use std::path::PathBuf;

use serde_json::json;

use clankcord::runtime::timeline::TimelineStore;
use clankcord::runtime::{RoomConfig, Runtime};

mod common;
use common::test_store;

#[tokio::test(flavor = "current_thread")]
async fn pause_and_resume_room_controls_are_timeline_store_state() {
    let raw = tempfile::tempdir().unwrap();
    let _config_dir = TestConfigDir::enter(raw.path().join("config"));
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

struct TestConfigDir {
    original_dir: PathBuf,
}

impl TestConfigDir {
    fn enter(path: PathBuf) -> Self {
        fs::create_dir_all(&path).unwrap();
        fs::write(
            path.join("config.toml"),
            include_str!("../../config.ex.toml"),
        )
        .unwrap();
        let original_dir = std::env::current_dir().unwrap();
        std::env::set_current_dir(path).unwrap();
        Self { original_dir }
    }
}

impl Drop for TestConfigDir {
    fn drop(&mut self) {
        std::env::set_current_dir(&self.original_dir).unwrap();
    }
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
