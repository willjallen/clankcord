use std::collections::{BTreeMap, BTreeSet};

use serde_json::json;

use clankcord::runtime::{
    AgentRuntime, BinaryPayload, ControlConfig, DiscordSlashCommandPayload, Job, JobKind, Runtime,
};

mod common;
use common::{dt, initialize_test_config, test_store};

#[test]
fn discord_slash_command_job_round_trips() {
    let job = Job::discord_slash_command(DiscordSlashCommandPayload {
        interaction_id: "interaction-1".to_string(),
        interaction_token: "token-1".to_string(),
        application_id: "app-1".to_string(),
        guild_id: "guild".to_string(),
        channel_id: "code".to_string(),
        user_id: "user-a".to_string(),
        username: "will".to_string(),
        command_name: "join".to_string(),
        options: BinaryPayload::from_json(&json!([{"name": "room", "value": "code"}])).unwrap(),
        created_at: "2026-05-15T10:00:00.000Z".to_string(),
        response_visibility: "ephemeral".to_string(),
    });

    let decoded = Job::decode(&job.encode().unwrap()).unwrap();
    assert_eq!(decoded.kind, JobKind::DiscordSlashCommand);
    assert_eq!(decoded.requested_by_user_id, "user-a");
    assert_eq!(decoded.payload.to_json()["command_name"], "join");
    assert_eq!(decoded.payload.to_json()["options"][0]["value"], "code");
}

#[tokio::test(flavor = "current_thread")]
async fn feedback_slash_records_durable_timeline_event() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(raw.path()).await;
    let mut runtime = test_runtime(store.clone());
    let job = store
        .create_job(Job::discord_slash_command(DiscordSlashCommandPayload {
            interaction_id: "interaction-feedback".to_string(),
            interaction_token: "token-feedback".to_string(),
            application_id: "app-1".to_string(),
            guild_id: "guild".to_string(),
            channel_id: "code".to_string(),
            user_id: "user-a".to_string(),
            username: "will".to_string(),
            command_name: "feedback".to_string(),
            options: BinaryPayload::from_json(
                &json!([{"name": "message", "value": "The join command stalled."}]),
            )
            .unwrap(),
            created_at: "2026-05-15T10:00:00.000Z".to_string(),
            response_visibility: "ephemeral".to_string(),
        }))
        .await
        .unwrap();

    let job_id = job.id.clone();
    runtime.dispatch_claimed_runtime_job(job).await.unwrap();

    let mut kinds = BTreeSet::new();
    kinds.insert("feedback".to_string());
    let events = store
        .load_events("guild", "code", None, None, Some(&kinds), None, false)
        .await
        .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["kind"], json!("feedback"));
    assert_eq!(events[0]["job_id"], json!(job_id));
    assert_eq!(events[0]["interaction_id"], json!("interaction-feedback"));
    assert_eq!(events[0]["speaker_user_id"], json!("user-a"));
    assert_eq!(events[0]["speaker_label"], json!("will"));
    assert_eq!(events[0]["text"], json!("The join command stalled."));
    assert_eq!(
        events[0]["feedback_message"],
        json!("The join command stalled.")
    );
    assert_eq!(events[0]["timestamp"], json!("2026-05-15T10:00:00.000Z"));
}

fn test_runtime(timeline_store: clankcord::runtime::timeline::TimelineStore) -> Runtime {
    Runtime {
        started_at: dt(2026, 5, 12, 15, 0, 0),
        guilds: BTreeMap::new(),
        rooms: BTreeMap::new(),
        control_config: ControlConfig::default(),
        sessions: BTreeMap::new(),
        bots: BTreeMap::new(),
        assignments: BTreeMap::new(),
        agents: AgentRuntime::default(),
        automations: BTreeMap::new(),
        timeline_store,
        auto_join_enabled: true,
        manual_leave_cooldown_seconds: 20 * 60,
        manual_join_hold_seconds: 60 * 60,
        pause_release_seconds: 20 * 60,
    }
}
