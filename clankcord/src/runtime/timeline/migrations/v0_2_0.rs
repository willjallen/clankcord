use serde::{Deserialize, Serialize};

use crate::Result;
use crate::runtime::jobs::JobMetadata;
use crate::runtime::timeline::store::upsert_job_rows;
use crate::runtime::{
    BinaryPayload, DiscordSlashCommandPayload, Job, JobKind, JobPayload, JobState,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum PreV0_2_0JobState {
    Queued,
    Running,
    Waiting,
    Complete,
    Cancelled,
    CancelRequested,
    ConfirmationPending,
    Approved,
    ApprovalFailed,
    Failed,
    FailedTimeout,
    AgentDispatchFailed,
    FailedDraftRetained,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PreV0_2_0Job<State, Payload> {
    id: String,
    kind: JobKind,
    guild_id: String,
    voice_channel_id: String,
    state: State,
    requested_by_user_id: String,
    payload: Payload,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
enum PreV0_2_0SlashPayloadNoVoiceChannel {
    AudioSegment,
    WakeActivation,
    AgentTask,
    DiscordTextMessage,
    DiscordSlashCommand(PreV0_2_0DiscordSlashCommandPayloadNoVoiceChannel),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PreV0_2_0DiscordSlashCommandPayloadNoVoiceChannel {
    interaction_id: String,
    interaction_token: String,
    application_id: String,
    guild_id: String,
    channel_id: String,
    user_id: String,
    username: String,
    command_name: String,
    options: BinaryPayload,
    created_at: String,
    response_visibility: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
enum PreV0_2_0SlashPayloadNoInteractionId {
    AudioSegment,
    WakeActivation,
    AgentTask,
    DiscordTextMessage,
    DiscordSlashCommand(PreV0_2_0DiscordSlashCommandPayloadNoInteractionId),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PreV0_2_0DiscordSlashCommandPayloadNoInteractionId {
    interaction_token: String,
    application_id: String,
    guild_id: String,
    channel_id: String,
    user_id: String,
    username: String,
    command_name: String,
    options: BinaryPayload,
    created_at: String,
    response_visibility: String,
}

pub(super) async fn run(transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>) -> Result<()> {
    let rows = sqlx::query(
        r#"
        SELECT j.job_id, j.kind, j.state, j.voice_channel_id, p.payload_blob
        FROM jobs j
        JOIN job_payloads p ON p.job_id = j.job_id
        ORDER BY j.job_id
        "#,
    )
    .fetch_all(transaction.as_mut())
    .await?;
    for row in rows {
        let job_id: String = sqlx::Row::try_get(&row, "job_id")?;
        let kind = sqlx::Row::try_get::<String, _>(&row, "kind")?.parse::<JobKind>()?;
        let state: String = sqlx::Row::try_get(&row, "state")?;
        let voice_channel_id: String = sqlx::Row::try_get(&row, "voice_channel_id")?;
        let payload_blob: Vec<u8> = sqlx::Row::try_get(&row, "payload_blob")?;
        if Job::is_current_payload_blob(&payload_blob) {
            if state == "agent_dispatch_failed" {
                let job = Job::decode(&payload_blob).map_err(|error| {
                    anyhow::anyhow!("rewriting pre-v0.2.0 job projection state {job_id}: {error:#}")
                })?;
                upsert_job_rows(transaction, &job).await?;
            }
            continue;
        }
        let job =
            decode_pre_v0_2_0_job_payload_blob(&payload_blob, kind, &state, &voice_channel_id)
                .map_err(|error| {
                    anyhow::anyhow!("migrating pre-v0.2.0 job payload blob {job_id}: {error:#}")
                })?;
        upsert_job_rows(transaction, &job).await?;
    }
    Ok(())
}

fn decode_pre_v0_2_0_job_payload_blob(
    bytes: &[u8],
    projected_kind: JobKind,
    projected_state: &str,
    projected_voice_channel_id: &str,
) -> Result<Job> {
    let state = pre_v0_2_0_payload_migration_state(projected_state)?;
    if let Ok(previous) = bincode::deserialize::<PreV0_2_0Job<PreV0_2_0JobState, JobPayload>>(bytes)
    {
        return previous.into_current_with(state, |payload, _job_id| Ok(payload));
    }
    if projected_kind == JobKind::DiscordSlashCommand {
        if let Ok(previous) = bincode::deserialize::<
            PreV0_2_0Job<PreV0_2_0JobState, PreV0_2_0SlashPayloadNoVoiceChannel>,
        >(bytes)
        {
            return previous.into_current_with(state, |payload, _job_id| match payload {
                PreV0_2_0SlashPayloadNoVoiceChannel::DiscordSlashCommand(payload) => Ok(
                    JobPayload::DiscordSlashCommand(DiscordSlashCommandPayload {
                        interaction_id: payload.interaction_id,
                        interaction_token: payload.interaction_token,
                        application_id: payload.application_id,
                        guild_id: payload.guild_id,
                        channel_id: payload.channel_id,
                        voice_channel_id: projected_voice_channel_id.to_string(),
                        user_id: payload.user_id,
                        username: payload.username,
                        command_name: payload.command_name,
                        options: payload.options,
                        created_at: payload.created_at,
                        response_visibility: payload.response_visibility,
                    }),
                ),
                _ => anyhow::bail!("pre-v0.2.0 slash-command job payload kind mismatch"),
            });
        }
        if let Ok(previous) = bincode::deserialize::<
            PreV0_2_0Job<PreV0_2_0JobState, PreV0_2_0SlashPayloadNoInteractionId>,
        >(bytes)
        {
            return previous.into_current_with(state, |payload, job_id| match payload {
                PreV0_2_0SlashPayloadNoInteractionId::DiscordSlashCommand(payload) => Ok(
                    JobPayload::DiscordSlashCommand(DiscordSlashCommandPayload {
                        interaction_id: job_id.to_string(),
                        interaction_token: payload.interaction_token,
                        application_id: payload.application_id,
                        guild_id: payload.guild_id,
                        channel_id: payload.channel_id,
                        voice_channel_id: projected_voice_channel_id.to_string(),
                        user_id: payload.user_id,
                        username: payload.username,
                        command_name: payload.command_name,
                        options: payload.options,
                        created_at: payload.created_at,
                        response_visibility: payload.response_visibility,
                    }),
                ),
                _ => anyhow::bail!("pre-v0.2.0 slash-command job payload kind mismatch"),
            });
        }
    }
    Ok(bincode::deserialize(bytes)?)
}

fn pre_v0_2_0_payload_migration_state(raw: &str) -> Result<JobState> {
    match raw {
        "agent_dispatch_failed" => Ok(JobState::Failed),
        value => value.parse(),
    }
}

impl<State, Payload> PreV0_2_0Job<State, Payload> {
    fn into_current_with(
        self,
        state: JobState,
        payload: impl FnOnce(Payload, &str) -> Result<JobPayload>,
    ) -> Result<Job> {
        let payload = payload(self.payload, &self.id)?;
        Ok(Job {
            id: self.id,
            kind: payload.kind(),
            guild_id: self.guild_id,
            voice_channel_id: self.voice_channel_id,
            state,
            requested_by_user_id: self.requested_by_user_id,
            payload,
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
            metadata: self.metadata,
        })
    }
}
