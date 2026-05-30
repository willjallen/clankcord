use crate::Result;
use crate::runtime::jobs::{
    AgentSessionResumePayload, AgentSessionRetirementPayload, AgentSessionStartOutput,
    AgentSessionStartPayload, AgentSessionSunsetPayload, AgentTaskMetadata, AgentTaskPayload,
    AgentThreadTitleRefreshPayload, AudioSegmentPayload, AutomationEvaluationPayload,
    BinaryPayload, CommandPayload, ConfirmationRequiredPayload, DiscordForumThreadCreateOutput,
    DiscordForumThreadCreatePayload, DiscordForumThreadRenameOutput,
    DiscordForumThreadRenamePayload, DiscordSlashCommandPayload, DiscordTextMessagePayload,
    DiscordTextSendOutput, DiscordTextSendPayload, DiscordTypingIndicatorOutput,
    DiscordTypingIndicatorPayload, DiscordVoiceDeafenOutput, DiscordVoiceDeafenPayload,
    DiscordVoiceJoinOutput, DiscordVoiceJoinPayload, DiscordVoiceLeaveOutput,
    DiscordVoiceLeavePayload, DiscordVoiceMuteOutput, DiscordVoiceMutePayload,
    DiscordVoicePlayAudioOutput, DiscordVoicePlayAudioPayload, DiscordVoicePlaybackOutput,
    DiscordVoicePlaybackPayload, DiscordVoiceStatusSnapshotOutput,
    DiscordVoiceStatusSnapshotPayload, EphemeralJobGcPayload, JobCreatedOutput, JobMetadata,
    JobOutput, RoomAgentPlacementOutput, RoomAgentPlacementPayload, RuntimeControlOutput,
    RuntimeControlPayload, RuntimeMaintenancePayload, StaleRunningJobSweepPayload,
    StaleWakeProbeSweepPayload, TextDeliveryOutput, TextDeliveryPayload,
    TranscriptPublicationOutput, TranscriptPublicationPayload, VoiceStatusSyncPayload,
    WakeActivationPayload, WakeProbePayload,
};
use crate::runtime::{Job, JobKind, JobPayload, JobState, RuntimeScopeKind};
use crate::runtime::{VoiceBotStatus, VoiceCaptureSessionStatus};
use serde::{Deserialize, Serialize};

