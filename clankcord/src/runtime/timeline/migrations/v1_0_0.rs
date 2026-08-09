//! v1.0.0: the overhaul release.
//!
//! Bridges a live v0.13.0 database across every durable contract the
//! restructure changed:
//! - removed job kinds (`refine_transcript`, `stale_running_job_sweep`) are
//!   purged: rows, payload blobs, and dependency edges;
//! - `job_dependencies.resolution_policy` (written, never read) is dropped —
//!   resume policy lives in the job spec;
//! - `job_schedules` (the single recurring-work clock) and `wake_circuit`
//!   (the durable wake-provider breaker) are created;
//! - every job payload blob is re-encoded from the frozen v8 wire format to
//!   v9 (payload variant indexes shifted; agent task metadata gained typed
//!   outcome, phase, and delivery deadline). In-flight agent tasks map onto
//!   the new phase machine: a recorded dispatch outcome resumes in
//!   AwaitDelivery with an immediate deadline; anything else re-dispatches.

use serde::Deserialize;
use sqlx::Row as SqlxRow;

use crate::Result;
use crate::model::job::Job;
use crate::model::job::{AgentTaskMetadata, AgentTaskOutcome, AgentTaskPhase};
use crate::runtime::timeline::isoformat_z;

pub(super) async fn run(transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>) -> Result<()> {
    purge_removed_job_kinds(transaction).await?;
    drop_resolution_policy(transaction).await?;
    create_job_schedules(transaction).await?;
    create_wake_circuit(transaction).await?;
    reencode_job_payloads(transaction).await?;
    Ok(())
}

