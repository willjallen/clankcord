use std::collections::BTreeSet;

use serde_json::json;

use clankcord::adapters::discord::gateway::slash::{
    slash_missing_voice_channel_response_content, slash_success_response_content,
};
use clankcord::runtime::{
    BinaryPayload, CommandKind, DebugOverviewRequest, DiscordSlashCommandPayload, Job, JobKind,
    Runtime, RuntimeScopeKind,
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
    assert_eq!(decoded.scope_kind, RuntimeScopeKind::VoiceChannel);
    assert_eq!(decoded.scope_id, "code");
    assert_eq!(decoded.payload.to_json()["command_name"], "join");
    assert_eq!(decoded.payload.to_json()["voice_channel_id"], "code");
    assert_eq!(decoded.payload.to_json()["options"][0]["value"], "code");
}

#[test]
fn slash_command_responses_are_human_readable() {
    let join = slash_success_response_content(&slash_payload(
        "interaction-join",
        "join",
        "slash-text",
        "code",
        json!([]),
    ));
    assert_eq!(join, "Connecting Clanky to <#code>.");

    let deafen = slash_success_response_content(&slash_payload(
        "interaction-deafen",
        "deafen",
        "slash-text",
        "code",
        json!([]),
    ));
    assert_eq!(deafen, "Deafening Clanky in <#code>.");

    let feedback = slash_success_response_content(&slash_payload(
        "interaction-feedback",
        "feedback",
        "slash-text",
        "",
        json!([{"name": "message", "value": "The join command stalled."}]),
    ));
    assert_eq!(feedback, "Feedback sent: The join command stalled.");

    let responses = [
        join,
        deafen,
        feedback,
        slash_missing_voice_channel_response_content().to_string(),
    ];
    for response in responses {
        assert!(!response.contains("job_"));
        assert!(!response.contains("queued"));
    }
    assert_eq!(
        slash_missing_voice_channel_response_content(),
        "You are not in a voice channel."
    );
}

#[tokio::test(flavor = "current_thread")]
async fn feedback_slash_records_durable_timeline_event() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(raw.path()).await;
    let mut runtime = test_runtime(store.clone());
    let job = store
        .create_job(Job::discord_slash_command(slash_payload(
            "interaction-feedback",
            "feedback",
            "slash-text",
            "code",
            json!([{"name": "message", "value": "The join command stalled."}]),
        )))
        .await
        .unwrap();

    let job_id = job.id.clone();
    runtime.dispatch_claimed_runtime_job(job).await.unwrap();

    let mut kinds = BTreeSet::new();
    kinds.insert("discord_slash_command".to_string());
    kinds.insert("feedback".to_string());
    let events = store
        .load_events("guild", "code", None, None, Some(&kinds), None, false)
        .await
        .unwrap();
    assert_eq!(events.len(), 2);
    let slash_event = events
        .iter()
        .find(|event| event["kind"] == json!("discord_slash_command"))
        .unwrap();
    let feedback_event = events
        .iter()
        .find(|event| event["kind"] == json!("feedback"))
        .unwrap();

    assert_eq!(slash_event["job_id"], json!(job_id));
    assert_eq!(slash_event["command_name"], json!("feedback"));
    assert_eq!(
        slash_event["options"],
        json!([{"name": "message", "value": "The join command stalled."}])
    );
    assert_eq!(feedback_event["job_id"], json!(job_id));
    assert_eq!(
        feedback_event["interaction_id"],
        json!("interaction-feedback")
    );
    assert_eq!(feedback_event["discord_channel_id"], json!("slash-text"));
    assert_eq!(feedback_event["voice_channel_id"], json!("code"));
    assert_eq!(feedback_event["speaker_user_id"], json!("user-a"));
    assert_eq!(feedback_event["speaker_label"], json!("will"));
    assert_eq!(feedback_event["text"], json!("The join command stalled."));
    assert_eq!(
        feedback_event["feedback_message"],
        json!("The join command stalled.")
    );
    assert_eq!(
        feedback_event["timestamp"],
        json!("2026-05-15T10:00:00.000Z")
    );

    let overview = Runtime::from_store(store)
        .unwrap()
        .debug_overview(DebugOverviewRequest {
            timeline_window: "all".to_string(),
            timeline_query: "/feedback".to_string(),
            timeline_query_field: "all".to_string(),
            ..DebugOverviewRequest::default()
        })
        .await
        .unwrap();
    let dashboard_events = overview["timeline"]["recentEvents"].as_array().unwrap();
    let dashboard_slash_event = dashboard_events
        .iter()
        .find(|event| event["kind"] == json!("discord_slash_command"))
        .unwrap();
    assert_eq!(dashboard_slash_event["command_name"], json!("feedback"));
    assert_eq!(
        dashboard_slash_event["options"],
        json!([{"name": "message", "value": "The join command stalled."}])
    );
    assert!(
        dashboard_events
            .iter()
            .any(|event| event["kind"] == json!("feedback"))
    );
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
        ("interaction-join", "join", CommandKind::JoinRoom),
        ("interaction-leave", "leave", CommandKind::LeaveRoom),
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
                json!([{"name": "room", "value": "other"}]),
            )))
            .await
            .unwrap();

        let job_id = job.id.clone();
        runtime.dispatch_claimed_runtime_job(job).await.unwrap();

        let children = store.list_child_jobs(&job_id).await.unwrap();
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].kind, JobKind::Command);
        assert_eq!(children[0].guild_id, "guild");
        assert_eq!(children[0].scope_kind, RuntimeScopeKind::VoiceChannel);
        assert_eq!(children[0].scope_id, "code");
        let command = children[0].command().unwrap();
        assert_eq!(command.command_kind, expected_kind);
        assert_eq!(command.guild_id, "guild");
        assert_eq!(command.scope_id, "code");
        assert_eq!(command.requested_by_user_id, "user-a");
        assert_eq!(command.requested_by_speaker_label, "will");
        assert_eq!(command.target_channel_id, "");
        assert_eq!(command.arguments.channel, "");
        assert_eq!(command.arguments.target_channel, "");
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
