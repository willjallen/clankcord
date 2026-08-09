
use std::collections::BTreeSet;

use serde::Serialize;
use serde_json::json;

use clankcord::model::job::{
    BinaryPayload, CommandRequest, DiscordTypingAction, DiscordTypingIndicatorPayload, Job,
    JobKind, JobOutput, JobPayload, JobState, TextTarget, TextTargetKind,
};
use clankcord::model::scope::{RuntimeScope, RuntimeScopeKind};
use clankcord::store::JobVisibility;

use crate::support::{initialize_test_config, test_store};

// ---------------------------------------------------------------------------
// Faithful v0.13.0 (blob v8) writers. Variant orders match the shipped
// enums, including the kinds removed at v1.0.0.
// ---------------------------------------------------------------------------

#[derive(Serialize)]
#[allow(dead_code)]
enum V8JobKind {
    AudioSegment,
    WakeActivation,
    AgentTask,
    DiscordTextMessage,
    DiscordSlashCommand,
    TextDelivery,
    DiscordTextSend,
    DiscordForumThreadCreate,
    DiscordForumThreadRename,
    AgentSessionStart,
    AgentSessionSunset,
    AgentSessionResume,
    AgentSessionRetirement,
    AgentThreadTitleRefresh,
    TranscriptPublication,
    RefineTranscript,
    ConfirmationRequired,
    Command,
    RoomAgentPlacement,
    DiscordVoiceJoin,
    DiscordVoiceLeave,
    DiscordVoicePlayback,
    DiscordVoiceMute,
    DiscordVoicePlayAudio,
    RuntimeControl,
    WakeProbe,
    RuntimeMaintenance,
    VoiceStatusSync,
    DiscordVoiceStatusSnapshot,
    AutomationEvaluation,
    StaleWakeProbeSweep,
    StaleRunningJobSweep,
    EphemeralJobGc,
    DiscordVoiceDeafen,
    DiscordTypingIndicator,
    TranscriptionMux,
    TranscriptionMuxPlan,
}

#[derive(Serialize)]
#[allow(dead_code, clippy::large_enum_variant)]
enum V8JobPayload {
    AudioSegment(clankcord::model::job::AudioSegmentPayload),
    WakeActivation(clankcord::model::job::WakeActivationPayload),
    AgentTask(clankcord::model::job::AgentTaskPayload),
    DiscordTextMessage(clankcord::model::job::DiscordTextMessagePayload),
    DiscordSlashCommand(clankcord::model::job::DiscordSlashCommandPayload),
    TextDelivery(clankcord::model::job::TextDeliveryPayload),
    DiscordTextSend(clankcord::model::job::DiscordTextSendPayload),
    DiscordForumThreadCreate(clankcord::model::job::DiscordForumThreadCreatePayload),
    DiscordForumThreadRename(clankcord::model::job::DiscordForumThreadRenamePayload),
    AgentSessionStart(clankcord::model::job::AgentSessionStartPayload),
    AgentSessionSunset(clankcord::model::job::AgentSessionSunsetPayload),
    AgentSessionResume(clankcord::model::job::AgentSessionResumePayload),
    AgentSessionRetirement(clankcord::model::job::AgentSessionRetirementPayload),
    AgentThreadTitleRefresh(clankcord::model::job::AgentThreadTitleRefreshPayload),
    TranscriptPublication(clankcord::model::job::TranscriptPublicationPayload),
    ConfirmationRequired(clankcord::model::job::ConfirmationRequiredPayload),
    Command(clankcord::model::job::CommandPayload),
    RoomAgentPlacement(clankcord::model::job::RoomAgentPlacementPayload),
    DiscordVoiceJoin(clankcord::model::job::DiscordVoiceJoinPayload),
    DiscordVoiceLeave(clankcord::model::job::DiscordVoiceLeavePayload),
    DiscordVoicePlayback(clankcord::model::job::DiscordVoicePlaybackPayload),
    DiscordVoiceMute(clankcord::model::job::DiscordVoiceMutePayload),
    DiscordVoicePlayAudio(clankcord::model::job::DiscordVoicePlayAudioPayload),
    RuntimeControl(clankcord::model::job::RuntimeControlPayload),
    WakeProbe(clankcord::model::job::WakeProbePayload),
    RuntimeMaintenance(clankcord::model::job::RuntimeMaintenancePayload),
    VoiceStatusSync(clankcord::model::job::VoiceStatusSyncPayload),
    DiscordVoiceStatusSnapshot(clankcord::model::job::DiscordVoiceStatusSnapshotPayload),
    AutomationEvaluation(clankcord::model::job::AutomationEvaluationPayload),
    StaleWakeProbeSweep(clankcord::model::job::StaleWakeProbeSweepPayload),
    StaleRunningJobSweep(V8StaleRunningJobSweepPayload),
    EphemeralJobGc(clankcord::model::job::EphemeralJobGcPayload),
    DiscordVoiceDeafen(clankcord::model::job::DiscordVoiceDeafenPayload),
    DiscordTypingIndicator(clankcord::model::job::DiscordTypingIndicatorPayload),
    TranscriptionMux(clankcord::model::job::TranscriptionMuxPayload),
    TranscriptionMuxPlan(clankcord::model::job::TranscriptionMuxPlanPayload),
}