const JOB_PAYLOAD_BLOB_MAGIC: &[u8; 8] = b"CLANKJOB";
const PRE_V0_8_0_JOB_PAYLOAD_BLOB_VERSION: u16 = 5;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[allow(dead_code)]
struct PreV0_8_0TranscriptPublicationPayload {
    publication_id: String,
    live: bool,
    removed_boolean_slot: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[allow(dead_code)]
struct PreV0_8_0RefineTranscriptPayload {
    window_id: String,
    publication_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
enum PreV0_8_0JobPayload {
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
    TranscriptPublication(PreV0_8_0TranscriptPublicationPayload),
    RefineTranscript(PreV0_8_0RefineTranscriptPayload),
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
    StaleRunningJobSweep(StaleRunningJobSweepPayload),
    EphemeralJobGc(EphemeralJobGcPayload),
    DiscordVoiceDeafen(DiscordVoiceDeafenPayload),
    DiscordTypingIndicator(DiscordTypingIndicatorPayload),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PreV0_8_0Job {
    id: String,
    kind: JobKind,
    scope_kind: RuntimeScopeKind,
    guild_id: String,
    scope_id: String,
    state: JobState,
    requested_by_user_id: String,
    payload: PreV0_8_0JobPayload,
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
    metadata: PreV0_8_0JobMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
struct PreV0_8_0ConfirmationJobMetadata {
    delivery: String,
    channel_id: String,
    message_id: String,
    post_error: String,
    approved_by_user_id: String,
    approved_at: String,
    approval_error: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
enum PreV0_8_0JobMetadataDetail {
    AgentTask(AgentTaskMetadata),
    Confirmation(PreV0_8_0ConfirmationJobMetadata),
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
struct PreV0_8_0JobMetadata {
    detail: Option<Box<PreV0_8_0JobMetadataDetail>>,
    error: String,
    timed_out_at: String,
    cancel_requested: bool,
    cancelled_by_user_id: String,
    output: Option<PreV0_8_0JobOutput>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PreV0_8_0DiscordVoiceLeaveOutput {
    session_id: String,
    status: String,
    session: Option<VoiceCaptureSessionStatus>,
    bot_status: Option<VoiceBotStatus>,
    guild_id: String,
    voice_channel_id: String,
    capture_run_id: String,
    audio_jobs: Vec<PreV0_8_0Job>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
enum PreV0_8_0JobOutput {
    Empty,
    JobCreated(JobCreatedOutput),
    RuntimeControl(RuntimeControlOutput),
    TextDelivery(TextDeliveryOutput),
    DiscordTextSend(DiscordTextSendOutput),
    DiscordForumThreadCreate(DiscordForumThreadCreateOutput),
    DiscordForumThreadRename(DiscordForumThreadRenameOutput),
    AgentSessionStart(AgentSessionStartOutput),
    TranscriptPublication(TranscriptPublicationOutput),
    RoomAgentPlacement(RoomAgentPlacementOutput),
    DiscordVoiceJoin(DiscordVoiceJoinOutput),
    DiscordVoiceLeave(PreV0_8_0DiscordVoiceLeaveOutput),
    DiscordVoicePlayback(DiscordVoicePlaybackOutput),
    DiscordVoiceMute(DiscordVoiceMuteOutput),
    DiscordVoicePlayAudio(DiscordVoicePlayAudioOutput),
    DiscordVoiceStatusSnapshot(DiscordVoiceStatusSnapshotOutput),
    Record(BinaryPayload),
    DiscordVoiceDeafen(DiscordVoiceDeafenOutput),
    DiscordTypingIndicator(DiscordTypingIndicatorOutput),
}

pub(super) async fn run(transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>) -> Result<()> {
    create_transcription_slots(transaction).await?;
    delete_refinement_jobs(transaction).await?;
    drop_authoritative_spans(transaction).await?;
    relabel_existing_local_speech_events(transaction).await?;
    reencode_job_payloads(transaction).await?;
    Ok(())
}

async fn create_transcription_slots(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<()> {
    sqlx::raw_sql(
        r#"
        CREATE TABLE IF NOT EXISTS transcription_slots (
          slot_id TEXT PRIMARY KEY,
          source_job_id TEXT NOT NULL UNIQUE,
          mux_job_id TEXT NOT NULL DEFAULT '',
          state TEXT NOT NULL DEFAULT 'queued',
          guild_id TEXT NOT NULL DEFAULT '',
          voice_channel_id TEXT NOT NULL DEFAULT '',
          capture_run_id TEXT NOT NULL DEFAULT '',
          voice_bot_id TEXT NOT NULL DEFAULT '',
          voice_bot_discord_user_id TEXT NOT NULL DEFAULT '',
          speaker_user_id TEXT NOT NULL DEFAULT '',
          speaker_label TEXT NOT NULL DEFAULT '',
          speaker_username TEXT NOT NULL DEFAULT '',
          segment_index BIGINT NOT NULL DEFAULT 0,
          segment_start_ms BIGINT NOT NULL DEFAULT 0,
          segment_end_ms BIGINT NOT NULL DEFAULT 0,
          duration_ms BIGINT NOT NULL DEFAULT 0,
          source_audio_path TEXT NOT NULL DEFAULT '',
          audio_checksum TEXT NOT NULL DEFAULT '',
          audio_bytes BIGINT NOT NULL DEFAULT 0,
          audio_format TEXT NOT NULL DEFAULT '',
          sample_rate_hz BIGINT NOT NULL DEFAULT 0,
          channels BIGINT NOT NULL DEFAULT 0,
          sample_width_bits BIGINT NOT NULL DEFAULT 0,
          post_processing TEXT NOT NULL DEFAULT '',
          transcription_source_id TEXT NOT NULL DEFAULT '',
          provider TEXT NOT NULL DEFAULT '',
          model TEXT NOT NULL DEFAULT '',
          priority BIGINT NOT NULL DEFAULT 0,
          mux_stream_id TEXT NOT NULL DEFAULT '',
          mux_start_ms BIGINT,
          mux_end_ms BIGINT,
          guard_before_ms BIGINT NOT NULL DEFAULT 0,
          guard_after_ms BIGINT NOT NULL DEFAULT 0,
          created_at_ms BIGINT NOT NULL,
          updated_at_ms BIGINT NOT NULL,
          payload_json JSONB NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_transcription_slots_state_priority
          ON transcription_slots(state, priority DESC, created_at_ms, slot_id);
        CREATE INDEX IF NOT EXISTS idx_transcription_slots_scope_speaker_time
          ON transcription_slots(guild_id, voice_channel_id, speaker_user_id, segment_start_ms, segment_end_ms);
        CREATE INDEX IF NOT EXISTS idx_transcription_slots_mux_job
          ON transcription_slots(mux_job_id, slot_id)
          WHERE mux_job_id <> '';
        "#
    )
    .execute(transaction.as_mut())
    .await?;
    Ok(())
}

async fn delete_refinement_jobs(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<()> {
    sqlx::raw_sql(
        r#"
        DELETE FROM job_dependencies
        WHERE parent_job_id IN (SELECT job_id FROM jobs WHERE kind = 'refine_transcript')
           OR child_job_id IN (SELECT job_id FROM jobs WHERE kind = 'refine_transcript');
        DELETE FROM job_payloads
        WHERE job_id IN (SELECT job_id FROM jobs WHERE kind = 'refine_transcript');
        DELETE FROM jobs
        WHERE kind = 'refine_transcript';
        UPDATE publications
        SET payload_json = payload_json
          - 'refinement_job_id'
          - 'refined_artifact_path'
          - 'recording_artifact_path'
          - 'speaker_alignment_artifact_path'
          - 'refine_requested'
        WHERE payload_json ?| array[
          'refinement_job_id',
          'refined_artifact_path',
          'recording_artifact_path',
          'speaker_alignment_artifact_path',
          'refine_requested'
        ];
        "#,
    )
    .execute(transaction.as_mut())
    .await?;
    Ok(())
}

async fn drop_authoritative_spans(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<()> {
    sqlx::raw_sql("DROP TABLE IF EXISTS authoritative_spans;")
        .execute(transaction.as_mut())
        .await?;
    Ok(())
}

async fn relabel_existing_local_speech_events(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<()> {
    sqlx::raw_sql(
        r#"
        UPDATE timeline_events
        SET payload_json =
          jsonb_set(
            jsonb_set(
              jsonb_set(
                jsonb_set(
                  payload_json,
                  '{transcription_source_id}',
                  to_jsonb('local-granite'::text),
                  true
                ),
                '{stt_provider}',
                to_jsonb('openai_compatible'::text),
                true
              ),
              '{stt_model}',
              to_jsonb('local-granite'::text),
              true
            ),
            '{stt}',
            COALESCE(payload_json->'stt', '{}'::jsonb)
              || jsonb_build_object(
                'provider', 'openai_compatible',
                'transcription_source_id', 'local-granite',
                'model', 'local-granite'
              ),
            true
          )
        WHERE event_kind = 'speech_segment'
          AND COALESCE(payload_json->>'transcription_source_id', '') = ''
          AND COALESCE(payload_json->>'stt_provider', 'local') IN ('', 'local');
        "#,
    )
    .execute(transaction.as_mut())
    .await?;
    Ok(())
}

async fn reencode_job_payloads(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<()> {
    let rows = sqlx::query(
        r#"
        SELECT job_id, payload_blob
        FROM job_payloads
        ORDER BY job_id
        "#,
    )
    .fetch_all(transaction.as_mut())
    .await?;
    for row in rows {
        let job_id: String = sqlx::Row::try_get(&row, "job_id")?;
        let payload_blob: Vec<u8> = sqlx::Row::try_get(&row, "payload_blob")?;
        if Job::is_current_payload_blob(&payload_blob) {
            continue;
        }
        let job = decode_pre_v0_8_0_job_payload_blob(&payload_blob).map_err(|error| {
            anyhow::anyhow!("migrating v0.8.0 job payload blob {job_id}: {error:#}")
        })?;
        sqlx::query(
            r#"
            UPDATE job_payloads
            SET payload_blob = $2
            WHERE job_id = $1
            "#,
        )
        .bind(&job.id)
        .bind(job.encode()?)
        .execute(transaction.as_mut())
        .await?;
    }
    Ok(())
}

fn decode_pre_v0_8_0_job_payload_blob(bytes: &[u8]) -> Result<Job> {
    let body = pre_v0_8_0_envelope_body(bytes)?;
    let previous: PreV0_8_0Job = bincode::deserialize(body)?;
    previous.into_current()
}

fn pre_v0_8_0_envelope_body(bytes: &[u8]) -> Result<&[u8]> {
    let header_len = JOB_PAYLOAD_BLOB_MAGIC.len() + std::mem::size_of::<u16>();
    if bytes.len() < header_len || &bytes[..JOB_PAYLOAD_BLOB_MAGIC.len()] != JOB_PAYLOAD_BLOB_MAGIC
    {
        anyhow::bail!("job payload blob is not a pre-v0.8.0 encoded job payload");
    }
    let version_offset = JOB_PAYLOAD_BLOB_MAGIC.len();
    let version = u16::from_le_bytes([bytes[version_offset], bytes[version_offset + 1]]);
    if version != PRE_V0_8_0_JOB_PAYLOAD_BLOB_VERSION {
        anyhow::bail!(
            "unsupported pre-v0.8.0 job payload blob version {version}; expected {PRE_V0_8_0_JOB_PAYLOAD_BLOB_VERSION}"
        );
    }
    Ok(&bytes[header_len..])
}

impl PreV0_8_0Job {
    fn into_current(self) -> Result<Job> {
        Ok(Job {
            id: self.id,
            kind: self.kind,
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
            metadata: self.metadata.into_current()?,
        })
    }
}

impl PreV0_8_0JobMetadata {
    fn into_current(self) -> Result<JobMetadata> {
        let mut metadata = JobMetadata {
            error: self.error,
            timed_out_at: self.timed_out_at,
            cancel_requested: self.cancel_requested,
            cancelled_by_user_id: self.cancelled_by_user_id,
            output: self
                .output
                .map(PreV0_8_0JobOutput::into_current)
                .transpose()?,
            ..JobMetadata::default()
        };
        match self.detail.map(|detail| *detail) {
            Some(PreV0_8_0JobMetadataDetail::AgentTask(task)) => {
                metadata.set_agent_task(task);
            }
            Some(PreV0_8_0JobMetadataDetail::Confirmation(confirmation)) => {
                let current = metadata.confirmation_mut();
                current.delivery = confirmation.delivery;
                current.channel_id = confirmation.channel_id;
                current.message_id = confirmation.message_id;
                current.post_error = confirmation.post_error;
                current.approved_by_user_id = confirmation.approved_by_user_id;
                current.approved_at = confirmation.approved_at;
                current.approval_error = confirmation.approval_error;
            }
            None => {}
        }
        Ok(metadata)
    }
}

impl PreV0_8_0DiscordVoiceLeaveOutput {
    fn into_current(self) -> Result<DiscordVoiceLeaveOutput> {
        Ok(DiscordVoiceLeaveOutput {
            session_id: self.session_id,
            status: self.status,
            session: self.session,
            bot_status: self.bot_status,
            guild_id: self.guild_id,
            voice_channel_id: self.voice_channel_id,
            capture_run_id: self.capture_run_id,
            audio_jobs: self
                .audio_jobs
                .into_iter()
                .map(PreV0_8_0Job::into_current)
                .collect::<Result<Vec<_>>>()?,
        })
    }
}

impl PreV0_8_0JobOutput {
    fn into_current(self) -> Result<JobOutput> {
        Ok(match self {
            Self::Empty => JobOutput::Empty,
            Self::JobCreated(output) => JobOutput::JobCreated(output),
            Self::RuntimeControl(output) => JobOutput::RuntimeControl(output),
            Self::TextDelivery(output) => JobOutput::TextDelivery(output),
            Self::DiscordTextSend(output) => JobOutput::DiscordTextSend(output),
            Self::DiscordForumThreadCreate(output) => JobOutput::DiscordForumThreadCreate(output),
            Self::DiscordForumThreadRename(output) => JobOutput::DiscordForumThreadRename(output),
            Self::AgentSessionStart(output) => JobOutput::AgentSessionStart(output),
            Self::TranscriptPublication(output) => JobOutput::TranscriptPublication(output),
            Self::RoomAgentPlacement(output) => JobOutput::RoomAgentPlacement(output),
            Self::DiscordVoiceJoin(output) => JobOutput::DiscordVoiceJoin(output),
            Self::DiscordVoiceLeave(output) => JobOutput::DiscordVoiceLeave(output.into_current()?),
            Self::DiscordVoicePlayback(output) => JobOutput::DiscordVoicePlayback(output),
            Self::DiscordVoiceMute(output) => JobOutput::DiscordVoiceMute(output),
            Self::DiscordVoicePlayAudio(output) => JobOutput::DiscordVoicePlayAudio(output),
            Self::DiscordVoiceStatusSnapshot(output) => {
                JobOutput::DiscordVoiceStatusSnapshot(output)
            }
            Self::Record(output) => JobOutput::Record(output),
            Self::DiscordVoiceDeafen(output) => JobOutput::DiscordVoiceDeafen(output),
            Self::DiscordTypingIndicator(output) => JobOutput::DiscordTypingIndicator(output),
        })
    }
}

impl PreV0_8_0JobPayload {
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
            Self::TranscriptPublication(payload) => {
                JobPayload::TranscriptPublication(TranscriptPublicationPayload {
                    publication_id: payload.publication_id,
                    live: payload.live,
                })
            }
            Self::RefineTranscript(_) => {
                anyhow::bail!("pre-v0.8.0 refine_transcript payload survived deletion")
            }
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
            Self::StaleRunningJobSweep(payload) => JobPayload::StaleRunningJobSweep(payload),
            Self::EphemeralJobGc(payload) => JobPayload::EphemeralJobGc(payload),
            Self::DiscordVoiceDeafen(payload) => JobPayload::DiscordVoiceDeafen(payload),
            Self::DiscordTypingIndicator(payload) => JobPayload::DiscordTypingIndicator(payload),
        })
    }
}
