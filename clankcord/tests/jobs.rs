use std::collections::BTreeSet;

use chrono::{Duration, SecondsFormat, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;

use clankcord::runtime::jobs::DiscordPostMetadata;
use clankcord::runtime::jobs::JobMetadata;
use clankcord::runtime::timeline::JobVisibility;
use clankcord::runtime::timeline::views::JobsRequest;
use clankcord::runtime::{
    AgentSessionStartPayload, AudioSegmentPayload, BinaryPayload, CommandRequest,
    DiscordForumThreadCreatePayload, DiscordForumThreadRenamePayload, DiscordTextMessagePayload,
    DiscordTextSendPayload, DiscordTypingAction, DiscordTypingIndicatorOutput,
    DiscordTypingIndicatorPayload, DiscordVoiceDeafenOutput, DiscordVoiceDeafenPayload,
    DiscordVoiceJoinPayload, DiscordVoiceLeaveOutput, DiscordVoiceMuteOutput,
    DiscordVoiceMutePayload, DiscordVoicePlayAudioOutput, DiscordVoicePlayAudioPayload,
    DiscordVoicePlaybackCue, DiscordVoicePlaybackOutput, DiscordVoicePlaybackPayload, Job, JobKind,
    JobOutput, JobPayload, JobState, RefineTranscriptPayload, RoomConfig, Runtime, RuntimeScope,
    RuntimeScopeKind, TextDeliveryKind, TextDeliveryPayload, TextTarget, TextTargetKind,
    TranscriptPublicationPayload, WakeActivationPayload, WakeProbePayload,
};

mod common;
use common::{initialize_test_config, test_store};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PreV0_3_0Job {
    id: String,
    kind: JobKind,
    guild_id: String,
    voice_channel_id: String,
    state: JobState,
    requested_by_user_id: String,
    payload: JobPayload,
    attempts: i64,
    created_at: String,
    updated_at: String,
    next_run_at: Option<String>,
    started_at: Option<String>,
    completed_at: Option<String>,
    cancelled_at: Option<String>,
    parent_job_id: Option<String>,
    root_job_id: String,
    lineage_depth: u8,
    metadata: JobMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
struct PreV0_6_0AgentInvocationMetadata {
    session_id: String,
    provider: String,
    model: String,
    usage: BinaryPayload,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
struct PreV0_6_0AgentPreflightCheck {
    command: String,
    returncode: Option<i32>,
    ok: bool,
    stdout_preview: String,
    stderr_preview: String,
    error: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
struct PreV0_6_0AgentPreflightMetadata {
    ok: bool,
    checked_at: String,
    checks: Vec<PreV0_6_0AgentPreflightCheck>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
struct PreV0_6_0AgentTaskMetadata {
    dispatch_attempts: i64,
    dispatch_error: String,
    dispatch_error_after_cancel: String,
    workdir_path: String,
    prompt_path: String,
    result_path: String,
    raw_result_path: String,
    dispatch_stdout_preview: String,
    dispatch_stderr: String,
    agent: PreV0_6_0AgentInvocationMetadata,
    preflight: Option<PreV0_6_0AgentPreflightMetadata>,
    response_text: String,
    command: String,
    result_suppressed: bool,
    discord_post: Option<DiscordPostMetadata>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
struct PreV0_6_0ConfirmationJobMetadata {
    delivery: String,
    channel_id: String,
    message_id: String,
    post_error: String,
    approved_by_user_id: String,
    approved_at: String,
    approval_error: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
enum PreV0_6_0JobMetadataDetail {
    AgentTask(PreV0_6_0AgentTaskMetadata),
    Confirmation(PreV0_6_0ConfirmationJobMetadata),
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
struct PreV0_6_0JobMetadata {
    detail: Option<Box<PreV0_6_0JobMetadataDetail>>,
    error: String,
    timed_out_at: String,
    cancel_requested: bool,
    cancelled_by_user_id: String,
    output: Option<JobOutput>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PreV0_6_0Job {
    id: String,
    kind: JobKind,
    scope_kind: RuntimeScopeKind,
    guild_id: String,
    scope_id: String,
    state: JobState,
    requested_by_user_id: String,
    payload: JobPayload,
    attempts: i64,
    created_at: String,
    updated_at: String,
    next_run_at: Option<String>,
    started_at: Option<String>,
    completed_at: Option<String>,
    cancelled_at: Option<String>,
    parent_job_id: Option<String>,
    root_job_id: String,
    lineage_depth: u8,
    metadata: PreV0_6_0JobMetadata,
}

#[tokio::test(flavor = "current_thread")]
async fn job_round_trips_as_binary_record() {
    let command = CommandRequest::from_json(&json!({
        "command_kind": "agent_task",
        "guild_id": "guild",
        "scope_id": "channel",
        "requested_by_user_id": "requester",
        "arguments": {"question": "what happened?", "relative_start": "-20m"}
    }))
    .unwrap();
    let job = Job::agent_task_for_session(
        "ags_test",
        RuntimeScope::voice_channel("guild", "channel"),
        "requester",
        command,
    );

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

#[test]
fn job_payload_blob_uses_current_version_envelope() {
    let job = Job::runtime_maintenance(500);
    let encoded = job.encode().unwrap();

    assert_eq!(&encoded[..8], b"CLANKJOB");
    assert_eq!(u16::from_le_bytes([encoded[8], encoded[9]]), 4);
    assert!(Job::is_current_payload_blob(&encoded));
}

#[test]
fn job_decode_rejects_pre_v0_2_0_raw_bincode_payload() {
    let job = Job::runtime_maintenance(500);
    let pre_v0_2_0 = bincode::serialize(&job).unwrap();

    let error = Job::decode(&pre_v0_2_0).unwrap_err().to_string();

    assert!(error.contains("job payload blob is not a current encoded job payload"));
    assert!(!Job::is_current_payload_blob(&pre_v0_2_0));
}

#[test]
fn job_state_rejects_agent_specific_dispatch_failure_state() {
    assert!("agent_dispatch_failed".parse::<JobState>().is_err());
    assert_eq!("failed".parse::<JobState>().unwrap(), JobState::Failed);
}

#[tokio::test(flavor = "current_thread")]
async fn timeline_initialize_records_registered_schema_migrations() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(&raw.path().join("voice")).await;

    let rows = sqlx::query(
        r#"
        SELECT version, name, clankcord_version
        FROM clankcord_schema_migrations
        ORDER BY version
        "#,
    )
    .fetch_all(&store.pool)
    .await
    .unwrap();
    let migrations = rows
        .iter()
        .map(|row| {
            (
                sqlx::Row::try_get::<String, _>(row, "version").unwrap(),
                sqlx::Row::try_get::<String, _>(row, "name").unwrap(),
                sqlx::Row::try_get::<String, _>(row, "clankcord_version").unwrap(),
            )
        })
        .collect::<Vec<_>>();

    assert_eq!(
        migrations,
        vec![
            (
                "0.2.0".to_string(),
                "job payload blob envelope".to_string(),
                "0.6.0".to_string()
            ),
            (
                "0.3.0".to_string(),
                "generic runtime scope projections".to_string(),
                "0.6.0".to_string()
            ),
            (
                "0.4.0".to_string(),
                "database hard-cut performance contracts".to_string(),
                "0.6.0".to_string()
            ),
            (
                "0.5.0".to_string(),
                "policy-driven durable retention".to_string(),
                "0.6.0".to_string()
            ),
            (
                "0.6.0".to_string(),
                "job payload blob v4 agent invocation metadata".to_string(),
                "0.6.0".to_string()
            ),
        ]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn v0_3_0_schema_migration_rewrites_legacy_job_scope_projection_and_blob() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(&raw.path().join("voice")).await;
    let created = store
        .create_job(Job::agent_task_for_session(
            "ags_test",
            RuntimeScope::voice_channel("guild", "code"),
            "user-a",
            CommandRequest::agent_task("guild", "code", "user-a", "summarize"),
        ))
        .await
        .unwrap();
    let legacy_blob = encode_pre_v0_3_0_job(&created);

    sqlx::raw_sql(
        r#"
        ALTER TABLE jobs ADD COLUMN voice_channel_id TEXT NOT NULL DEFAULT '';
        UPDATE jobs SET voice_channel_id = scope_id;
        ALTER TABLE jobs DROP COLUMN scope_kind CASCADE;
        ALTER TABLE jobs DROP COLUMN scope_id CASCADE;
        "#,
    )
    .execute(&store.pool)
    .await
    .unwrap();
    sqlx::query("UPDATE job_payloads SET payload_blob = $1 WHERE job_id = $2")
        .bind(legacy_blob)
        .bind(&created.id)
        .execute(&store.pool)
        .await
        .unwrap();
    sqlx::query(
        "DELETE FROM clankcord_schema_migrations WHERE version IN ('0.3.0', '0.4.0', '0.5.0', '0.6.0')",
    )
    .execute(&store.pool)
    .await
    .unwrap();

    let applied = store.run_pending_schema_migrations().await.unwrap();

    assert_eq!(applied.len(), 4);
    assert_eq!(applied[0].version, "0.3.0");
    assert_eq!(applied[1].version, "0.4.0");
    assert_eq!(applied[2].version, "0.5.0");
    assert_eq!(applied[3].version, "0.6.0");
    assert!(!column_exists(&store.pool, "jobs", "voice_channel_id").await);
    let row = sqlx::query("SELECT scope_kind, scope_id FROM jobs WHERE job_id = $1")
        .bind(&created.id)
        .fetch_one(&store.pool)
        .await
        .unwrap();
    assert_eq!(
        sqlx::Row::try_get::<String, _>(&row, "scope_kind").unwrap(),
        "voice_channel"
    );
    assert_eq!(
        sqlx::Row::try_get::<String, _>(&row, "scope_id").unwrap(),
        "code"
    );
    let migrated = store.get_job(&created.id).await.unwrap();
    assert_eq!(migrated.scope_kind, RuntimeScopeKind::VoiceChannel);
    assert_eq!(migrated.scope_id, "code");
    let row = sqlx::query("SELECT payload_blob FROM job_payloads WHERE job_id = $1")
        .bind(&created.id)
        .fetch_one(&store.pool)
        .await
        .unwrap();
    let payload_blob: Vec<u8> = sqlx::Row::try_get(&row, "payload_blob").unwrap();
    assert!(Job::is_current_payload_blob(&payload_blob));
}

#[tokio::test(flavor = "current_thread")]
async fn v0_4_0_schema_migration_enforces_timeline_event_time_contract() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(&raw.path().join("voice")).await;

    sqlx::raw_sql(
        r#"
        ALTER TABLE timeline_events
          ALTER COLUMN started_at_ms DROP NOT NULL,
          ALTER COLUMN ended_at_ms DROP NOT NULL
        "#,
    )
    .execute(&store.pool)
    .await
    .unwrap();
    assert!(column_nullable(&store.pool, "timeline_events", "started_at_ms").await);
    assert!(column_nullable(&store.pool, "timeline_events", "ended_at_ms").await);

    sqlx::query(
        "DELETE FROM clankcord_schema_migrations WHERE version IN ('0.4.0', '0.5.0', '0.6.0')",
    )
    .execute(&store.pool)
    .await
    .unwrap();

    let applied = store.run_pending_schema_migrations().await.unwrap();

    assert_eq!(applied.len(), 3);
    assert_eq!(applied[0].version, "0.4.0");
    assert_eq!(applied[1].version, "0.5.0");
    assert_eq!(applied[2].version, "0.6.0");
    assert!(!column_nullable(&store.pool, "timeline_events", "started_at_ms").await);
    assert!(!column_nullable(&store.pool, "timeline_events", "ended_at_ms").await);
}

#[tokio::test(flavor = "current_thread")]
async fn v0_5_0_schema_migration_drops_terminal_retention_index() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(&raw.path().join("voice")).await;

    sqlx::raw_sql(
        r#"
        CREATE INDEX idx_jobs_terminal_retention
          ON jobs(created_at_ms, job_id)
          WHERE terminal = TRUE;
        "#,
    )
    .execute(&store.pool)
    .await
    .unwrap();
    assert!(index_exists(&store.pool, "idx_jobs_terminal_retention").await);

    sqlx::query("DELETE FROM clankcord_schema_migrations WHERE version IN ('0.5.0', '0.6.0')")
        .execute(&store.pool)
        .await
        .unwrap();

    let applied = store.run_pending_schema_migrations().await.unwrap();

    assert_eq!(applied.len(), 2);
    assert_eq!(applied[0].version, "0.5.0");
    assert_eq!(applied[1].version, "0.6.0");
    assert!(!index_exists(&store.pool, "idx_jobs_terminal_retention").await);
}

#[tokio::test(flavor = "current_thread")]
async fn v0_6_0_schema_migration_rewrites_v3_agent_task_job_blob() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(&raw.path().join("voice")).await;
    let created = store
        .create_job(Job::agent_task_for_session(
            "ags_test",
            RuntimeScope::voice_channel("guild", "code"),
            "user-a",
            CommandRequest::agent_task("guild", "code", "user-a", "summarize"),
        ))
        .await
        .unwrap();
    let legacy_blob = encode_pre_v0_6_0_agent_task_job(&created);

    sqlx::query("UPDATE job_payloads SET payload_blob = $1 WHERE job_id = $2")
        .bind(legacy_blob)
        .bind(&created.id)
        .execute(&store.pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM clankcord_schema_migrations WHERE version = '0.6.0'")
        .execute(&store.pool)
        .await
        .unwrap();

    let applied = store.run_pending_schema_migrations().await.unwrap();

    assert_eq!(applied.len(), 1);
    assert_eq!(applied[0].version, "0.6.0");
    let migrated = store.get_job(&created.id).await.unwrap();
    let metadata = migrated.metadata.to_json();
    let agent = &metadata["agent_task"]["agent"];
    assert_eq!(agent["session_id"], json!("codex-session-v3"));
    assert_eq!(agent["provider"], json!("codex"));
    assert_eq!(agent["model"], json!("codex-default"));
    assert!(agent.get("reasoning_effort").is_none());
    assert!(agent.get("fast_mode").is_none());
    let row = sqlx::query("SELECT payload_blob FROM job_payloads WHERE job_id = $1")
        .bind(&created.id)
        .fetch_one(&store.pool)
        .await
        .unwrap();
    let payload_blob: Vec<u8> = sqlx::Row::try_get(&row, "payload_blob").unwrap();
    assert!(Job::is_current_payload_blob(&payload_blob));
}

#[tokio::test(flavor = "current_thread")]
async fn jobs_public_view_uses_generic_scope_fields() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(&raw.path().join("voice")).await;
    let created = store
        .create_job(Job::agent_task_for_session(
            "ags_test",
            RuntimeScope::voice_channel("guild", "code"),
            "user-a",
            CommandRequest::agent_task("guild", "code", "user-a", "summarize"),
        ))
        .await
        .unwrap();
    let runtime = Runtime::from_store(store).unwrap();

    let jobs = runtime
        .jobs(JobsRequest {
            guild_id: "guild".to_string(),
            ..JobsRequest::default()
        })
        .await
        .unwrap();
    let job = jobs["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|job| job["job_id"] == created.id)
        .expect("created job appears in public jobs view");

    assert_eq!(job["scope_kind"], "voice_channel");
    assert_eq!(job["scope_id"], "code");
    assert!(job.get("voice_channel_id").is_none());

    let verbose = runtime.get_job_payload(&created.id, true).await.unwrap();
    assert_eq!(verbose["scope_kind"], "voice_channel");
    assert_eq!(verbose["scope_id"], "code");
    assert!(verbose.get("voice_channel_id").is_none());
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
async fn runtime_maintenance_replacement_deletes_active_singleton_by_projection() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(&raw.path().join("voice")).await;
    let existing = store
        .create_job(Job::runtime_maintenance(500))
        .await
        .unwrap();
    sqlx::query("UPDATE job_payloads SET payload_blob = $1 WHERE job_id = $2")
        .bind(vec![0_u8, 1, 2])
        .bind(&existing.id)
        .execute(&store.pool)
        .await
        .unwrap();

    let replacement = store
        .replace_runtime_maintenance_job(Job::runtime_maintenance(1000))
        .await
        .unwrap();

    assert_ne!(replacement.id, existing.id);
    assert!(store.get_job(&existing.id).await.is_err());
    let active = store
        .list_jobs_by_kind_with_visibility(
            JobKind::RuntimeMaintenance,
            10,
            JobVisibility::OnlyEphemeral,
        )
        .await
        .unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].id, replacement.id);
    assert_eq!(
        active[0].runtime_maintenance_payload().unwrap().interval_ms,
        1000
    );
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
        6
    );

    let jobs = store
        .list_jobs_with_visibility(None, None, JobVisibility::IncludeEphemeral)
        .await
        .unwrap();
    let kinds = jobs.iter().map(|job| job.kind).collect::<BTreeSet<_>>();
    assert!(kinds.contains(&JobKind::RuntimeMaintenance));
    assert!(kinds.contains(&JobKind::VoiceStatusSync));
    assert!(kinds.contains(&JobKind::AutomationEvaluation));
    assert!(kinds.contains(&JobKind::AgentSessionRetirement));
    assert!(kinds.contains(&JobKind::StaleWakeProbeSweep));
    assert!(kinds.contains(&JobKind::StaleRunningJobSweep));
    assert!(kinds.contains(&JobKind::EphemeralJobGc));
}

#[tokio::test(flavor = "current_thread")]
async fn maintenance_work_jobs_are_typed_ephemeral_jobs() {
    let jobs = [
        Job::voice_status_sync("job_source"),
        Job::discord_voice_status_snapshot("job_source"),
        Job::automation_evaluation("job_source"),
        Job::agent_session_retirement("job_source"),
        Job::agent_thread_title_refresh(
            "job_source",
            "ags_1",
            "guild",
            "code",
            "thread-1",
            "agent code ags_1",
            2,
        ),
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
        RuntimeScope::voice_channel("guild", "channel"),
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
    let job = Job::text_delivery(
        RuntimeScope::voice_channel("guild", "code"),
        "user-a",
        payload,
    );
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
        RuntimeScope::voice_channel("guild", "code"),
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
        RuntimeScope::voice_channel("guild", "code"),
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

    let rename = Job::discord_forum_thread_rename(
        RuntimeScope::voice_channel("guild", "code"),
        "runtime",
        DiscordForumThreadRenamePayload {
            thread_id: "thread-1".to_string(),
            name: "gRPC and REST".to_string(),
            source_job_id: "job_source".to_string(),
        },
    );
    let decoded = Job::decode(&rename.encode().unwrap()).unwrap();
    assert_eq!(decoded.kind, JobKind::DiscordForumThreadRename);
    assert_eq!(decoded.payload.to_json()["thread_id"], "thread-1");
    assert_eq!(decoded.payload.to_json()["name"], "gRPC and REST");
}

#[tokio::test(flavor = "current_thread")]
async fn discord_typing_indicator_job_round_trips_and_requeues_agent_parent() {
    let typing = Job::discord_typing_indicator(
        RuntimeScope::voice_channel("guild", "code"),
        "user-a",
        DiscordTypingIndicatorPayload {
            action: DiscordTypingAction::Start,
            target: TextTarget {
                kind: TextTargetKind::AgentSession,
                channel_id: String::new(),
                user_id: String::new(),
            },
            source_job_id: "job_agent".to_string(),
            requested_by_user_id: "user-a".to_string(),
            agent_task_attempt: 2,
        },
    );
    let decoded = Job::decode(&typing.encode().unwrap()).unwrap();
    assert_eq!(decoded.kind, JobKind::DiscordTypingIndicator);
    assert_eq!(decoded.payload.to_json()["action"], "start");
    assert_eq!(decoded.payload.to_json()["source_job_id"], "job_agent");
    assert_eq!(decoded.payload.to_json()["agent_task_attempt"], 2);

    let mut completed = decoded;
    completed.metadata.output = Some(JobOutput::DiscordTypingIndicator(
        DiscordTypingIndicatorOutput {
            action: DiscordTypingAction::Stop,
            target: TextTarget {
                kind: TextTargetKind::Channel,
                channel_id: "thread-1".to_string(),
                user_id: String::new(),
            },
            source_job_id: "job_agent".to_string(),
            status: "stopped".to_string(),
        },
    ));
    let completed = Job::decode(&completed.encode().unwrap()).unwrap();
    assert!(matches!(
        completed.metadata.output,
        Some(JobOutput::DiscordTypingIndicator(_))
    ));

    let raw = tempfile::tempdir().unwrap();
    let store = test_store(&raw.path().join("voice")).await;
    let parent = store
        .create_job(Job::agent_task_for_session(
            "ags_test",
            RuntimeScope::voice_channel("guild", "code"),
            "user-a",
            CommandRequest::agent_task("guild", "code", "user-a", "summarize this"),
        ))
        .await
        .unwrap();
    let child = store.create_child_job(&parent, completed).await.unwrap();
    let mut completed_child = store.get_job(&child.id).await.unwrap();
    completed_child.mark_complete();
    store.update_job(&completed_child).await.unwrap();

    let resolved = store.resolve_waiting_jobs().await.unwrap();

    assert_eq!(resolved.len(), 1);
    assert_eq!(
        store.get_job(&parent.id).await.unwrap().state,
        JobState::Queued
    );
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

    let deafen = Job::discord_voice_deafen(
        "guild",
        "code",
        "user-a",
        DiscordVoiceDeafenPayload {
            session_id: "cap_1".to_string(),
            deafened: true,
            source_job_id: "job_parent".to_string(),
            reason: "deafen_listening".to_string(),
        },
    );
    let decoded = Job::decode(&deafen.encode().unwrap()).unwrap();
    assert_eq!(decoded.kind, JobKind::DiscordVoiceDeafen);
    assert!(decoded.discord_voice_deafen_payload().unwrap().deafened);

    let mut completed = decoded;
    completed.metadata.output = Some(JobOutput::DiscordVoiceDeafen(DiscordVoiceDeafenOutput {
        session_id: "cap_1".to_string(),
        deafened: true,
        status: "set".to_string(),
        guild_id: "guild".to_string(),
        voice_channel_id: "code".to_string(),
        message: String::new(),
    }));
    let completed = Job::decode(&completed.encode().unwrap()).unwrap();
    assert!(matches!(
        completed.metadata.output,
        Some(JobOutput::DiscordVoiceDeafen(_))
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

fn encode_pre_v0_3_0_job(job: &Job) -> Vec<u8> {
    let previous = PreV0_3_0Job {
        id: job.id.clone(),
        kind: job.kind,
        guild_id: job.guild_id.clone(),
        voice_channel_id: job.scope_id.clone(),
        state: job.state,
        requested_by_user_id: job.requested_by_user_id.clone(),
        payload: job.payload.clone(),
        attempts: job.attempts,
        created_at: job.created_at.clone(),
        updated_at: job.updated_at.clone(),
        next_run_at: job.next_run_at.clone(),
        started_at: job.started_at.clone(),
        completed_at: job.completed_at.clone(),
        cancelled_at: job.cancelled_at.clone(),
        parent_job_id: job.parent_job_id.clone(),
        root_job_id: job.root_job_id.clone(),
        lineage_depth: job.lineage_depth,
        metadata: job.metadata.clone(),
    };
    let body = bincode::serialize(&previous).unwrap();
    let mut bytes = Vec::with_capacity(10 + body.len());
    bytes.extend_from_slice(b"CLANKJOB");
    bytes.extend_from_slice(&2_u16.to_le_bytes());
    bytes.extend_from_slice(&body);
    bytes
}

fn encode_pre_v0_6_0_agent_task_job(job: &Job) -> Vec<u8> {
    let previous = PreV0_6_0Job {
        id: job.id.clone(),
        kind: job.kind,
        scope_kind: job.scope_kind,
        guild_id: job.guild_id.clone(),
        scope_id: job.scope_id.clone(),
        state: job.state,
        requested_by_user_id: job.requested_by_user_id.clone(),
        payload: job.payload.clone(),
        attempts: job.attempts,
        created_at: job.created_at.clone(),
        updated_at: job.updated_at.clone(),
        next_run_at: job.next_run_at.clone(),
        started_at: job.started_at.clone(),
        completed_at: job.completed_at.clone(),
        cancelled_at: job.cancelled_at.clone(),
        parent_job_id: job.parent_job_id.clone(),
        root_job_id: job.root_job_id.clone(),
        lineage_depth: job.lineage_depth,
        metadata: PreV0_6_0JobMetadata {
            detail: Some(Box::new(PreV0_6_0JobMetadataDetail::AgentTask(
                PreV0_6_0AgentTaskMetadata {
                    dispatch_stdout_preview: "done".to_string(),
                    agent: PreV0_6_0AgentInvocationMetadata {
                        session_id: "codex-session-v3".to_string(),
                        provider: "codex".to_string(),
                        model: "codex-default".to_string(),
                        usage: BinaryPayload::empty(),
                    },
                    response_text: "done".to_string(),
                    ..PreV0_6_0AgentTaskMetadata::default()
                },
            ))),
            ..PreV0_6_0JobMetadata::default()
        },
    };
    let body = bincode::serialize(&previous).unwrap();
    let mut bytes = Vec::with_capacity(10 + body.len());
    bytes.extend_from_slice(b"CLANKJOB");
    bytes.extend_from_slice(&3_u16.to_le_bytes());
    bytes.extend_from_slice(&body);
    bytes
}

async fn column_exists(pool: &sqlx::PgPool, table: &str, column: &str) -> bool {
    let row = sqlx::query(
        r#"
        SELECT EXISTS (
          SELECT 1
          FROM information_schema.columns
          WHERE table_schema = current_schema()
            AND table_name = $1
            AND column_name = $2
        ) AS exists
        "#,
    )
    .bind(table)
    .bind(column)
    .fetch_one(pool)
    .await
    .unwrap();
    sqlx::Row::try_get(&row, "exists").unwrap()
}

async fn column_nullable(pool: &sqlx::PgPool, table: &str, column: &str) -> bool {
    let row = sqlx::query(
        r#"
        SELECT is_nullable
        FROM information_schema.columns
        WHERE table_schema = current_schema()
          AND table_name = $1
          AND column_name = $2
        "#,
    )
    .bind(table)
    .bind(column)
    .fetch_one(pool)
    .await
    .unwrap();
    sqlx::Row::try_get::<String, _>(&row, "is_nullable").unwrap() == "YES"
}

async fn index_exists(pool: &sqlx::PgPool, index: &str) -> bool {
    let row = sqlx::query(
        r#"
        SELECT EXISTS (
          SELECT 1
          FROM pg_indexes
          WHERE schemaname = current_schema()
            AND indexname = $1
        ) AS exists
        "#,
    )
    .bind(index)
    .fetch_one(pool)
    .await
    .unwrap();
    sqlx::Row::try_get(&row, "exists").unwrap()
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
