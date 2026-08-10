use std::collections::BTreeSet;

use serde_json::json;

use clankcord::domain::Ctx;
use clankcord::model::job::{CommandKind, Job, JobKind};
use clankcord::model::scope::RuntimeScopeKind;
use clankcord::views::{DashboardFilter, DashboardTimelineRequest};

use crate::support::cli::slash_payload;
use crate::support::{initialize_test_config, test_store};

#[tokio::test(flavor = "current_thread")]
async fn feedback_slash_records_durable_timeline_event() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(raw.path()).await;
    let runtime = test_runtime(store.clone());
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
    clankcord::engine::dispatcher::dispatch_claimed_runtime_job(
        &runtime,
        &clankcord::ports::discord::DiscordApiUnavailable,
        job,
    )
    .await
    .unwrap();

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

    let page = clankcord::views::dashboard::dashboard_timeline(
        &Ctx::new(store),
        DashboardTimelineRequest {
            record_types: DashboardFilter::Values(BTreeSet::from(["event".to_string()])),
            job_kinds: DashboardFilter::None,
            from: "all".to_string(),
            search: "/feedback".to_string(),
            metadata: "none".to_string(),
            ..DashboardTimelineRequest::default()
        },
    )
    .await
    .unwrap();
    let dashboard_events = page["records"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|record| record.get("event"))
        .collect::<Vec<_>>();
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
    let runtime = test_runtime(store.clone());
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
    clankcord::engine::dispatcher::dispatch_claimed_runtime_job(
        &runtime,
        &clankcord::ports::discord::DiscordApiUnavailable,
        job,
    )
    .await
    .unwrap();

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
    let runtime = test_runtime(store.clone());

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
        clankcord::engine::dispatcher::dispatch_claimed_runtime_job(
            &runtime,
            &clankcord::ports::discord::DiscordApiUnavailable,
            job,
        )
        .await
        .unwrap();

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

fn test_runtime(timeline_store: clankcord::store::TimelineStore) -> Ctx {
    Ctx::new(timeline_store)
}