#[derive(Serialize)]
struct V8StaleRunningJobSweepPayload {
    source_job_id: String,
    timeout_minutes: i64,
}

#[derive(Serialize, Default)]
struct V8AgentInvocationMetadata {
    session_id: String,
    provider: String,
    model: String,
    reasoning_effort: String,
    fast_mode: bool,
    usage: BinaryPayload,
}

#[derive(Serialize)]
struct V8AgentTaskMetadata {
    dispatch_attempts: i64,
    dispatch_error: String,
    dispatch_error_after_cancel: String,
    workdir_path: String,
    prompt_path: String,
    result_path: String,
    raw_result_path: String,
    dispatch_stdout_preview: String,
    dispatch_stderr: String,
    agent: V8AgentInvocationMetadata,
    preflight: Option<()>,
    response_text: String,
    command: String,
    result_suppressed: bool,
    discord_post: Option<()>,
}

#[derive(Serialize)]
enum V8JobMetadataDetail {
    AgentTask(V8AgentTaskMetadata),
}

#[derive(Serialize)]
struct V8JobMetadata {
    detail: Option<Box<V8JobMetadataDetail>>,
    error: String,
    timed_out_at: String,
    cancel_requested: bool,
    cancelled_by_user_id: String,
    output: Option<JobOutput>,
}

#[derive(Serialize)]
struct V8Job {
    id: String,
    kind: V8JobKind,
    scope_kind: RuntimeScopeKind,
    guild_id: String,
    scope_id: String,
    state: JobState,
    requested_by_user_id: String,
    payload: V8JobPayload,
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
    metadata: V8JobMetadata,
}

fn v8_blob(job: V8Job) -> Vec<u8> {
    let body = bincode::serialize(&job).unwrap();
    let mut bytes = Vec::with_capacity(10 + body.len());
    bytes.extend_from_slice(b"CLANKJOB");
    bytes.extend_from_slice(&8_u16.to_le_bytes());
    bytes.extend_from_slice(&body);
    bytes
}

fn v8_job_shell(
    job: &Job,
    kind: V8JobKind,
    payload: V8JobPayload,
    detail: Option<V8JobMetadataDetail>,
) -> V8Job {
    V8Job {
        id: job.id.clone(),
        kind,
        scope_kind: job.scope_kind,
        guild_id: job.guild_id.clone(),
        scope_id: job.scope_id.clone(),
        state: job.state,
        requested_by_user_id: job.requested_by_user_id.clone(),
        payload,
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
        metadata: V8JobMetadata {
            detail: detail.map(Box::new),
            error: String::new(),
            timed_out_at: String::new(),
            cancel_requested: false,
            cancelled_by_user_id: String::new(),
            output: None,
        },
    }
}

