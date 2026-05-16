use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::sync::Mutex;

use chrono::{Duration, SecondsFormat, TimeZone, Utc};
use serde_json::json;

use clankcord::runtime::timeline::JobVisibility;
use clankcord::runtime::{
    AgentSessionStartPayload, AudioSegmentPayload, BinaryPayload, CommandRequest,
    DiscordForumThreadCreatePayload, DiscordTextMessagePayload, DiscordTextSendPayload,
    DiscordVoiceJoinPayload, DiscordVoiceLeaveOutput, DiscordVoiceMuteOutput,
    DiscordVoiceMutePayload, DiscordVoicePlayAudioOutput, DiscordVoicePlayAudioPayload,
    DiscordVoicePlaybackCue, DiscordVoicePlaybackOutput, DiscordVoicePlaybackPayload, Job, JobKind,
    JobOutput, JobPayload, JobState, RefineTranscriptPayload, RoomConfig, Runtime,
    TextDeliveryKind, TextDeliveryPayload, TextTarget, TextTargetKind,
    TranscriptPublicationPayload, WakeActivationPayload, WakeProbePayload,
};

mod common;
use common::test_store;

#[tokio::test(flavor = "current_thread")]
async fn job_round_trips_as_binary_record() {
    let command = CommandRequest::from_json(&json!({
        "command_kind": "agent_task",
        "guild_id": "guild",
        "voice_channel_id": "channel",
        "requested_by_user_id": "requester",
        "arguments": {"question": "what happened?", "relative_start": "-20m"}
    }))
    .unwrap();
    let job = Job::agent_task_for_session("ags_test", "guild", "channel", "requester", command);

    let encoded = job.encode().unwrap();
    let parsed = Job::decode(&encoded).unwrap();

    assert_eq!(parsed.kind, JobKind::AgentTask);
    assert_eq!(parsed.state, JobState::Queued);
    assert_eq!(parsed.command_kind(), "agent_task");
    assert_eq!(
        parsed.command().unwrap().arguments.question,
        "what happened?"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn audio_segment_payload_references_ready_audio_artifact() {
    let start = chrono::Utc.with_ymd_and_hms(2026, 5, 13, 12, 0, 0).unwrap();
    let source_audio_path = std::path::PathBuf::from("/tmp/clankcord/segment.wav");
    let job = Job::audio_segment(AudioSegmentPayload {
        guild_id: "guild".to_string(),
        guild_slug: "guild".to_string(),
        voice_channel_id: "channel".to_string(),
        voice_channel_name: "Channel".to_string(),
        voice_channel_slug: "channel".to_string(),
        capture_run_id: "cap".to_string(),
        voice_bot_id: "bot".to_string(),
        voice_bot_discord_user_id: "bot-user".to_string(),
        speaker_user_id: "speaker".to_string(),
        speaker_label: "Speaker".to_string(),
        speaker_username: "speaker_name".to_string(),
        segment_start_time: start,
        segment_end_time: start + chrono::Duration::milliseconds(20),
        segment_index: 7,
        duration_ms: 20,
        source_audio_path: source_audio_path.clone(),
        audio_checksum: "sha256:test".to_string(),
        audio_bytes: 44,
        audio_format: "wav".to_string(),
        sample_rate_hz: 48_000,
        channels: 2,
        sample_width_bits: 16,
        post_processing: "pcm_s16le_to_wav".to_string(),
    });

    assert_eq!(job.kind, JobKind::AudioSegment);
    assert_eq!(
        job.audio_segment_payload().unwrap().source_audio_path,
        source_audio_path
    );
    let payload = job.payload_value();
    assert_eq!(
        payload["source_audio_path"],
        json!("/tmp/clankcord/segment.wav")
    );
    assert_eq!(payload["audio_bytes"], json!(44));
    assert!(payload.get("pcm").is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn wake_probe_payload_references_ready_audio_artifact() {
    let start = chrono::Utc.with_ymd_and_hms(2026, 5, 13, 12, 0, 0).unwrap();
    let source_audio_path = std::path::PathBuf::from("/tmp/clankcord/wake-probe.wav");
    let job = Job::wake_probe(WakeProbePayload {
        guild_id: "guild".to_string(),
        guild_slug: "guild".to_string(),
        voice_channel_id: "channel".to_string(),
        voice_channel_name: "Channel".to_string(),
        voice_channel_slug: "channel".to_string(),
        capture_run_id: "cap".to_string(),
        voice_bot_id: "bot".to_string(),
        voice_bot_discord_user_id: "bot-user".to_string(),
        speaker_user_id: "speaker".to_string(),
        speaker_label: "Speaker".to_string(),
        speaker_username: "speaker_name".to_string(),
        probe_start_time: start,
        probe_end_time: start + chrono::Duration::milliseconds(500),
        probe_index: 2,
        duration_ms: 500,
        source_audio_path: source_audio_path.clone(),
        audio_checksum: "sha256:test".to_string(),
        audio_bytes: 44,
        audio_format: "wav".to_string(),
        sample_rate_hz: 48_000,
        channels: 2,
        sample_width_bits: 16,
        post_processing: "pcm_s16le_to_wav".to_string(),
        stream_id: "guild:channel:speaker".to_string(),
        reset_stream: false,
    });

    assert_eq!(job.kind, JobKind::WakeProbe);
    assert_eq!(
        job.wake_probe_payload().unwrap().source_audio_path,
        source_audio_path
    );
    let payload = job.payload_value();
    assert_eq!(
        payload["source_audio_path"],
        json!("/tmp/clankcord/wake-probe.wav")
    );
    assert_eq!(payload["stream_id"], json!("guild:channel:speaker"));
    assert_eq!(payload["reset_stream"], json!(false));
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_maintenance_job_is_ephemeral_and_round_trips() {
    let job = Job::runtime_maintenance(500);
    let decoded = Job::decode(&job.encode().unwrap()).unwrap();

    assert_eq!(decoded.kind, JobKind::RuntimeMaintenance);
    assert!(decoded.kind.is_ephemeral());
    assert_eq!(
        decoded.runtime_maintenance_payload().unwrap().interval_ms,
        500
    );
    assert_eq!(decoded.payload_value()["interval_ms"], json!(500));
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_maintenance_submits_background_work_jobs() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(&raw.path().join("voice")).await;
    let mut runtime = Runtime::from_store(store.clone()).unwrap();
    let created = store
        .create_job(Job::runtime_maintenance(500))
        .await
        .unwrap();
    let mut running = created.clone();
    running.mark_running();
    store.update_job(&running).await.unwrap();

    runtime.dispatch_claimed_runtime_job(running).await.unwrap();

    let completed = store.get_job(&created.id).await.unwrap();
    assert_eq!(completed.state, JobState::Complete);
    let output = completed.metadata.output.unwrap().to_json();
    assert_eq!(output["kind"], json!("runtime_maintenance"));
    assert_eq!(
        output["submitted_jobs"]
            .as_array()
            .map(|values| values.len())
            .unwrap(),
        5
    );

    let jobs = store
        .list_jobs_with_visibility(None, None, JobVisibility::IncludeEphemeral)
        .await
        .unwrap();
    let kinds = jobs.iter().map(|job| job.kind).collect::<BTreeSet<_>>();
    assert!(kinds.contains(&JobKind::RuntimeMaintenance));
    assert!(kinds.contains(&JobKind::VoiceStatusSync));
    assert!(kinds.contains(&JobKind::AutomationEvaluation));
    assert!(kinds.contains(&JobKind::StaleWakeProbeSweep));
    assert!(kinds.contains(&JobKind::StaleRunningJobSweep));
    assert!(kinds.contains(&JobKind::EphemeralJobGc));
}

fn initialize_test_config(root: &Path) {
    static CONFIG_LOCK: Mutex<()> = Mutex::new(());
    let _guard = CONFIG_LOCK.lock().unwrap();
    let path = root.join("config");
    fs::create_dir_all(&path).unwrap();
    fs::write(
        path.join("config.toml"),
        include_str!("../../config.ex.toml"),
    )
    .unwrap();
    let original_dir = std::env::current_dir().unwrap();
    std::env::set_current_dir(&path).unwrap();
    let _ = clankcord::config::app_config();
    std::env::set_current_dir(original_dir).unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn maintenance_work_jobs_are_typed_ephemeral_jobs() {
    let jobs = [
        Job::voice_status_sync("job_source"),
        Job::discord_voice_status_snapshot("job_source"),
        Job::automation_evaluation("job_source"),
        Job::stale_wake_probe_sweep("job_source", 15),
        Job::stale_running_job_sweep("job_source", 30),
        Job::ephemeral_job_gc("job_source", 500),
    ];

    for job in jobs {
        let decoded = Job::decode(&job.encode().unwrap()).unwrap();
        assert_eq!(decoded.kind, job.kind);
        assert!(decoded.kind.is_ephemeral());
        assert_eq!(
            decoded.payload_value()["source_job_id"],
            json!("job_source")
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn opaque_json_lowers_to_binary_payload() {
    let payload = BinaryPayload::from_json(&json!({"nested": ["value", 1]})).unwrap();
    assert!(!payload.as_bytes().is_empty());
    assert_eq!(payload.to_json(), json!({"nested": ["value", 1]}));
}

#[tokio::test(flavor = "current_thread")]
async fn job_lineage_allows_arbitrary_dag_depth_metadata() {
    let root = Job::new(
        "guild",
        "channel",
        "requester",
        JobState::Queued,
        JobPayload::RefineTranscript(RefineTranscriptPayload {
            window_id: "root".to_string(),
            publication_id: "pub".to_string(),
        }),
    );
    let mut child = Job::refine_transcript("guild", "channel", "requester", "child", "pub");
    child.attach_to_parent(&root).unwrap();
    let mut grandchild =
        Job::refine_transcript("guild", "channel", "requester", "grandchild", "pub");
    grandchild.attach_to_parent(&child).unwrap();
    let mut too_deep = Job::refine_transcript("guild", "channel", "requester", "deep", "pub");
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
async fn text_delivery_payload_is_a_first_class_binary_job() {
    let payload = TextDeliveryPayload::from_json(&json!({
        "intent": "question",
        "target": "agent_chat",
        "source_job_id": "job_source",
        "requested_by_user_id": "user-a",
        "content": "Do you mean the last 20 minutes?",
        "extra_boundary_field": {"kept": true}
    }))
    .unwrap();
    let job = Job::text_delivery("guild", "code", "user-a", payload);
    let decoded = Job::decode(&job.encode().unwrap()).unwrap();

    assert_eq!(decoded.kind, JobKind::TextDelivery);
    let delivery = decoded.text_delivery_payload().unwrap();
    assert_eq!(delivery.intent, TextDeliveryKind::Question);
    assert_eq!(delivery.target.kind, TextTargetKind::AgentChat);
    assert_eq!(delivery.source_job_id, "job_source");
    assert_eq!(
        delivery.to_json()["extra_boundary_field"]["kept"],
        json!(true)
    );
}

#[test]
fn discord_text_io_jobs_round_trip() {
    let text = Job::discord_text_send(
        "guild",
        "code",
        "user-a",
        DiscordTextSendPayload {
            intent: TextDeliveryKind::Message,
            target: TextTarget {
                kind: TextTargetKind::Channel,
                channel_id: "thread-1".to_string(),
                user_id: String::new(),
            },
            content: "Approve this?".to_string(),
            source_job_id: "job_source".to_string(),
            requested_by_user_id: String::new(),
            allowed_mentions: BinaryPayload::from_json(&json!({"parse": []})).unwrap(),
            components: BinaryPayload::from_json(&json!([{"type": 1}])).unwrap(),
        },
    );
    let decoded = Job::decode(&text.encode().unwrap()).unwrap();
    assert_eq!(decoded.kind, JobKind::DiscordTextSend);
    assert_eq!(decoded.payload.to_json()["components"][0]["type"], 1);

    let thread = Job::discord_forum_thread_create(
        "guild",
        "code",
        "user-a",
        DiscordForumThreadCreatePayload {
            parent_channel_id: "forum-1".to_string(),
            name: "agent code ags_1".to_string(),
            content: "# Agent Session".to_string(),
            auto_archive_minutes: 1440,
            source_job_id: "job_source".to_string(),
        },
    );
    let decoded = Job::decode(&thread.encode().unwrap()).unwrap();
    assert_eq!(decoded.kind, JobKind::DiscordForumThreadCreate);
    assert_eq!(decoded.payload.to_json()["parent_channel_id"], "forum-1");
}

#[test]
fn agent_session_start_and_publication_jobs_round_trip() {
    let command = CommandRequest::agent_task(
        "guild".to_string(),
        "code".to_string(),
        "user-a".to_string(),
        "follow up".to_string(),
    );
    let session = Job::agent_session_start(
        "guild",
        "code",
        "user-a",
        AgentSessionStartPayload {
            agent_session_id: "ags_1".to_string(),
            guild_id: "guild".to_string(),
            voice_channel_id: "code".to_string(),
            discord_parent_channel_id: "agent-threads".to_string(),
            requested_by_user_id: "user-a".to_string(),
            command,
        },
    );
    let decoded = Job::decode(&session.encode().unwrap()).unwrap();
    assert_eq!(decoded.kind, JobKind::AgentSessionStart);
    assert_eq!(decoded.payload.to_json()["agent_session_id"], "ags_1");

    let publication = Job::transcript_publication(
        "guild",
        "code",
        "user-a",
        TranscriptPublicationPayload {
            publication_id: "pub_1".to_string(),
            live: false,
            refined_queued: true,
        },
    );
    let decoded = Job::decode(&publication.encode().unwrap()).unwrap();
    assert_eq!(decoded.kind, JobKind::TranscriptPublication);
    assert_eq!(decoded.payload.to_json()["refined_queued"], true);
}

#[tokio::test(flavor = "current_thread")]
async fn wake_activation_payload_is_a_first_class_binary_job() {
    let payload = WakeActivationPayload {
        activation_id: "act_1".to_string(),
        guild_id: "guild".to_string(),
        voice_channel_id: "code".to_string(),
        voice_channel_name: "Code Lounge".to_string(),
        speaker_user_id: "user-a".to_string(),
        speaker_label: "Will".to_string(),
        wake_event_id: "evt_wake".to_string(),
        wake_started_at: "2026-05-14T12:00:00.000Z".to_string(),
        wake_ended_at: "2026-05-14T12:00:01.000Z".to_string(),
        latest_wake_event_id: "evt_wake".to_string(),
        latest_wake_at: "2026-05-14T12:00:00.000Z".to_string(),
        lookback_seconds: 30,
        min_post_seconds: 5,
        speaker_idle_seconds: 5,
        stt_flush_grace_seconds: 2,
        max_window_seconds: 60,
        additive_preempt_seconds: 10,
        independent_after_seconds: 45,
        amended_wake_event_ids: Vec::new(),
        replacement_of_job_ids: Vec::new(),
    };
    let job = Job::wake_activation(payload);
    let decoded = Job::decode(&job.encode().unwrap()).unwrap();

    assert_eq!(decoded.kind, JobKind::WakeActivation);
    assert_eq!(
        decoded.wake_activation_payload().unwrap().wake_event_id,
        "evt_wake"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn timeline_claim_due_jobs_marks_running_without_claiming_future_jobs() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(&raw.path().join("voice")).await;
    let due = Job::text_delivery("guild", "code", "user-a", text_delivery_payload("due"));
    let due_id = due.id.clone();
    let mut future = Job::text_delivery("guild", "code", "user-a", text_delivery_payload("future"));
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
async fn timeline_allows_multiple_text_deliveries_for_one_agent_source() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(&raw.path().join("voice")).await;
    let command = CommandRequest::from_json(&json!({
        "command_kind": "agent_task",
        "guild_id": "guild",
        "voice_channel_id": "code",
        "requested_by_user_id": "user-a",
        "arguments": {"question": "fact check this"}
    }))
    .unwrap();
    let mut source = Job::agent_task_for_session("ags_test", "guild", "code", "user-a", command);
    source.id = "job_agent_source".to_string();
    source.root_job_id = source.id.clone();
    store.create_job(source).await.unwrap();

    let first = Job::text_delivery(
        "guild",
        "code",
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
        "guild",
        "code",
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
async fn timeline_reports_earliest_queued_ready_time() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(&raw.path().join("voice")).await;
    let early = Utc::now() + Duration::seconds(30);
    let late = early + Duration::seconds(30);
    let mut early_job =
        Job::text_delivery("guild", "code", "user-a", text_delivery_payload("early"));
    let mut late_job = Job::text_delivery("guild", "code", "user-a", text_delivery_payload("late"));
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
        "voice_channel_id": "code",
        "requested_by_user_id": "user-a",
        "arguments": {"question": "summarize this"}
    }))
    .unwrap();
    let job = Job::agent_task_for_session("ags_test", "guild", "code", "user-a", command);
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
async fn timeline_claim_due_agent_ingress_serializes_by_voice_route_across_job_kinds() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(&raw.path().join("voice")).await;
    let command_job = Job::command_request(
        "guild",
        "code",
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
        "voice_channel_id": "code",
        "requested_by_user_id": "user-a",
        "arguments": {"question": "summarize this"}
    }))
    .unwrap();
    let mut first =
        Job::agent_task_for_session("ags_test", "guild", "code", "user-a", command.clone());
    first.created_at = Utc
        .with_ymd_and_hms(2026, 5, 12, 16, 0, 0)
        .unwrap()
        .to_rfc3339_opts(SecondsFormat::Millis, true);
    first.updated_at = first.created_at.clone();
    let first_id = first.id.clone();
    let mut second = Job::agent_task_for_session("ags_test", "guild", "code", "user-a", command);
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

    store.create_wake_probe_job(first).await.unwrap();
    store.create_wake_probe_job(second).await.unwrap();
    store.create_wake_probe_job(third).await.unwrap();
    store.create_wake_probe_job(fourth).await.unwrap();

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
            "guild",
            "code",
            "user-a",
            text_delivery_payload("parent"),
        ))
        .await
        .unwrap();
    let child = Job::text_delivery("guild", "code", "user-a", text_delivery_payload("child"));
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

#[tokio::test(flavor = "current_thread")]
async fn discord_voice_jobs_are_first_class_binary_jobs() {
    let room = RoomConfig {
        room_id: "code-lounge".to_string(),
        guild_id: "guild".to_string(),
        guild_slug: "guild".to_string(),
        channel_id: "code".to_string(),
        channel_slug: "code-lounge".to_string(),
        channel_name: "Code Lounge".to_string(),
        auto_join: true,
    };
    let payload = DiscordVoiceJoinPayload {
        room: room.clone(),
        bot_id: "clanky-vc1".to_string(),
        capture_run_id: "cap_1".to_string(),
        assignment_id: "assign_1".to_string(),
        started_at: Utc::now(),
        session_dir: raw_path("session"),
        requested_by_user_id: "user-a".to_string(),
        reason: "auto_join".to_string(),
    };
    let job = Job::discord_voice_join(payload);
    let decoded = Job::decode(&job.encode().unwrap()).unwrap();

    assert_eq!(decoded.kind, JobKind::DiscordVoiceJoin);
    assert_eq!(
        decoded.discord_voice_join_payload().unwrap().room.room_id,
        room.room_id
    );

    let output = JobOutput::DiscordVoiceLeave(DiscordVoiceLeaveOutput {
        session_id: "cap_1".to_string(),
        status: "ended".to_string(),
        session: None,
        bot_status: None,
        guild_id: "guild".to_string(),
        voice_channel_id: "code".to_string(),
        capture_run_id: "cap_1".to_string(),
        audio_jobs: Vec::new(),
    });
    let mut completed = decoded.clone();
    completed.metadata.output = Some(output);
    let completed = Job::decode(&completed.encode().unwrap()).unwrap();

    assert!(matches!(
        completed.metadata.output,
        Some(JobOutput::DiscordVoiceLeave(_))
    ));

    let playback = Job::discord_voice_playback(
        "guild",
        "code",
        "user-a",
        DiscordVoicePlaybackPayload {
            session_id: "cap_1".to_string(),
            cue: DiscordVoicePlaybackCue::Deafen,
            source_job_id: "job_parent".to_string(),
            reason: "deafen_listening".to_string(),
        },
    );
    let decoded = Job::decode(&playback.encode().unwrap()).unwrap();
    assert_eq!(decoded.kind, JobKind::DiscordVoicePlayback);
    let payload = decoded.discord_voice_playback_payload().unwrap();
    assert_eq!(payload.cue, DiscordVoicePlaybackCue::Deafen);
    assert_eq!(payload.cue.asset_file_name(), "clanky-deafen.wav");

    let mut completed = decoded;
    completed.metadata.output = Some(JobOutput::DiscordVoicePlayback(
        DiscordVoicePlaybackOutput {
            session_id: "cap_1".to_string(),
            cue: DiscordVoicePlaybackCue::Undeafen,
            status: "played".to_string(),
            guild_id: "guild".to_string(),
            voice_channel_id: "code".to_string(),
            audio_path: "/workspace/clankcord/res/audio/clanky-deafen.wav".to_string(),
            duration_ms: 250,
            message: String::new(),
        },
    ));
    let completed = Job::decode(&completed.encode().unwrap()).unwrap();
    assert!(matches!(
        completed.metadata.output,
        Some(JobOutput::DiscordVoicePlayback(_))
    ));

    let mute = Job::discord_voice_mute(
        "guild",
        "code",
        "user-a",
        DiscordVoiceMutePayload {
            session_id: "cap_1".to_string(),
            muted: false,
            source_job_id: "job_parent".to_string(),
            reason: "before_playback".to_string(),
        },
    );
    let decoded = Job::decode(&mute.encode().unwrap()).unwrap();
    assert_eq!(decoded.kind, JobKind::DiscordVoiceMute);
    assert!(!decoded.discord_voice_mute_payload().unwrap().muted);

    let mut completed = decoded;
    completed.metadata.output = Some(JobOutput::DiscordVoiceMute(DiscordVoiceMuteOutput {
        session_id: "cap_1".to_string(),
        muted: false,
        status: "set".to_string(),
        guild_id: "guild".to_string(),
        voice_channel_id: "code".to_string(),
        message: String::new(),
    }));
    let completed = Job::decode(&completed.encode().unwrap()).unwrap();
    assert!(matches!(
        completed.metadata.output,
        Some(JobOutput::DiscordVoiceMute(_))
    ));

    let play_audio = Job::discord_voice_play_audio(
        "guild",
        "code",
        "user-a",
        DiscordVoicePlayAudioPayload {
            session_id: "cap_1".to_string(),
            cue: DiscordVoicePlaybackCue::Wake,
            source_job_id: "job_parent".to_string(),
            reason: "wake_detected".to_string(),
        },
    );
    let decoded = Job::decode(&play_audio.encode().unwrap()).unwrap();
    assert_eq!(decoded.kind, JobKind::DiscordVoicePlayAudio);
    assert_eq!(
        decoded.discord_voice_play_audio_payload().unwrap().cue,
        DiscordVoicePlaybackCue::Wake
    );

    let mut completed = decoded;
    completed.metadata.output = Some(JobOutput::DiscordVoicePlayAudio(
        DiscordVoicePlayAudioOutput {
            session_id: "cap_1".to_string(),
            cue: DiscordVoicePlaybackCue::Wake,
            status: "played".to_string(),
            guild_id: "guild".to_string(),
            voice_channel_id: "code".to_string(),
            audio_path: "/workspace/clankcord/res/audio/clanky-wake.wav".to_string(),
            duration_ms: 250,
            message: String::new(),
        },
    ));
    let completed = Job::decode(&completed.encode().unwrap()).unwrap();
    assert!(matches!(
        completed.metadata.output,
        Some(JobOutput::DiscordVoicePlayAudio(_))
    ));
}

fn text_delivery_payload(content: &str) -> TextDeliveryPayload {
    TextDeliveryPayload::from_json(&json!({
        "intent": "message",
        "target": "agent_chat",
        "requested_by_user_id": "user-a",
        "content": content,
    }))
    .unwrap()
}

fn raw_path(path: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(path)
}

fn wake_probe_payload(stream_id: &str, probe_index: i64) -> WakeProbePayload {
    let start = Utc.with_ymd_and_hms(2026, 5, 13, 12, 0, 0).unwrap()
        + chrono::Duration::milliseconds(probe_index * 500);
    WakeProbePayload {
        guild_id: "guild".to_string(),
        guild_slug: "guild".to_string(),
        voice_channel_id: "code".to_string(),
        voice_channel_name: "Code".to_string(),
        voice_channel_slug: "code".to_string(),
        capture_run_id: "cap".to_string(),
        voice_bot_id: "bot".to_string(),
        voice_bot_discord_user_id: "bot-user".to_string(),
        speaker_user_id: "user-a".to_string(),
        speaker_label: "Will".to_string(),
        speaker_username: "will".to_string(),
        probe_start_time: start,
        probe_end_time: start + chrono::Duration::milliseconds(500),
        probe_index,
        duration_ms: 500,
        source_audio_path: raw_path("/tmp/clankcord/wake-probe.wav"),
        audio_checksum: "sha256:test".to_string(),
        audio_bytes: 44,
        audio_format: "wav".to_string(),
        sample_rate_hz: 48_000,
        channels: 2,
        sample_width_bits: 16,
        post_processing: "pcm_s16le_to_wav".to_string(),
        stream_id: stream_id.to_string(),
        reset_stream: true,
    }
}

fn wake_activation_payload(guild_id: &str, voice_channel_id: &str) -> WakeActivationPayload {
    WakeActivationPayload {
        activation_id: "act_route".to_string(),
        guild_id: guild_id.to_string(),
        voice_channel_id: voice_channel_id.to_string(),
        voice_channel_name: "Code".to_string(),
        speaker_user_id: "user-a".to_string(),
        speaker_label: "Will".to_string(),
        wake_event_id: "evt_wake".to_string(),
        wake_started_at: "2026-05-14T12:00:00.000Z".to_string(),
        wake_ended_at: "2026-05-14T12:00:01.000Z".to_string(),
        latest_wake_event_id: "evt_wake".to_string(),
        latest_wake_at: "2026-05-14T12:00:00.000Z".to_string(),
        lookback_seconds: 30,
        min_post_seconds: 5,
        speaker_idle_seconds: 5,
        stt_flush_grace_seconds: 2,
        max_window_seconds: 60,
        additive_preempt_seconds: 10,
        independent_after_seconds: 45,
        amended_wake_event_ids: Vec::new(),
        replacement_of_job_ids: Vec::new(),
    }
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
