use serde::{Deserialize, Serialize};

use crate::Result;
use crate::runtime::automations::{
    AutomationAction, AutomationCondition, AutomationDelay, AutomationExpiry, AutomationOwner,
    AutomationPendingRecheck, AutomationRecord, AutomationSpec, AutomationState, AutomationTrigger,
};
use crate::runtime::jobs::JobMetadata;
use crate::runtime::{
    AgentSessionRecord, AgentSessionRouteKind, Job, JobKind, JobPayload, JobState, RuntimeScope,
    RuntimeScopeKind, dm_route_key, thread_route_key, voice_route_key,
};

const JOB_PAYLOAD_BLOB_MAGIC: &[u8; 8] = b"CLANKJOB";
const PRE_V0_3_0_JOB_PAYLOAD_BLOB_VERSION: u16 = 2;

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PreV0_3_0AutomationScope {
    guild_id: String,
    voice_channel_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct PreV0_3_0AutomationSpec {
    schema: String,
    name: String,
    idempotency_key: String,
    owner: AutomationOwner,
    scope: PreV0_3_0AutomationScope,
    trigger: AutomationTrigger,
    condition: AutomationCondition,
    delay: Option<AutomationDelay>,
    expiry: AutomationExpiry,
    actions: Vec<AutomationAction>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct PreV0_3_0AutomationRecord {
    automation_id: String,
    state: AutomationState,
    created_at: String,
    updated_at: String,
    last_evaluated_at: String,
    last_fired_at: String,
    fire_count: u64,
    pending_recheck: Option<AutomationPendingRecheck>,
    spec: PreV0_3_0AutomationSpec,
}

pub(super) async fn run(transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>) -> Result<()> {
    migrate_jobs(transaction).await?;
    migrate_agent_sessions(transaction).await?;
    migrate_voice_scoped_projection_table(transaction, "timeline_events").await?;
    migrate_voice_scoped_projection_table(transaction, "conversations").await?;
    migrate_voice_scoped_projection_table(transaction, "windows").await?;
    migrate_voice_scoped_projection_table(transaction, "publications").await?;
    migrate_voice_scoped_projection_table(transaction, "authoritative_spans").await?;
    migrate_automations(transaction).await?;
    Ok(())
}

async fn migrate_jobs(transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>) -> Result<()> {
    let has_legacy_column = column_exists(transaction, "jobs", "voice_channel_id").await?;
    sqlx::query(
        r#"
        ALTER TABLE jobs
          ADD COLUMN IF NOT EXISTS scope_kind TEXT NOT NULL DEFAULT 'voice_channel',
          ADD COLUMN IF NOT EXISTS scope_id TEXT NOT NULL DEFAULT ''
        "#,
    )
    .execute(transaction.as_mut())
    .await?;
    if !has_legacy_column {
        return Ok(());
    }
    sqlx::query(
        r#"
        UPDATE jobs
        SET scope_kind = 'voice_channel',
            scope_id = voice_channel_id
        WHERE scope_id = ''
        "#,
    )
    .execute(transaction.as_mut())
    .await?;
    let rows = sqlx::query(
        r#"
        SELECT j.job_id, j.voice_channel_id, p.payload_blob
        FROM jobs j
        JOIN job_payloads p ON p.job_id = j.job_id
        ORDER BY j.job_id
        "#,
    )
    .fetch_all(transaction.as_mut())
    .await?;
    for row in rows {
        let job_id: String = sqlx::Row::try_get(&row, "job_id")?;
        let voice_channel_id: String = sqlx::Row::try_get(&row, "voice_channel_id")?;
        let payload_blob: Vec<u8> = sqlx::Row::try_get(&row, "payload_blob")?;
        let job = decode_job_payload_for_scope_migration(&payload_blob, &voice_channel_id)
            .map_err(|error| anyhow::anyhow!("migrating v0.3.0 job {job_id}: {error:#}"))?;
        sqlx::query(
            r#"
            UPDATE jobs
            SET scope_kind = $2,
                guild_id = $3,
                scope_id = $4,
                kind = $5
            WHERE job_id = $1
            "#,
        )
        .bind(&job.id)
        .bind(job.scope_kind.as_str())
        .bind(&job.guild_id)
        .bind(&job.scope_id)
        .bind(job.kind.as_str())
        .execute(transaction.as_mut())
        .await?;
        sqlx::query("UPDATE job_payloads SET payload_blob = $2 WHERE job_id = $1")
            .bind(&job.id)
            .bind(job.encode()?)
            .execute(transaction.as_mut())
            .await?;
    }
    sqlx::query("ALTER TABLE jobs DROP COLUMN IF EXISTS voice_channel_id")
        .execute(transaction.as_mut())
        .await?;
    Ok(())
}

async fn migrate_agent_sessions(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<()> {
    let has_legacy_column =
        column_exists(transaction, "agent_sessions", "voice_channel_id").await?;
    sqlx::query(
        r#"
        ALTER TABLE agent_sessions
          ADD COLUMN IF NOT EXISTS scope_id TEXT NOT NULL DEFAULT ''
        "#,
    )
    .execute(transaction.as_mut())
    .await?;
    if !has_legacy_column {
        return Ok(());
    }
    sqlx::query(
        r#"
        UPDATE agent_sessions
        SET scope_id = voice_channel_id
        WHERE scope_id = ''
        "#,
    )
    .execute(transaction.as_mut())
    .await?;
    let rows = sqlx::query(
        r#"
        SELECT agent_session_id, voice_channel_id, payload_blob
        FROM agent_sessions
        ORDER BY agent_session_id
        "#,
    )
    .fetch_all(transaction.as_mut())
    .await?;
    for row in rows {
        let agent_session_id: String = sqlx::Row::try_get(&row, "agent_session_id")?;
        let legacy_scope_id: String = sqlx::Row::try_get(&row, "voice_channel_id")?;
        let payload_blob: Vec<u8> = sqlx::Row::try_get(&row, "payload_blob")?;
        let mut record: AgentSessionRecord =
            bincode::deserialize(&payload_blob).map_err(|error| {
                anyhow::anyhow!("migrating agent session {agent_session_id}: {error:#}")
            })?;
        if record.scope_id.trim().is_empty() {
            record.scope_id = legacy_scope_id;
        }
        match record.route_kind {
            AgentSessionRouteKind::Dm => {
                if record.dm_user_id.trim().is_empty() {
                    record.dm_user_id = record.scope_id.clone();
                }
                record.guild_id.clear();
                record.scope_id = record.dm_user_id.clone();
                record.route_key = dm_route_key(&record.dm_user_id);
            }
            AgentSessionRouteKind::Voice => {
                record.route_key = voice_route_key(&record.guild_id, &record.scope_id);
            }
            AgentSessionRouteKind::Thread => {
                if record.scope_id.trim().is_empty() {
                    record.scope_id = record.discord_thread_id.clone();
                }
                record.route_key = thread_route_key(&record.guild_id, &record.discord_thread_id);
            }
        }
        sqlx::query(
            r#"
            UPDATE agent_sessions
            SET guild_id = $2,
                scope_id = $3,
                dm_user_id = $4,
                route_key = $5,
                payload_blob = $6
            WHERE agent_session_id = $1
            "#,
        )
        .bind(&record.agent_session_id)
        .bind(&record.guild_id)
        .bind(&record.scope_id)
        .bind(&record.dm_user_id)
        .bind(&record.route_key)
        .bind(bincode::serialize(&record)?)
        .execute(transaction.as_mut())
        .await?;
    }
    sqlx::query("ALTER TABLE agent_sessions DROP COLUMN IF EXISTS voice_channel_id")
        .execute(transaction.as_mut())
        .await?;
    Ok(())
}

async fn migrate_voice_scoped_projection_table(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    table: &str,
) -> Result<()> {
    let has_legacy_column = column_exists(transaction, table, "voice_channel_id").await?;
    let quoted_table = quote_identifier(table);
    sqlx::query(&format!(
        r#"
        ALTER TABLE {quoted_table}
          ADD COLUMN IF NOT EXISTS scope_kind TEXT NOT NULL DEFAULT 'voice_channel',
          ADD COLUMN IF NOT EXISTS scope_id TEXT NOT NULL DEFAULT ''
        "#
    ))
    .execute(transaction.as_mut())
    .await?;
    if !has_legacy_column {
        return Ok(());
    }
    sqlx::query(&format!(
        r#"
        UPDATE {quoted_table}
        SET scope_kind = 'voice_channel',
            scope_id = voice_channel_id
        WHERE scope_id = ''
        "#
    ))
    .execute(transaction.as_mut())
    .await?;
    sqlx::query(&format!(
        "ALTER TABLE {quoted_table} DROP COLUMN IF EXISTS voice_channel_id"
    ))
    .execute(transaction.as_mut())
    .await?;
    Ok(())
}

async fn migrate_automations(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<()> {
    let has_legacy_column = column_exists(transaction, "automations", "voice_channel_id").await?;
    sqlx::query(
        r#"
        ALTER TABLE automations
          ADD COLUMN IF NOT EXISTS scope_kind TEXT NOT NULL DEFAULT 'voice_channel',
          ADD COLUMN IF NOT EXISTS scope_id TEXT NOT NULL DEFAULT ''
        "#,
    )
    .execute(transaction.as_mut())
    .await?;
    if !has_legacy_column {
        return Ok(());
    }
    sqlx::query(
        r#"
        UPDATE automations
        SET scope_kind = 'voice_channel',
            scope_id = voice_channel_id
        WHERE scope_id = ''
        "#,
    )
    .execute(transaction.as_mut())
    .await?;
    let rows = sqlx::query(
        r#"
        SELECT automation_id, voice_channel_id, payload_blob
        FROM automations
        ORDER BY automation_id
        "#,
    )
    .fetch_all(transaction.as_mut())
    .await?;
    for row in rows {
        let automation_id: String = sqlx::Row::try_get(&row, "automation_id")?;
        let legacy_scope_id: String = sqlx::Row::try_get(&row, "voice_channel_id")?;
        let payload_blob: Vec<u8> = sqlx::Row::try_get(&row, "payload_blob")?;
        let record = decode_automation_for_scope_migration(&payload_blob, &legacy_scope_id)
            .map_err(|error| {
                anyhow::anyhow!("migrating v0.3.0 automation {automation_id}: {error:#}")
            })?;
        sqlx::query(
            r#"
            UPDATE automations
            SET scope_kind = $2,
                guild_id = $3,
                scope_id = $4,
                idempotency_key = $5,
                payload_blob = $6
            WHERE automation_id = $1
            "#,
        )
        .bind(&record.automation_id)
        .bind(&record.spec.scope.scope_kind)
        .bind(&record.spec.scope.guild_id)
        .bind(&record.spec.scope.scope_id)
        .bind(&record.spec.idempotency_key)
        .bind(bincode::serialize(&record)?)
        .execute(transaction.as_mut())
        .await?;
    }
    sqlx::query("ALTER TABLE automations DROP COLUMN IF EXISTS voice_channel_id")
        .execute(transaction.as_mut())
        .await?;
    Ok(())
}

async fn column_exists(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    table: &str,
    column: &str,
) -> Result<bool> {
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
    .fetch_one(transaction.as_mut())
    .await?;
    Ok(sqlx::Row::try_get(&row, "exists")?)
}

fn decode_job_payload_for_scope_migration(bytes: &[u8], legacy_scope_id: &str) -> Result<Job> {
    if let Ok(job) = Job::decode(bytes) {
        return Ok(job);
    }
    if let Some(body) = pre_v0_3_0_envelope_body(bytes) {
        let previous: PreV0_3_0Job = bincode::deserialize(body)?;
        return Ok(previous.into_current(legacy_scope_id));
    }
    let previous: PreV0_3_0Job = bincode::deserialize(bytes)?;
    Ok(previous.into_current(legacy_scope_id))
}

fn pre_v0_3_0_envelope_body(bytes: &[u8]) -> Option<&[u8]> {
    let header_len = JOB_PAYLOAD_BLOB_MAGIC.len() + std::mem::size_of::<u16>();
    if bytes.len() < header_len || &bytes[..JOB_PAYLOAD_BLOB_MAGIC.len()] != JOB_PAYLOAD_BLOB_MAGIC
    {
        return None;
    }
    let version_offset = JOB_PAYLOAD_BLOB_MAGIC.len();
    let version = u16::from_le_bytes([bytes[version_offset], bytes[version_offset + 1]]);
    if version != PRE_V0_3_0_JOB_PAYLOAD_BLOB_VERSION {
        return None;
    }
    Some(&bytes[header_len..])
}

fn decode_automation_for_scope_migration(
    bytes: &[u8],
    legacy_scope_id: &str,
) -> Result<AutomationRecord> {
    let previous: PreV0_3_0AutomationRecord = bincode::deserialize(bytes)?;
    Ok(previous.into_current(legacy_scope_id))
}

impl PreV0_3_0Job {
    fn into_current(self, legacy_scope_id: &str) -> Job {
        let scope = legacy_scope_for_job(&self.guild_id, &self.voice_channel_id, &self.payload);
        Job {
            id: self.id,
            kind: self.payload.kind(),
            scope_kind: scope.kind,
            guild_id: scope.guild_id,
            scope_id: if scope.scope_id.is_empty() {
                legacy_scope_id.to_string()
            } else {
                scope.scope_id
            },
            state: self.state,
            requested_by_user_id: self.requested_by_user_id,
            payload: self.payload,
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
        }
    }
}

impl PreV0_3_0AutomationRecord {
    fn into_current(self, legacy_scope_id: &str) -> AutomationRecord {
        AutomationRecord {
            automation_id: self.automation_id,
            state: self.state,
            created_at: self.created_at,
            updated_at: self.updated_at,
            last_evaluated_at: self.last_evaluated_at,
            last_fired_at: self.last_fired_at,
            fire_count: self.fire_count,
            pending_recheck: self.pending_recheck,
            spec: self.spec.into_current(legacy_scope_id),
        }
    }
}

impl PreV0_3_0AutomationSpec {
    fn into_current(self, legacy_scope_id: &str) -> AutomationSpec {
        AutomationSpec {
            schema: self.schema,
            name: self.name,
            idempotency_key: self.idempotency_key,
            owner: self.owner,
            scope: crate::runtime::automations::AutomationScope {
                scope_kind: RuntimeScopeKind::VoiceChannel.as_str().to_string(),
                guild_id: self.scope.guild_id,
                scope_id: if self.scope.voice_channel_id.is_empty() {
                    legacy_scope_id.to_string()
                } else {
                    self.scope.voice_channel_id
                },
            },
            trigger: self.trigger,
            condition: self.condition,
            delay: self.delay,
            expiry: self.expiry,
            actions: self.actions,
        }
    }
}

fn legacy_scope_for_job(
    guild_id: &str,
    legacy_scope_id: &str,
    payload: &JobPayload,
) -> RuntimeScope {
    match payload {
        JobPayload::DiscordTextMessage(payload) if payload.guild_id.trim().is_empty() => {
            RuntimeScope::dm(payload.author_user_id.clone())
        }
        JobPayload::DiscordTextMessage(payload) => {
            RuntimeScope::text_channel(payload.guild_id.clone(), payload.channel_id.clone())
        }
        JobPayload::DiscordSlashCommand(payload) if payload.guild_id.trim().is_empty() => {
            RuntimeScope::dm(payload.user_id.clone())
        }
        JobPayload::DiscordSlashCommand(payload)
            if payload.timeline_channel_id() == payload.voice_channel_id
                && !payload.voice_channel_id.trim().is_empty() =>
        {
            RuntimeScope::voice_channel(payload.guild_id.clone(), payload.voice_channel_id.clone())
        }
        JobPayload::DiscordSlashCommand(payload) => {
            RuntimeScope::text_channel(payload.guild_id.clone(), payload.channel_id.clone())
        }
        JobPayload::RuntimeMaintenance(_)
        | JobPayload::VoiceStatusSync(_)
        | JobPayload::DiscordVoiceStatusSnapshot(_)
        | JobPayload::AutomationEvaluation(_)
        | JobPayload::StaleWakeProbeSweep(_)
        | JobPayload::StaleRunningJobSweep(_)
        | JobPayload::EphemeralJobGc(_)
        | JobPayload::AgentSessionSunset(_)
        | JobPayload::AgentSessionRetirement(_) => RuntimeScope::runtime(),
        JobPayload::AgentSessionResume(payload) if payload.route_kind == "dm" => {
            RuntimeScope::dm(payload.dm_user_id.clone())
        }
        _ if guild_id.trim().is_empty() || guild_id == "dm" => {
            RuntimeScope::dm(legacy_scope_id.to_string())
        }
        _ => RuntimeScope {
            kind: RuntimeScopeKind::VoiceChannel,
            guild_id: guild_id.to_string(),
            scope_id: legacy_scope_id.to_string(),
        },
    }
}

fn quote_identifier(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}
