use std::collections::BTreeSet;

use serde_json::json;

use clankcord::runtime::{
    BinaryPayload, CommandKind, DiscordSlashCommandPayload, Job, JobKind, Runtime,
};

mod common;
use common::{initialize_test_config, test_store};

#[test]
fn discord_slash_command_job_round_trips() {
    let job = Job::discord_slash_command(DiscordSlashCommandPayload {
        interaction_id: "interaction-1".to_string(),
        interaction_token: "token-1".to_string(),
        application_id: "app-1".to_string(),
        guild_id: "guild".to_string(),
        channel_id: "code".to_string(),
        voice_channel_id: "code".to_string(),
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
    assert_eq!(decoded.voice_channel_id, "code");
    assert_eq!(decoded.payload.to_json()["command_name"], "join");
    assert_eq!(decoded.payload.to_json()["voice_channel_id"], "code");
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
            voice_channel_id: "code".to_string(),
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

#[tokio::test(flavor = "current_thread")]
async fn wake_slash_schedules_manual_activation_for_invoker_voice_room() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(raw.path()).await;
    let mut runtime = test_runtime(store.clone());
    let job = store
        .create_job(Job::discord_slash_command(slash_payload(
            "interaction-wake",
            "wake",
            "slash-text",
            "code",
            json!([]),
        )))
        .await
        .unwrap();

    let job_id = job.id.clone();
    runtime.dispatch_claimed_runtime_job(job).await.unwrap();

    let mut kinds = BTreeSet::new();
    kinds.insert("wake_detected".to_string());
    let events = store
        .load_events("guild", "code", None, None, Some(&kinds), None, false)
        .await
        .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["kind"], json!("wake_detected"));
    assert_eq!(events[0]["manual"], json!(true));
    assert_eq!(events[0]["source"], json!("discord_slash_command"));
    assert_eq!(events[0]["job_id"], json!(job_id));
    assert_eq!(events[0]["discord_channel_id"], json!("slash-text"));
    assert_eq!(events[0]["speaker_user_id"], json!("user-a"));

    let activations = store
        .list_jobs_by_scope_kind("guild", "code", JobKind::WakeActivation)
        .await
        .unwrap();
    assert_eq!(activations.len(), 1);
    let activation = activations[0].wake_activation_payload().unwrap();
    assert_eq!(activation.guild_id, "guild");
    assert_eq!(activation.voice_channel_id, "code");
    assert_eq!(activation.speaker_user_id, "user-a");
    assert_eq!(activation.speaker_label, "will");
    assert_eq!(
        activation.wake_event_id,
        events[0]["event_id"].as_str().unwrap()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn voice_control_slash_commands_use_invoker_voice_room() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(raw.path()).await;
    let mut runtime = test_runtime(store.clone());

    for (interaction_id, slash_name, expected_kind) in [
        ("interaction-deafen", "deafen", CommandKind::DeafenListening),
        (
            "interaction-undeafen",
            "undeafen",
            CommandKind::ResumeListening,
        ),
    ] {
        let job = store
            .create_job(Job::discord_slash_command(slash_payload(
                interaction_id,
                slash_name,
                "slash-text",
                "code",
                json!([]),
            )))
            .await
            .unwrap();

        let job_id = job.id.clone();
        runtime.dispatch_claimed_runtime_job(job).await.unwrap();

        let children = store.list_child_jobs(&job_id).await.unwrap();
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].kind, JobKind::Command);
        assert_eq!(children[0].guild_id, "guild");
        assert_eq!(children[0].voice_channel_id, "code");
        let command = children[0].command().unwrap();
        assert_eq!(command.command_kind, expected_kind);
        assert_eq!(command.guild_id, "guild");
        assert_eq!(command.voice_channel_id, "code");
        assert_eq!(command.requested_by_user_id, "user-a");
        assert_eq!(command.requested_by_speaker_label, "will");
        assert_eq!(command.target_voice_channel_id, "");
    }
}

fn slash_payload(
    interaction_id: &str,
    command_name: &str,
    channel_id: &str,
    voice_channel_id: &str,
    options: serde_json::Value,
) -> DiscordSlashCommandPayload {
    DiscordSlashCommandPayload {
        interaction_id: interaction_id.to_string(),
        interaction_token: format!("token-{interaction_id}"),
        application_id: "app-1".to_string(),
        guild_id: "guild".to_string(),
        channel_id: channel_id.to_string(),
        voice_channel_id: voice_channel_id.to_string(),
        user_id: "user-a".to_string(),
        username: "will".to_string(),
        command_name: command_name.to_string(),
        options: BinaryPayload::from_json(&options).unwrap(),
        created_at: "2026-05-15T10:00:00.000Z".to_string(),
        response_visibility: "ephemeral".to_string(),
    }
}

fn test_runtime(timeline_store: clankcord::runtime::timeline::TimelineStore) -> Runtime {
    Runtime::from_store(timeline_store).unwrap()
}