async fn purge_removed_job_kinds(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<()> {
    sqlx::raw_sql(
        r#"
        DELETE FROM job_dependencies
        WHERE parent_job_id IN (
            SELECT job_id FROM jobs
            WHERE kind IN ('refine_transcript', 'stale_running_job_sweep')
          )
          OR child_job_id IN (
            SELECT job_id FROM jobs
            WHERE kind IN ('refine_transcript', 'stale_running_job_sweep')
          );
        DELETE FROM job_payloads
        WHERE job_id IN (
          SELECT job_id FROM jobs
          WHERE kind IN ('refine_transcript', 'stale_running_job_sweep')
        );
        DELETE FROM jobs
        WHERE kind IN ('refine_transcript', 'stale_running_job_sweep');
        "#,
    )
    .execute(transaction.as_mut())
    .await?;
    Ok(())
}

async fn drop_resolution_policy(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<()> {
    sqlx::raw_sql("ALTER TABLE job_dependencies DROP COLUMN IF EXISTS resolution_policy;")
        .execute(transaction.as_mut())
        .await?;
    Ok(())
}

async fn create_job_schedules(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<()> {
    sqlx::raw_sql(
        r#"
        CREATE TABLE IF NOT EXISTS job_schedules (
          schedule_id TEXT PRIMARY KEY,
          kind TEXT NOT NULL,
          payload_json JSONB NOT NULL,
          interval_ms BIGINT NOT NULL,
          enabled BOOLEAN NOT NULL DEFAULT TRUE,
          next_due_at_ms BIGINT NOT NULL,
          last_submitted_at_ms BIGINT,
          last_job_id TEXT NOT NULL DEFAULT '',
          created_at_ms BIGINT NOT NULL,
          updated_at_ms BIGINT NOT NULL
        );
        "#,
    )
    .execute(transaction.as_mut())
    .await?;
    Ok(())
}

async fn create_wake_circuit(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<()> {
    sqlx::raw_sql(
        r#"
        CREATE TABLE IF NOT EXISTS wake_circuit (
          circuit_id TEXT PRIMARY KEY,
          consecutive_failures BIGINT NOT NULL DEFAULT 0,
          open_count BIGINT NOT NULL DEFAULT 0,
          open_until_ms BIGINT,
          half_open_started_at_ms BIGINT,
          last_failure_at_ms BIGINT,
          last_success_at_ms BIGINT,
          last_error TEXT NOT NULL DEFAULT '',
          suppressed_probes BIGINT NOT NULL DEFAULT 0,
          updated_at_ms BIGINT NOT NULL
        );
        "#,
    )
    .execute(transaction.as_mut())
    .await?;
    Ok(())
}

const V8_BLOB_MAGIC: &[u8; 8] = b"CLANKJOB";
const V8_BLOB_VERSION: u16 = 8;

async fn reencode_job_payloads(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<()> {
    let rows = sqlx::query("SELECT job_id, payload_blob FROM job_payloads")
        .fetch_all(transaction.as_mut())
        .await?;
    for row in rows {
        let job_id: String = row.try_get("job_id")?;
        let blob: Vec<u8> = row.try_get("payload_blob")?;
        if Job::is_current_payload_blob(&blob) {
            // Already v9: a resumed run or a migration-chain replay that
            // fabricated current blobs. Re-encoding is a no-op either way.
            continue;
        }
        let body = strip_v8_header(&job_id, &blob)?;
        let legacy: V8Job = bincode::deserialize(body)
            .map_err(|error| anyhow::anyhow!("job {job_id} v8 payload decode failed: {error}"))?;
        let current = legacy.into_current()?;
        sqlx::query("UPDATE job_payloads SET payload_blob = $1 WHERE job_id = $2")
            .bind(current.encode()?)
            .bind(&job_id)
            .execute(transaction.as_mut())
            .await?;
    }
    Ok(())
}

fn strip_v8_header<'a>(job_id: &str, blob: &'a [u8]) -> Result<&'a [u8]> {
    if blob.len() < V8_BLOB_MAGIC.len() + 2 {
        anyhow::bail!("job {job_id} payload blob is too short for a v8 header");
    }
    let (magic, rest) = blob.split_at(V8_BLOB_MAGIC.len());
    if magic != V8_BLOB_MAGIC {
        anyhow::bail!("job {job_id} payload blob is not a CLANKJOB blob");
    }
    let (version_bytes, body) = rest.split_at(2);
    let version = u16::from_le_bytes([version_bytes[0], version_bytes[1]]);
    if version != V8_BLOB_VERSION {
        anyhow::bail!("job {job_id} payload blob is v{version}; the v1.0.0 migration expects v8");
    }
    Ok(body)
}

// ---------------------------------------------------------------------------
// Frozen v0.13.0 wire format. Payload struct shapes are unchanged at v1.0.0,
// so variants reuse the live structs; only the variant SETS and the agent
// task metadata differ.
// ---------------------------------------------------------------------------

use crate::model::job::{
    AgentInvocationMetadata, AgentPreflightMetadata, AgentSessionResumePayload,
    AgentSessionRetirementPayload, AgentSessionStartPayload, AgentSessionSunsetPayload,
    AgentTaskPayload, AgentThreadTitleRefreshPayload, AudioSegmentPayload,
    AutomationEvaluationPayload, CommandPayload, ConfirmationRequiredPayload,
    DiscordForumThreadCreatePayload, DiscordForumThreadRenamePayload, DiscordSlashCommandPayload,
    DiscordTextMessagePayload, DiscordTextSendPayload, DiscordTypingIndicatorPayload,
    DiscordVoiceDeafenPayload, DiscordVoiceJoinPayload, DiscordVoiceLeavePayload,
    DiscordVoiceMutePayload, DiscordVoicePlayAudioPayload, DiscordVoicePlaybackPayload,
    DiscordVoiceStatusSnapshotPayload, EphemeralJobGcPayload, JobOutput, JobPayload,
    RoomAgentPlacementPayload, RuntimeControlPayload, RuntimeMaintenancePayload,
    StaleWakeProbeSweepPayload, TextDeliveryPayload, TranscriptPublicationPayload,
    TranscriptionMuxPayload, TranscriptionMuxPlanPayload, VoiceStatusSyncPayload,
    WakeActivationPayload, WakeProbePayload,
};
use crate::model::job::{ConfirmationJobMetadata, DiscordPostMetadata};
use crate::model::job::{JobKind, JobState};
use crate::model::scope::RuntimeScopeKind;

#[derive(Debug, Deserialize)]
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

impl V8Job {
    fn into_current(self) -> Result<Job> {
        Ok(Job {
            id: self.id,
            kind: self.kind.into_current()?,
            scope_kind: self.scope_kind,
            guild_id: self.guild_id,
            scope_id: self.scope_id,
            state: self.state,
            requested_by_user_id: self.requested_by_user_id,
            payload: self.payload.into_current()?,
            attempts: self.attempts,
            created_at: self.created_at,
            updated_at: self.updated_at,
            next_run_at: self.next_run_at,
            started_at: self.started_at,
            completed_at: self.completed_at,
            cancelled_at: self.cancelled_at,
            parent_job_id: self.parent_job_id,
            root_job_id: self.root_job_id,
            lineage_depth: self.lineage_depth,
            metadata: self.metadata.into_current(),
        })
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
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

impl V8JobKind {
    fn into_current(self) -> Result<JobKind> {
        Ok(match self {
            Self::AudioSegment => JobKind::AudioSegment,
            Self::WakeActivation => JobKind::WakeActivation,
            Self::AgentTask => JobKind::AgentTask,
            Self::DiscordTextMessage => JobKind::DiscordTextMessage,
            Self::DiscordSlashCommand => JobKind::DiscordSlashCommand,
            Self::TextDelivery => JobKind::TextDelivery,
            Self::DiscordTextSend => JobKind::DiscordTextSend,
            Self::DiscordForumThreadCreate => JobKind::DiscordForumThreadCreate,
            Self::DiscordForumThreadRename => JobKind::DiscordForumThreadRename,
            Self::AgentSessionStart => JobKind::AgentSessionStart,
            Self::AgentSessionSunset => JobKind::AgentSessionSunset,
            Self::AgentSessionResume => JobKind::AgentSessionResume,
            Self::AgentSessionRetirement => JobKind::AgentSessionRetirement,
            Self::AgentThreadTitleRefresh => JobKind::AgentThreadTitleRefresh,
            Self::TranscriptPublication => JobKind::TranscriptPublication,
            Self::ConfirmationRequired => JobKind::ConfirmationRequired,
            Self::Command => JobKind::Command,
            Self::RoomAgentPlacement => JobKind::RoomAgentPlacement,
            Self::DiscordVoiceJoin => JobKind::DiscordVoiceJoin,
            Self::DiscordVoiceLeave => JobKind::DiscordVoiceLeave,
            Self::DiscordVoicePlayback => JobKind::DiscordVoicePlayback,
            Self::DiscordVoiceMute => JobKind::DiscordVoiceMute,
            Self::DiscordVoicePlayAudio => JobKind::DiscordVoicePlayAudio,
            Self::RuntimeControl => JobKind::RuntimeControl,
            Self::WakeProbe => JobKind::WakeProbe,
            Self::RuntimeMaintenance => JobKind::RuntimeMaintenance,
            Self::VoiceStatusSync => JobKind::VoiceStatusSync,
            Self::DiscordVoiceStatusSnapshot => JobKind::DiscordVoiceStatusSnapshot,
            Self::AutomationEvaluation => JobKind::AutomationEvaluation,
            Self::StaleWakeProbeSweep => JobKind::StaleWakeProbeSweep,
            Self::EphemeralJobGc => JobKind::EphemeralJobGc,
            Self::DiscordVoiceDeafen => JobKind::DiscordVoiceDeafen,
            Self::DiscordTypingIndicator => JobKind::DiscordTypingIndicator,
            Self::TranscriptionMux => JobKind::TranscriptionMux,
            Self::TranscriptionMuxPlan => JobKind::TranscriptionMuxPlan,
            Self::RefineTranscript | Self::StaleRunningJobSweep => {
                anyhow::bail!("rows for removed job kinds must be purged before payload re-encode")
            }
        })
    }
}

#[derive(Debug, Deserialize)]
enum V8JobPayload {
    AudioSegment(AudioSegmentPayload),
    WakeActivation(WakeActivationPayload),
    AgentTask(AgentTaskPayload),
    DiscordTextMessage(DiscordTextMessagePayload),
    DiscordSlashCommand(DiscordSlashCommandPayload),
    TextDelivery(TextDeliveryPayload),
    DiscordTextSend(DiscordTextSendPayload),
    DiscordForumThreadCreate(DiscordForumThreadCreatePayload),
    DiscordForumThreadRename(DiscordForumThreadRenamePayload),
    AgentSessionStart(AgentSessionStartPayload),
    AgentSessionSunset(AgentSessionSunsetPayload),
    AgentSessionResume(AgentSessionResumePayload),
    AgentSessionRetirement(AgentSessionRetirementPayload),
    AgentThreadTitleRefresh(AgentThreadTitleRefreshPayload),
    TranscriptPublication(TranscriptPublicationPayload),
    ConfirmationRequired(ConfirmationRequiredPayload),
    Command(CommandPayload),
    RoomAgentPlacement(RoomAgentPlacementPayload),
    DiscordVoiceJoin(DiscordVoiceJoinPayload),
    DiscordVoiceLeave(DiscordVoiceLeavePayload),
    DiscordVoicePlayback(DiscordVoicePlaybackPayload),
    DiscordVoiceMute(DiscordVoiceMutePayload),
    DiscordVoicePlayAudio(DiscordVoicePlayAudioPayload),
    RuntimeControl(RuntimeControlPayload),
    WakeProbe(WakeProbePayload),
    RuntimeMaintenance(RuntimeMaintenancePayload),
    VoiceStatusSync(VoiceStatusSyncPayload),
    DiscordVoiceStatusSnapshot(DiscordVoiceStatusSnapshotPayload),
    AutomationEvaluation(AutomationEvaluationPayload),
    StaleWakeProbeSweep(StaleWakeProbeSweepPayload),
    #[allow(dead_code)]
    StaleRunningJobSweep(V8StaleRunningJobSweepPayload),
    EphemeralJobGc(EphemeralJobGcPayload),
    DiscordVoiceDeafen(DiscordVoiceDeafenPayload),
    DiscordTypingIndicator(DiscordTypingIndicatorPayload),
    TranscriptionMux(TranscriptionMuxPayload),
    TranscriptionMuxPlan(TranscriptionMuxPlanPayload),
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct V8StaleRunningJobSweepPayload {
    source_job_id: String,
    timeout_minutes: i64,
}

impl V8JobPayload {
    fn into_current(self) -> Result<JobPayload> {
        Ok(match self {
            Self::AudioSegment(payload) => JobPayload::AudioSegment(payload),
            Self::WakeActivation(payload) => JobPayload::WakeActivation(payload),
            Self::AgentTask(payload) => JobPayload::AgentTask(payload),
            Self::DiscordTextMessage(payload) => JobPayload::DiscordTextMessage(payload),
            Self::DiscordSlashCommand(payload) => JobPayload::DiscordSlashCommand(payload),
            Self::TextDelivery(payload) => JobPayload::TextDelivery(payload),
            Self::DiscordTextSend(payload) => JobPayload::DiscordTextSend(payload),
            Self::DiscordForumThreadCreate(payload) => {
                JobPayload::DiscordForumThreadCreate(payload)
            }
            Self::DiscordForumThreadRename(payload) => {
                JobPayload::DiscordForumThreadRename(payload)
            }
            Self::AgentSessionStart(payload) => JobPayload::AgentSessionStart(payload),
            Self::AgentSessionSunset(payload) => JobPayload::AgentSessionSunset(payload),
            Self::AgentSessionResume(payload) => JobPayload::AgentSessionResume(payload),
            Self::AgentSessionRetirement(payload) => JobPayload::AgentSessionRetirement(payload),
            Self::AgentThreadTitleRefresh(payload) => JobPayload::AgentThreadTitleRefresh(payload),
            Self::TranscriptPublication(payload) => JobPayload::TranscriptPublication(payload),
            Self::ConfirmationRequired(payload) => JobPayload::ConfirmationRequired(payload),
            Self::Command(payload) => JobPayload::Command(payload),
            Self::RoomAgentPlacement(payload) => JobPayload::RoomAgentPlacement(payload),
            Self::DiscordVoiceJoin(payload) => JobPayload::DiscordVoiceJoin(payload),
            Self::DiscordVoiceLeave(payload) => JobPayload::DiscordVoiceLeave(payload),
            Self::DiscordVoicePlayback(payload) => JobPayload::DiscordVoicePlayback(payload),
            Self::DiscordVoiceMute(payload) => JobPayload::DiscordVoiceMute(payload),
            Self::DiscordVoicePlayAudio(payload) => JobPayload::DiscordVoicePlayAudio(payload),
            Self::RuntimeControl(payload) => JobPayload::RuntimeControl(payload),
            Self::WakeProbe(payload) => JobPayload::WakeProbe(payload),
            Self::RuntimeMaintenance(payload) => JobPayload::RuntimeMaintenance(payload),
            Self::VoiceStatusSync(payload) => JobPayload::VoiceStatusSync(payload),
            Self::DiscordVoiceStatusSnapshot(payload) => {
                JobPayload::DiscordVoiceStatusSnapshot(payload)
            }
            Self::AutomationEvaluation(payload) => JobPayload::AutomationEvaluation(payload),
            Self::StaleWakeProbeSweep(payload) => JobPayload::StaleWakeProbeSweep(payload),
            Self::EphemeralJobGc(payload) => JobPayload::EphemeralJobGc(payload),
            Self::DiscordVoiceDeafen(payload) => JobPayload::DiscordVoiceDeafen(payload),
            Self::DiscordTypingIndicator(payload) => JobPayload::DiscordTypingIndicator(payload),
            Self::TranscriptionMux(payload) => JobPayload::TranscriptionMux(payload),
            Self::TranscriptionMuxPlan(payload) => JobPayload::TranscriptionMuxPlan(payload),
            Self::StaleRunningJobSweep(_) => {
                anyhow::bail!("rows for removed job kinds must be purged before payload re-encode")
            }
        })
    }
}

#[derive(Debug, Deserialize)]
#[allow(clippy::large_enum_variant)] // wire/decision enums: boxing buys nothing on the encoded form
enum V8JobMetadataDetail {
    AgentTask(V8AgentTaskMetadata),
    Confirmation(ConfirmationJobMetadata),
}

#[derive(Debug, Deserialize)]
struct V8JobMetadata {
    detail: Option<Box<V8JobMetadataDetail>>,
    error: String,
    timed_out_at: String,
    cancel_requested: bool,
    cancelled_by_user_id: String,
    output: Option<JobOutput>,
}

impl V8JobMetadata {
    fn into_current(self) -> crate::model::job::JobMetadata {
        let mut metadata = crate::model::job::JobMetadata {
            detail: None,
            error: self.error,
            timed_out_at: self.timed_out_at,
            cancel_requested: self.cancel_requested,
            cancelled_by_user_id: self.cancelled_by_user_id,
            output: self.output,
        };
        match self.detail.map(|detail| *detail) {
            Some(V8JobMetadataDetail::AgentTask(task)) => {
                metadata.set_agent_task(task.into_current());
            }
            Some(V8JobMetadataDetail::Confirmation(confirmation)) => {
                *metadata.confirmation_mut() = confirmation;
            }
            None => {}
        }
        metadata
    }
}

#[derive(Debug, Deserialize)]
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
    agent: AgentInvocationMetadata,
    preflight: Option<AgentPreflightMetadata>,
    response_text: String,
    command: String,
    result_suppressed: bool,
    discord_post: Option<DiscordPostMetadata>,
}

impl V8AgentTaskMetadata {
    fn into_current(self) -> AgentTaskMetadata {
        // A v8 task that already recorded a dispatch outcome resumes in
        // AwaitDelivery with an immediate deadline (classify on next claim);
        // anything else re-dispatches from the top.
        let dispatched = !self.response_text.trim().is_empty()
            || !self.dispatch_error.trim().is_empty()
            || !self.dispatch_stdout_preview.trim().is_empty();
        let (phase, await_delivery_until) = if dispatched {
            (AgentTaskPhase::AwaitDelivery, isoformat_z(None))
        } else {
            (AgentTaskPhase::Dispatch, String::new())
        };
        AgentTaskMetadata {
            outcome: AgentTaskOutcome::Pending,
            phase,
            await_delivery_until,
            dispatch_attempts: self.dispatch_attempts,
            dispatch_error: self.dispatch_error,
            dispatch_error_after_cancel: self.dispatch_error_after_cancel,
            workdir_path: self.workdir_path,
            prompt_path: self.prompt_path,
            result_path: self.result_path,
            raw_result_path: self.raw_result_path,
            dispatch_stdout_preview: self.dispatch_stdout_preview,
            dispatch_stderr: self.dispatch_stderr,
            agent: self.agent,
            preflight: self.preflight,
            response_text: self.response_text,
            command: self.command,
            result_suppressed: self.result_suppressed,
            discord_post: self.discord_post,
        }
    }
}