async fn overwrite_blob(store: &clankcord::store::TimelineStore, job_id: &str, blob: Vec<u8>) {
    sqlx::query("UPDATE job_payloads SET payload_blob = $1 WHERE job_id = $2")
        .bind(blob)
        .bind(job_id)
        .execute(&store.pool)
        .await
        .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn v1_0_0_migrates_a_live_v0_13_0_database_forward() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(&raw.path().join("voice")).await;

    // --- shape the database back to v0.13.0 ---
    sqlx::raw_sql(
        r#"
        ALTER TABLE job_dependencies
          ADD COLUMN IF NOT EXISTS resolution_policy TEXT NOT NULL DEFAULT 'parent_resumes';
        DROP TABLE IF EXISTS job_schedules;
        DROP TABLE IF EXISTS wake_circuit;
        DELETE FROM clankcord_schema_migrations WHERE version = '1.0.0';
        "#,
    )
    .execute(&store.pool)
    .await
    .unwrap();

    // A running agent task with a recorded dispatch outcome (v8 wire).
    let command = CommandRequest::from_json(&json!({
        "command_kind": "agent_task",
        "guild_id": "guild",
        "scope_id": "code",
        "requested_by_user_id": "user-a",
        "arguments": {"request": "summarize"}
    }))
    .unwrap();
    let mut agent = Job::agent_task_for_session(
        "ags_migrate",
        RuntimeScope::voice_channel("guild", "code"),
        "user-a",
        command,
    );
    agent.mark_running();
    let agent = store.create_job(agent).await.unwrap();
    let agent_payload = match &agent.payload {
        JobPayload::AgentTask(payload) => payload.clone(),
        _ => unreachable!(),
    };
    overwrite_blob(
        &store,
        &agent.id,
        v8_blob(v8_job_shell(
            &agent,
            V8JobKind::AgentTask,
            V8JobPayload::AgentTask(agent_payload),
            Some(V8JobMetadataDetail::AgentTask(V8AgentTaskMetadata {
                dispatch_attempts: 1,
                dispatch_error: String::new(),
                dispatch_error_after_cancel: String::new(),
                workdir_path: "/tmp/w".into(),
                prompt_path: "/tmp/p".into(),
                result_path: "/tmp/r".into(),
                raw_result_path: "/tmp/raw".into(),
                dispatch_stdout_preview: "RESPONSE_SUBMITTED".into(),
                dispatch_stderr: String::new(),
                agent: V8AgentInvocationMetadata::default(),
                preflight: None,
                response_text: "RESPONSE_SUBMITTED".into(),
                command: "codex exec".into(),
                result_suppressed: false,
                discord_post: None,
            })),
        )),
    )
    .await;

    // Kinds whose variant indexes shift at v1.0.0 (after the removals).
    let sync = store
        .create_job(Job::voice_status_sync("legacy_source"))
        .await
        .unwrap();
    let sync_payload = match &sync.payload {
        JobPayload::VoiceStatusSync(payload) => payload.clone(),
        _ => unreachable!(),
    };
    overwrite_blob(
        &store,
        &sync.id,
        v8_blob(v8_job_shell(
            &sync,
            V8JobKind::VoiceStatusSync,
            V8JobPayload::VoiceStatusSync(sync_payload),
            None,
        )),
    )
    .await;

    let typing = store
        .create_job(Job::discord_typing_indicator(
            RuntimeScope::voice_channel("guild", "code"),
            "user-a",
            DiscordTypingIndicatorPayload {
                action: DiscordTypingAction::Start,
                target: TextTarget {
                    kind: TextTargetKind::Channel,
                    channel_id: "chan".into(),
                    user_id: String::new(),
                },
                source_job_id: agent.id.clone(),
                requested_by_user_id: "user-a".into(),
                agent_task_attempt: 1,
            },
        ))
        .await
        .unwrap();
    let typing_payload = match &typing.payload {
        JobPayload::DiscordTypingIndicator(payload) => payload.clone(),
        _ => unreachable!(),
    };
    overwrite_blob(
        &store,
        &typing.id,
        v8_blob(v8_job_shell(
            &typing,
            V8JobKind::DiscordTypingIndicator,
            V8JobPayload::DiscordTypingIndicator(typing_payload),
            None,
        )),
    )
    .await;

    // A row of a removed kind, with a v8 blob, plus a dependency edge.
    sqlx::raw_sql(
        r#"
        INSERT INTO jobs(job_id, kind, scope_kind, guild_id, scope_id, state,
                         created_at_ms, updated_at_ms, ready_at_ms, terminal,
                         failed, ephemeral, cancellable, lane, ordering_key,
                         command_kind, source_job_id, stream_id, target_job_id,
                         speaker_user_id)
        VALUES ('job_sweep_legacy', 'stale_running_job_sweep', 'runtime', '', 'runtime',
                'complete', 0, 0, 0, TRUE, FALSE, TRUE, FALSE, 'maintenance',
                'runtime:maintenance', '', '', '', '', '');
        INSERT INTO job_payloads(job_id, payload_blob) VALUES ('job_sweep_legacy', '\x00'::bytea);
        "#,
    )
    .execute(&store.pool)
    .await
    .unwrap();

    // --- migrate ---
    let applied = store.run_pending_schema_migrations().await.unwrap();
    assert_eq!(applied.len(), 1);
    assert_eq!(applied[0].version, "1.0.0");

    // Removed kinds purged before re-encode.
    let sweep_rows: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM jobs WHERE kind = 'stale_running_job_sweep'")
            .fetch_one(&store.pool)
            .await
            .unwrap();
    assert_eq!(sweep_rows, 0);

    // Schema changes landed.
    let column: Option<String> = sqlx::query_scalar(
        r#"
        SELECT column_name FROM information_schema.columns
        WHERE table_schema = current_schema()
          AND table_name = 'job_dependencies'
          AND column_name = 'resolution_policy'
        "#,
    )
    .fetch_optional(&store.pool)
    .await
    .unwrap();
    assert_eq!(column, None, "resolution_policy is dropped");
    let schedules: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM job_schedules")
        .fetch_one(&store.pool)
        .await
        .unwrap();
    assert_eq!(schedules, 0, "job_schedules exists and is empty");
    store.wake_circuit_row().await.unwrap();

    // Blobs re-encoded: current decode works and kinds survived the shift.
    let migrated_agent = store.get_job(&agent.id).await.unwrap();
    assert_eq!(migrated_agent.kind, JobKind::AgentTask);
    let metadata = migrated_agent.metadata.to_json();
    assert_eq!(metadata["agent_task"]["phase"], json!("await_delivery"));
    assert_eq!(
        metadata["agent_task"]["response_text"],
        json!("RESPONSE_SUBMITTED")
    );

    let migrated_sync = store.get_job(&sync.id).await.unwrap();
    assert_eq!(migrated_sync.kind, JobKind::VoiceStatusSync);
    let migrated_typing = store.get_job(&typing.id).await.unwrap();
    assert_eq!(migrated_typing.kind, JobKind::DiscordTypingIndicator);

    // Every surviving row decodes with the current reader.
    let kinds = store
        .list_jobs_with_visibility(None, None, JobVisibility::IncludeEphemeral)
        .await
        .unwrap()
        .into_iter()
        .map(|job| job.kind)
        .collect::<BTreeSet<_>>();
    assert!(kinds.contains(&JobKind::AgentTask));
    assert!(kinds.contains(&JobKind::VoiceStatusSync));
    assert!(kinds.contains(&JobKind::DiscordTypingIndicator));
}
