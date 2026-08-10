use serde_json::json;

use crate::support::rooms::{room_runtime, test_room};
use crate::support::{initialize_test_config, test_store};

#[tokio::test(flavor = "current_thread")]
async fn pause_and_resume_room_controls_are_timeline_store_state() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(raw.path()).await;
    let room = test_room();
    let runtime = room_runtime(store.clone(), room.clone());

    clankcord::domain::rooms::control_state::pause_room(&runtime, &room, 60, "user-a")
        .await
        .unwrap();

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

    let fresh_runtime = room_runtime(store.clone(), room.clone());
    let status =
        clankcord::domain::rooms::control_state::room_control_status(&fresh_runtime, &room)
            .await
            .unwrap();
    assert_eq!(status["listeningPaused"], json!(true));
    assert!(
        clankcord::domain::rooms::control_state::room_controls_json(&fresh_runtime)
            .await
            .unwrap()
            .contains_key(&room.channel_id)
    );

    clankcord::domain::rooms::control_state::resume_room(&runtime, &room, "user-a")
        .await
        .unwrap();

    assert!(
        store
            .get_room_control(&room.guild_id, &room.channel_id)
            .await
            .unwrap()
            .is_none()
    );
    let fresh_runtime = room_runtime(store, room.clone());
    let status =
        clankcord::domain::rooms::control_state::room_control_status(&fresh_runtime, &room)
            .await
            .unwrap();
    assert_eq!(status["listeningPaused"], json!(false));
    assert_eq!(status["control"], json!({}));
}
