use chrono::{SecondsFormat, TimeZone, Utc};

mod common;

use clankcord::runtime::{
    AgentSessionRecord, AgentSessionRecordState, DiscordTextMessagePayload, Job, JobKind,
    TextTargetKind, voice_route_key,
};

#[tokio::test(flavor = "current_thread")]
async fn agent_session_records_route_by_voice_and_thread() {
    let raw = tempfile::tempdir().unwrap();
    let store = common::test_store(&raw.path().join("voice")).await;
    let created_at = Utc.with_ymd_and_hms(2026, 5, 15, 10, 0, 0).unwrap();
    let expires_at = Utc.with_ymd_and_hms(2099, 5, 15, 14, 0, 0).unwrap();
    let record = AgentSessionRecord::new_voice(
        "ags_test",
        "guild",
        "code",
        "agent-threads",
        "thread-1",
        created_at.to_rfc3339_opts(SecondsFormat::Millis, true),
        expires_at.to_rfc3339_opts(SecondsFormat::Millis, true),
    );

    store
        .create_agent_session_record(record.clone())
        .await
        .unwrap();

    let by_route = store
        .active_agent_session_for_route(&voice_route_key("guild", "code"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(by_route.agent_session_id, "ags_test");
    assert_eq!(by_route.text_target.kind, TextTargetKind::Channel);
    assert_eq!(by_route.text_target.channel_id, "thread-1");

    let by_thread = store
        .agent_session_for_thread("thread-1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(by_thread.route_key, voice_route_key("guild", "code"));
}

#[tokio::test(flavor = "current_thread")]
async fn expired_agent_sessions_stop_matching_active_route() {
    let raw = tempfile::tempdir().unwrap();
    let store = common::test_store(&raw.path().join("voice")).await;
    let created_at = Utc.with_ymd_and_hms(2026, 5, 15, 10, 0, 0).unwrap();
    let expires_at = Utc.with_ymd_and_hms(2026, 5, 15, 10, 0, 1).unwrap();
    let mut record = AgentSessionRecord::new_voice(
        "ags_expired",
        "guild",
        "code",
        "agent-threads",
        "thread-1",
        created_at.to_rfc3339_opts(SecondsFormat::Millis, true),
        expires_at.to_rfc3339_opts(SecondsFormat::Millis, true),
    );
    record.state = AgentSessionRecordState::Expired;
    store.create_agent_session_record(record).await.unwrap();

    let by_route = store
        .active_agent_session_for_route(&voice_route_key("guild", "code"))
        .await
        .unwrap();
    assert!(by_route.is_none());

    let by_thread = store
        .agent_session_for_thread("thread-1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(by_thread.state, AgentSessionRecordState::Expired);
}

#[test]
fn discord_text_message_job_round_trips() {
    let job = Job::discord_text_message(DiscordTextMessagePayload {
        guild_id: "guild".to_string(),
        channel_id: "thread-1".to_string(),
        message_id: "message-1".to_string(),
        author_user_id: "user-a".to_string(),
        author_username: "will".to_string(),
        author_display_name: "Will".to_string(),
        content: "follow up".to_string(),
        created_at: "2026-05-15T10:00:00.000Z".to_string(),
        referenced_message_id: String::new(),
    });

    let decoded = Job::decode(&job.encode().unwrap()).unwrap();
    assert_eq!(decoded.kind, JobKind::DiscordTextMessage);
    assert_eq!(decoded.requested_by_user_id, "user-a");
    assert_eq!(decoded.payload.to_json()["content"], "follow up");
}
