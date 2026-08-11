//! Durable claim contracts: due ordering, route serialization, wake-window priority,
//! backlog cancellation, lineage and dependency edges.

use std::collections::BTreeSet;

use chrono::{Duration, SecondsFormat, TimeZone, Utc};
use serde_json::json;

use clankcord::domain::Ctx;
use clankcord::domain::transcription::execution;
use clankcord::model::job::{
    CommandRequest, DiscordTextMessagePayload, DiscordTypingAction, DiscordTypingIndicatorPayload,
    Job, JobKind, JobState, TextDeliveryKind, TextDeliveryPayload, TextTarget, TextTargetKind,
};
use clankcord::model::scope::RuntimeScope;
use clankcord::time::isoformat_z;

use crate::support::job_wire::encode_current_agent_task;
use crate::support::jobs::{
    audio_segment_payload, text_delivery_payload, wake_activation_payload, wake_probe_payload,
};
use crate::support::test_store;

#[tokio::test(flavor = "current_thread")]
async fn job_lineage_allows_arbitrary_dag_depth_metadata() {
    let root = Job::audio_segment(audio_segment_payload(
        "guild",
        "channel",
        "speaker",
        Utc::now() - Duration::seconds(4),
        Utc::now() - Duration::seconds(3),
        1,
    ));
    let mut child = Job::transcription_mux("local-granite");
    child.attach_to_parent(&root).unwrap();
    let mut grandchild = Job::transcription_mux("local-granite");
    grandchild.attach_to_parent(&child).unwrap();
    let mut too_deep = Job::transcription_mux("local-granite");
    too_deep.attach_to_parent(&grandchild).unwrap();

    assert_eq!(child.parent_job_id.as_deref(), Some(root.id.as_str()));
    assert_eq!(child.root_job_id, root.id);
    assert_eq!(child.lineage_depth, 1);
    assert_eq!(grandchild.parent_job_id.as_deref(), Some(child.id.as_str()));
    assert_eq!(grandchild.root_job_id, child.root_job_id);
    assert_eq!(grandchild.lineage_depth, 2);
    assert_eq!(
        too_deep.parent_job_id.as_deref(),
        Some(grandchild.id.as_str())
    );
    assert_eq!(too_deep.root_job_id, child.root_job_id);
    assert_eq!(too_deep.lineage_depth, 3);
}
#[tokio::test(flavor = "current_thread")]
async fn timeline_claim_due_jobs_marks_running_without_claiming_future_jobs() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(&raw.path().join("voice")).await;
    let due = Job::text_delivery(
        RuntimeScope::voice_channel("guild", "code"),
        "user-a",
        text_delivery_payload("due"),
    );
    let due_id = due.id.clone();
    let mut future = Job::text_delivery(
        RuntimeScope::voice_channel("guild", "code"),
        "user-a",
        text_delivery_payload("future"),
    );
    let future_id = future.id.clone();
    future.next_run_at =
        Some((Utc::now() + Duration::minutes(5)).to_rfc3339_opts(SecondsFormat::Millis, true));

    store.create_job(future).await.unwrap();
    store.create_job(due).await.unwrap();

    let mut blocked = BTreeSet::new();
    let claimed = store
        .claim_due_jobs(JobKind::TextDelivery, 8, &mut blocked)
        .await
        .unwrap();

    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].id, due_id);
    assert_eq!(claimed[0].state, JobState::Running);
    assert_eq!(
        store.get_job(&due_id).await.unwrap().state,
        JobState::Running
    );
    assert_eq!(
        store.get_job(&future_id).await.unwrap().state,
        JobState::Queued
    );
    assert!(
        store
            .claim_due_jobs(JobKind::TextDelivery, 8, &mut BTreeSet::new())
            .await
            .unwrap()
            .is_empty()
    );
}
#[tokio::test(flavor = "current_thread")]
async fn timeline_claim_due_audio_prioritizes_active_wake_window_segments() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(&raw.path().join("voice")).await;
    let now = Utc::now();
    let mut wake_payload = wake_activation_payload("guild", "code");
    wake_payload.wake_started_at = isoformat_z(Some(now - Duration::seconds(20)));
    wake_payload.latest_wake_at = isoformat_z(Some(now - Duration::seconds(20)));
    wake_payload.max_window_seconds = 3600;
    store
        .create_job(Job::wake_activation(wake_payload))
        .await
        .unwrap();

    let mut normal = Job::audio_segment(audio_segment_payload(
        "guild",
        "art",
        "user-b",
        now - Duration::seconds(70),
        now - Duration::seconds(69),
        1,
    ));
    normal.created_at = isoformat_z(Some(now - Duration::seconds(40)));
    normal.updated_at = normal.created_at.clone();
    let normal_id = normal.id.clone();
    store.create_job(normal).await.unwrap();

    let mut priority = Job::audio_segment(audio_segment_payload(
        "guild",
        "code",
        "user-b",
        now - Duration::seconds(10),
        now - Duration::seconds(9),
        2,
    ));
    priority.created_at = isoformat_z(Some(now - Duration::seconds(10)));
    priority.updated_at = priority.created_at.clone();
    let priority_id = priority.id.clone();
    store.create_job(priority).await.unwrap();

    let claimed = store
        .claim_due_jobs(JobKind::AudioSegment, 1, &mut BTreeSet::new())
        .await
        .unwrap();

    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].id, priority_id);
    assert_eq!(
        store.get_job(&normal_id).await.unwrap().state,
        JobState::Queued
    );
}
#[tokio::test(flavor = "current_thread")]
async fn timeline_claim_due_audio_does_not_prioritize_segments_after_closed_wake_window() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(&raw.path().join("voice")).await;
    let now = Utc::now();
    let mut wake_payload = wake_activation_payload("guild", "code");
    wake_payload.wake_started_at = isoformat_z(Some(now - Duration::seconds(30)));
    wake_payload.latest_wake_at = isoformat_z(Some(now - Duration::seconds(30)));
    let activation_id = wake_payload.activation_id.clone();
    let closed_at = now - Duration::seconds(20);
    store
        .create_job(Job::wake_activation(wake_payload))
        .await
        .unwrap();
    store
        .append_event(
            "guild",
            "code",
            json!({
                "event_kind": "wake_activation_window_closed",
                "kind": "wake_activation_window_closed",
                "activation_id": activation_id,
                "request_audio_closed_at": isoformat_z(Some(closed_at)),
                "startedAt": closed_at.to_rfc3339_opts(SecondsFormat::Millis, true),
                "endedAt": closed_at.to_rfc3339_opts(SecondsFormat::Millis, true),
            }),
        )
        .await
        .unwrap();

    let mut normal = Job::audio_segment(audio_segment_payload(
        "guild",
        "code",
        "user-b",
        now - Duration::seconds(40),
        now - Duration::seconds(39),
        1,
    ));
    normal.created_at = isoformat_z(Some(now - Duration::seconds(40)));
    normal.updated_at = normal.created_at.clone();
    let normal_id = normal.id.clone();
    store.create_job(normal).await.unwrap();

    let mut after_closed = Job::audio_segment(audio_segment_payload(
        "guild",
        "code",
        "user-a",
        now - Duration::seconds(10),
        now - Duration::seconds(9),
        2,
    ));
    after_closed.created_at = isoformat_z(Some(now - Duration::seconds(10)));
    after_closed.updated_at = after_closed.created_at.clone();
    let after_closed_id = after_closed.id.clone();
    store.create_job(after_closed).await.unwrap();

    let claimed = store
        .claim_due_jobs(JobKind::AudioSegment, 1, &mut BTreeSet::new())
        .await
        .unwrap();

    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].id, normal_id);
    assert_eq!(
        store.get_job(&after_closed_id).await.unwrap().state,
        JobState::Queued
    );
}
#[tokio::test(flavor = "current_thread")]
async fn timeline_maintenance_requeues_retryable_failed_audio_segments() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(&raw.path().join("voice")).await;
    let now = Utc::now();
    let mut retryable_transport = Job::audio_segment(audio_segment_payload(
        "guild",
        "code",
        "user-a",
        now - Duration::seconds(10),
        now - Duration::seconds(9),
        1,
    ));
    retryable_transport.set_state(JobState::Failed);
    retryable_transport.started_at = Some(isoformat_z(Some(now - Duration::seconds(8))));
    retryable_transport.metadata.error =
        "retryable STT connection error: error sending request for url (http://127.0.0.1:8080/v1/audio/transcriptions)"
            .to_string();
    let retryable_transport_id = retryable_transport.id.clone();
    store.create_job(retryable_transport).await.unwrap();

    let mut retryable_rate_limit = Job::audio_segment(audio_segment_payload(
        "guild",
        "code",
        "user-a",
        now - Duration::seconds(9),
        now - Duration::seconds(8),
        2,
    ));
    retryable_rate_limit.set_state(JobState::Failed);
    retryable_rate_limit.metadata.error =
        "HTTP status client error (429 Too Many Requests) for url (http://127.0.0.1:8080/v1/audio/transcriptions)"
            .to_string();
    let retryable_rate_limit_id = retryable_rate_limit.id.clone();
    store.create_job(retryable_rate_limit).await.unwrap();

    let mut retryable_server = Job::audio_segment(audio_segment_payload(
        "guild",
        "code",
        "user-a",
        now - Duration::seconds(8),
        now - Duration::seconds(7),
        3,
    ));
    retryable_server.set_state(JobState::Failed);
    retryable_server.metadata.error =
        "HTTP status server error (503 Service Unavailable) for url (http://127.0.0.1:8080/v1/audio/transcriptions)"
            .to_string();
    let retryable_server_id = retryable_server.id.clone();
    store.create_job(retryable_server).await.unwrap();

    let mut retryable_body = Job::audio_segment(audio_segment_payload(
        "guild",
        "code",
        "user-a",
        now - Duration::seconds(7),
        now - Duration::seconds(6),
        4,
    ));
    retryable_body.set_state(JobState::Failed);
    retryable_body.metadata.error =
        "request or response body error for url (http://127.0.0.1:8080/v1/audio/transcriptions)"
            .to_string();
    let retryable_body_id = retryable_body.id.clone();
    store.create_job(retryable_body).await.unwrap();

    let mut non_retryable_client = Job::audio_segment(audio_segment_payload(
        "guild",
        "code",
        "user-b",
        now - Duration::seconds(6),
        now - Duration::seconds(5),
        5,
    ));
    non_retryable_client.set_state(JobState::Failed);
    non_retryable_client.metadata.error =
        "HTTP status client error (401 Unauthorized) for url (http://127.0.0.1:8080/v1/audio/transcriptions)"
            .to_string();
    let non_retryable_client_id = non_retryable_client.id.clone();
    store.create_job(non_retryable_client).await.unwrap();

    let mut permanent = Job::audio_segment(audio_segment_payload(
        "guild",
        "code",
        "user-b",
        now - Duration::seconds(5),
        now - Duration::seconds(4),
        6,
    ));
    permanent.set_state(JobState::Failed);
    permanent.metadata.error = "audio segment artifact is missing: /tmp/missing.wav".to_string();
    let permanent_id = permanent.id.clone();
    store.create_job(permanent).await.unwrap();

    let requeued = execution::requeue_failed_audio_segment_jobs(&Ctx::new(store.clone()), 10)
        .await
        .unwrap();
    let requeued_ids = requeued
        .iter()
        .map(|job| job["job_id"].as_str().unwrap().to_string())
        .collect::<BTreeSet<_>>();

    assert_eq!(requeued.len(), 4);
    assert!(requeued_ids.contains(&retryable_transport_id));
    assert!(requeued_ids.contains(&retryable_rate_limit_id));
    assert!(requeued_ids.contains(&retryable_server_id));
    assert!(requeued_ids.contains(&retryable_body_id));
    for job_id in [
        retryable_transport_id,
        retryable_rate_limit_id,
        retryable_server_id,
        retryable_body_id,
    ] {
        let retryable = store.get_job(&job_id).await.unwrap();
        assert_eq!(retryable.state, JobState::Queued);
        assert_eq!(retryable.attempts, 1);
        assert!(retryable.next_run_at.is_some());
        assert!(retryable.started_at.is_none());
    }
    assert_eq!(
        store.get_job(&non_retryable_client_id).await.unwrap().state,
        JobState::Failed
    );
    assert_eq!(
        store.get_job(&permanent_id).await.unwrap().state,
        JobState::Failed
    );
}
#[tokio::test(flavor = "current_thread")]
async fn timeline_allows_multiple_text_deliveries_for_one_agent_source() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(&raw.path().join("voice")).await;
    let command = CommandRequest::from_json(&json!({
        "command_kind": "agent_task",
        "guild_id": "guild",
        "scope_id": "code",
        "requested_by_user_id": "user-a",
        "arguments": {"question": "fact check this"}
    }))
    .unwrap();
    let mut source = Job::agent_task_for_session(
        "ags_test",
        RuntimeScope::voice_channel("guild", "code"),
        "user-a",
        command,
    );
    source.id = "job_agent_source".to_string();
    source.root_job_id = source.id.clone();
    store.create_job(source).await.unwrap();

    let first = Job::text_delivery(
        RuntimeScope::voice_channel("guild", "code"),
        "user-a",
        TextDeliveryPayload::new(
            TextDeliveryKind::Message,
            TextTarget::default(),
            "first chunk",
            "job_agent_source",
            "user-a",
            false,
        ),
    );
    let first_id = first.id.clone();
    let second = Job::text_delivery(
        RuntimeScope::voice_channel("guild", "code"),
        "user-a",
        TextDeliveryPayload::new(
            TextDeliveryKind::Message,
            TextTarget::default(),
            "second chunk",
            "job_agent_source",
            "user-a",
            false,
        ),
    );
    let second_id = second.id.clone();

    let created_first = store.create_job(first).await.unwrap();
    let created_second = store.create_job(second).await.unwrap();

    assert_eq!(created_first.id, first_id);
    assert_eq!(created_second.id, second_id);
    let deliveries = store
        .list_text_delivery_jobs_for_source("job_agent_source")
        .await
        .unwrap();
    let delivery_ids = deliveries
        .iter()
        .map(|job| job.id.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(deliveries.len(), 2);
    assert!(delivery_ids.contains(first_id.as_str()));
    assert!(delivery_ids.contains(second_id.as_str()));
}
#[tokio::test(flavor = "current_thread")]
async fn completed_agent_task_with_missing_response_delivery_completes_terminally() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(&raw.path().join("voice")).await;
    let command = CommandRequest::from_json(&json!({
        "command_kind": "agent_task",
        "guild_id": "guild",
        "scope_id": "code",
        "requested_by_user_id": "user-a",
        "arguments": {"request": "leave the room"}
    }))
    .unwrap();
    let created = store
        .create_job(Job::agent_task_for_session(
            "ags_test",
            RuntimeScope::voice_channel("guild", "code"),
            "user-a",
            command,
        ))
        .await
        .unwrap();
    let stop = Job::discord_typing_indicator(
        RuntimeScope::voice_channel("guild", "code"),
        "user-a",
        DiscordTypingIndicatorPayload {
            action: DiscordTypingAction::Stop,
            target: TextTarget {
                kind: TextTargetKind::AgentSession,
                channel_id: String::new(),
                user_id: String::new(),
            },
            source_job_id: created.id.clone(),
            requested_by_user_id: "user-a".to_string(),
            agent_task_attempt: 0,
        },
    );
    let stop = store.create_child_job(&created, stop).await.unwrap();
    let waiting_parent = store.get_job(&created.id).await.unwrap();
    let encoded = encode_current_agent_task(
        &waiting_parent,
        "RESPONSE_SUBMITTED",
        "codex exec --output-last-message",
        "",
    );
    sqlx::query("UPDATE job_payloads SET payload_blob = $1 WHERE job_id = $2")
        .bind(encoded)
        .bind(&created.id)
        .execute(&store.pool)
        .await
        .unwrap();
    let mut stop = store.get_job(&stop.id).await.unwrap();
    stop.mark_complete();
    store.update_job(&stop).await.unwrap();
    store.resolve_waiting_jobs().await.unwrap();

    let claimed = store
        .claim_due_jobs(JobKind::AgentTask, 1, &mut BTreeSet::new())
        .await
        .unwrap();
    assert_eq!(claimed.len(), 1);

    let runtime = Ctx::new(store.clone());
    let result =
        clankcord::engine::dispatcher::dispatch_claimed_blocking_job(&runtime, claimed[0].clone())
            .await
            .unwrap();

    assert_eq!(result["dispatched"], json!(true));
    assert_eq!(result["outcome"], json!("submitted_without_delivery"));
    let completed = store.get_job(&created.id).await.unwrap();
    assert_eq!(completed.state, JobState::Complete);
    assert!(completed.completed_at.is_some());
    assert!(completed.metadata.error.is_empty());
    assert_eq!(
        completed.metadata.to_json()["agent_task"]["result_suppressed"],
        json!(true)
    );
}
#[tokio::test(flavor = "current_thread")]
async fn timeline_reports_earliest_queued_ready_time() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(&raw.path().join("voice")).await;
    let early = Utc::now() + Duration::seconds(30);
    let late = early + Duration::seconds(30);
    let mut early_job = Job::text_delivery(
        RuntimeScope::voice_channel("guild", "code"),
        "user-a",
        text_delivery_payload("early"),
    );
    let mut late_job = Job::text_delivery(
        RuntimeScope::voice_channel("guild", "code"),
        "user-a",
        text_delivery_payload("late"),
    );
    early_job.next_run_at = Some(early.to_rfc3339_opts(SecondsFormat::Millis, true));
    late_job.next_run_at = Some(late.to_rfc3339_opts(SecondsFormat::Millis, true));

    store.create_job(late_job).await.unwrap();
    store.create_job(early_job).await.unwrap();

    let next = store.next_queued_job_ready_at().await.unwrap().unwrap();
    assert_eq!(next.timestamp_millis(), early.timestamp_millis());
    let next_after_early = store
        .next_queued_job_ready_after(early)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(next_after_early.timestamp_millis(), late.timestamp_millis());
}
#[tokio::test(flavor = "current_thread")]
async fn timeline_claim_due_jobs_can_skip_active_agent_sessions() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(&raw.path().join("voice")).await;
    let command = CommandRequest::from_json(&json!({
        "command_kind": "agent_task",
        "guild_id": "guild",
        "scope_id": "code",
        "requested_by_user_id": "user-a",
        "arguments": {"question": "summarize this"}
    }))
    .unwrap();
    let job = Job::agent_task_for_session(
        "ags_test",
        RuntimeScope::voice_channel("guild", "code"),
        "user-a",
        command,
    );
    let job_id = job.id.clone();
    store.create_job(job).await.unwrap();

    let mut blocked = BTreeSet::from(["agent:session:ags_test".to_string()]);
    let skipped = store
        .claim_due_jobs(JobKind::AgentTask, 4, &mut blocked)
        .await
        .unwrap();

    assert!(skipped.is_empty());
    assert_eq!(
        store.get_job(&job_id).await.unwrap().state,
        JobState::Queued
    );

    let claimed = store
        .claim_due_jobs(JobKind::AgentTask, 4, &mut BTreeSet::new())
        .await
        .unwrap();
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].id, job_id);
    assert_eq!(
        store.get_job(&job_id).await.unwrap().state,
        JobState::Running
    );
}
#[tokio::test(flavor = "current_thread")]
async fn waiting_agent_task_holds_session_ordering_key() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(&raw.path().join("voice")).await;
    let command = CommandRequest::agent_task("guild", "code", "user-a", "summarize this");
    let first = store
        .create_job(Job::agent_task_for_session(
            "ags_test",
            RuntimeScope::voice_channel("guild", "code"),
            "user-a",
            command.clone(),
        ))
        .await
        .unwrap();
    store
        .create_child_job(
            &first,
            Job::discord_typing_indicator(
                RuntimeScope::voice_channel("guild", "code"),
                "user-a",
                DiscordTypingIndicatorPayload {
                    action: DiscordTypingAction::Start,
                    target: TextTarget {
                        kind: TextTargetKind::AgentSession,
                        channel_id: String::new(),
                        user_id: String::new(),
                    },
                    source_job_id: first.id.clone(),
                    requested_by_user_id: "user-a".to_string(),
                    agent_task_attempt: 0,
                },
            ),
        )
        .await
        .unwrap();
    let second = store
        .create_job(Job::agent_task_for_session(
            "ags_test",
            RuntimeScope::voice_channel("guild", "code"),
            "user-a",
            command,
        ))
        .await
        .unwrap();

    let mut blocked = store.active_ordering_keys().await.unwrap();
    let claimed = store
        .claim_due_jobs(JobKind::AgentTask, 4, &mut blocked)
        .await
        .unwrap();

    assert!(claimed.is_empty());
    assert_eq!(
        store.get_job(&first.id).await.unwrap().state,
        JobState::Waiting
    );
    assert_eq!(
        store.get_job(&second.id).await.unwrap().state,
        JobState::Queued
    );
}
#[tokio::test(flavor = "current_thread")]
async fn timeline_claim_due_agent_ingress_serializes_by_voice_route_across_job_kinds() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(&raw.path().join("voice")).await;
    let command_job = Job::command_request(
        RuntimeScope::voice_channel("guild", "code"),
        "user-a",
        CommandRequest::agent_task("guild", "code", "user-a", "first request"),
    );
    let wake_job = Job::wake_activation(wake_activation_payload("guild", "code"));
    let wake_job_id = wake_job.id.clone();
    store.create_job(command_job).await.unwrap();
    store.create_job(wake_job).await.unwrap();

    let claimed_commands = store
        .claim_due_jobs(JobKind::Command, 4, &mut BTreeSet::new())
        .await
        .unwrap();
    assert_eq!(claimed_commands.len(), 1);

    let mut blocked = store.active_ordering_keys().await.unwrap();
    let claimed_wake = store
        .claim_due_jobs(JobKind::WakeActivation, 4, &mut blocked)
        .await
        .unwrap();
    assert!(claimed_wake.is_empty());
    assert_eq!(
        store.get_job(&wake_job_id).await.unwrap().state,
        JobState::Queued
    );
}
#[tokio::test(flavor = "current_thread")]
async fn timeline_claim_due_dm_text_messages_serializes_by_user_route() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(&raw.path().join("voice")).await;
    let first = Job::discord_text_message(discord_dm_text_message("dm-a", "msg-1", "user-a"));
    let second = Job::discord_text_message(discord_dm_text_message("dm-b", "msg-2", "user-a"));
    store.create_job(first).await.unwrap();
    store.create_job(second).await.unwrap();

    let claimed = store
        .claim_due_jobs(JobKind::DiscordTextMessage, 4, &mut BTreeSet::new())
        .await
        .unwrap();

    assert_eq!(claimed.len(), 1);
}
#[tokio::test(flavor = "current_thread")]
async fn timeline_claim_due_jobs_applies_skip_after_due_sorting() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(&raw.path().join("voice")).await;
    let command = CommandRequest::from_json(&json!({
        "command_kind": "agent_task",
        "guild_id": "guild",
        "scope_id": "code",
        "requested_by_user_id": "user-a",
        "arguments": {"question": "summarize this"}
    }))
    .unwrap();
    let mut first = Job::agent_task_for_session(
        "ags_test",
        RuntimeScope::voice_channel("guild", "code"),
        "user-a",
        command.clone(),
    );
    first.created_at = Utc
        .with_ymd_and_hms(2026, 5, 12, 16, 0, 0)
        .unwrap()
        .to_rfc3339_opts(SecondsFormat::Millis, true);
    first.updated_at = first.created_at.clone();
    let first_id = first.id.clone();
    let mut second = Job::agent_task_for_session(
        "ags_test",
        RuntimeScope::voice_channel("guild", "code"),
        "user-a",
        command,
    );
    second.created_at = Utc
        .with_ymd_and_hms(2026, 5, 12, 16, 0, 1)
        .unwrap()
        .to_rfc3339_opts(SecondsFormat::Millis, true);
    second.updated_at = second.created_at.clone();
    let second_id = second.id.clone();
    store.create_job(first).await.unwrap();
    store.create_job(second).await.unwrap();

    let claimed = store
        .claim_due_jobs(JobKind::AgentTask, 4, &mut BTreeSet::new())
        .await
        .unwrap();

    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].id, first_id);
    assert_eq!(
        store.get_job(&first_id).await.unwrap().state,
        JobState::Running
    );
    assert_eq!(
        store.get_job(&second_id).await.unwrap().state,
        JobState::Queued
    );
}
#[tokio::test(flavor = "current_thread")]
async fn timeline_preserves_ordered_wake_probe_backlog_per_stream() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(&raw.path().join("voice")).await;
    let first = Job::wake_probe(wake_probe_payload("guild:code:cap:user-a", 0));
    let first_id = first.id.clone();
    let second = Job::wake_probe(wake_probe_payload("guild:code:cap:user-a", 1));
    let second_id = second.id.clone();
    let third = Job::wake_probe(wake_probe_payload("guild:code:cap:user-a", 2));
    let third_id = third.id.clone();
    let fourth = Job::wake_probe(wake_probe_payload("guild:code:cap:user-a", 3));
    let fourth_id = fourth.id.clone();

    store.create_job(first).await.unwrap();
    store.create_job(second).await.unwrap();
    store.create_job(third).await.unwrap();
    store.create_job(fourth).await.unwrap();

    assert_eq!(
        store
            .get_job(&first_id)
            .await
            .unwrap()
            .wake_probe_payload()
            .unwrap()
            .probe_index,
        0
    );
    assert_eq!(
        store
            .get_job(&second_id)
            .await
            .unwrap()
            .wake_probe_payload()
            .unwrap()
            .probe_index,
        1
    );
    let stored_third = store.get_job(&third_id).await.unwrap();
    assert_eq!(stored_third.state, JobState::Queued);
    assert_eq!(stored_third.wake_probe_payload().unwrap().probe_index, 2);
    let stored_fourth = store.get_job(&fourth_id).await.unwrap();
    assert_eq!(stored_fourth.state, JobState::Queued);
    assert_eq!(stored_fourth.wake_probe_payload().unwrap().probe_index, 3);
}
#[tokio::test(flavor = "current_thread")]
async fn timeline_cancels_stale_wake_probe_backlog() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(&raw.path().join("voice")).await;
    let old_at = Utc
        .with_ymd_and_hms(2026, 5, 12, 16, 0, 0)
        .unwrap()
        .to_rfc3339_opts(SecondsFormat::Millis, true);
    let mut old = Job::wake_probe(wake_probe_payload("guild:code:cap:user-a", 0));
    old.created_at = old_at.clone();
    old.updated_at = old_at;
    let old_id = old.id.clone();
    store.create_job(old).await.unwrap();

    let cancelled = store.cancel_stale_wake_probe_jobs(1).await.unwrap();

    assert_eq!(cancelled.len(), 1);
    assert_eq!(
        store.get_job(&old_id).await.unwrap().state,
        JobState::Cancelled
    );
}
#[tokio::test(flavor = "current_thread")]
async fn timeline_child_jobs_are_stored_as_dependency_edges() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(&raw.path().join("voice")).await;
    let parent = store
        .create_job(Job::text_delivery(
            RuntimeScope::voice_channel("guild", "code"),
            "user-a",
            text_delivery_payload("parent"),
        ))
        .await
        .unwrap();
    let child = Job::text_delivery(
        RuntimeScope::voice_channel("guild", "code"),
        "user-a",
        text_delivery_payload("child"),
    );
    let child_id = child.id.clone();

    store.create_child_job(&parent, child).await.unwrap();

    let parent = store.get_job(&parent.id).await.unwrap();
    assert_eq!(parent.state, JobState::Waiting);
    let children = store.list_child_jobs(&parent.id).await.unwrap();
    assert_eq!(children.len(), 1);
    assert_eq!(children[0].id, child_id);
    assert_eq!(
        children[0].parent_job_id.as_deref(),
        Some(parent.id.as_str())
    );
}
fn discord_dm_text_message(
    channel_id: &str,
    message_id: &str,
    author_user_id: &str,
) -> DiscordTextMessagePayload {
    DiscordTextMessagePayload {
        guild_id: String::new(),
        channel_id: channel_id.to_string(),
        message_id: message_id.to_string(),
        author_user_id: author_user_id.to_string(),
        author_username: "will".to_string(),
        author_display_name: "Will".to_string(),
        content: "follow up".to_string(),
        created_at: "2026-05-14T12:00:00.000Z".to_string(),
        referenced_message_id: String::new(),
    }
}
