//! Automation persistence: the automations table rows and their
//! CLANKAUT payload blobs. Spec/record semantics live in
//! domain/automations; this file only stores and retrieves them.

use super::*;

use serde_json::json;

use crate::domain::automations::{
    AutomationOwner, AutomationRecord, AutomationSpec, AutomationState,
};
use crate::model::job::JobKind;

impl TimelineStore {
    pub async fn create_automation(&self, spec: AutomationSpec) -> Result<AutomationRecord> {
        spec.validate()?;
        let mut transaction = self.pool.begin().await?;
        if let Some(existing) =
            find_active_automation_by_idempotency_key_in_tx(&mut transaction, &spec).await?
        {
            transaction.commit().await?;
            return Ok(existing);
        }
        if let Some(source_job_id) = agent_owner_source_job_id(&spec.owner) {
            lock_agent_automation_source(&mut transaction, source_job_id).await?;
            if let Some(existing) =
                find_active_agent_automation_by_source_in_tx(&mut transaction, &spec).await?
            {
                transaction.commit().await?;
                return Ok(existing);
            }
        }
        let record = AutomationRecord::new(spec);
        upsert_automation_record_in_tx(&mut transaction, &record).await?;
        transaction.commit().await?;
        self.append_event(
            &record.spec.scope.guild_id,
            &record.spec.scope.scope_id,
            json!({
                "event_kind": "automation_created",
                "kind": "automation_created",
                "automation_id": record.automation_id,
                "name": record.spec.name,
            }),
        )
        .await?;
        Ok(record)
    }

    pub(crate) async fn save_automation_record(&self, record: &AutomationRecord) -> Result<()> {
        self.upsert_automation_record(record).await
    }

    pub async fn get_automation(&self, automation_id: &str) -> Result<AutomationRecord> {
        let row = sqlx::query("SELECT payload_blob FROM automations WHERE automation_id = $1")
            .bind(automation_id)
            .fetch_one(&self.pool)
            .await?;
        let payload: Vec<u8> = row.try_get("payload_blob")?;
        AutomationRecord::decode(&payload)
    }

    pub async fn list_automations(
        &self,
        guild_id: Option<&str>,
        scope_id: Option<&str>,
        state: Option<AutomationState>,
    ) -> Result<Vec<AutomationRecord>> {
        let records = self
            .automation_rows()
            .await?
            .into_iter()
            .filter(|record| {
                guild_id
                    .filter(|value| !value.trim().is_empty())
                    .is_none_or(|value| record.spec.scope.guild_id == value)
            })
            .filter(|record| {
                scope_id
                    .filter(|value| !value.trim().is_empty())
                    .is_none_or(|value| record.spec.scope.scope_id == value)
            })
            .filter(|record| state.is_none_or(|value| record.state == value))
            .collect::<Vec<_>>();
        Ok(records)
    }

    pub async fn cancel_automation(&self, automation_id: &str) -> Result<AutomationRecord> {
        let mut record = self.get_automation(automation_id).await?;
        record.state = AutomationState::Cancelled;
        record.updated_at = isoformat_z(None);
        self.upsert_automation_record(&record).await?;
        self.append_event(
            &record.spec.scope.guild_id,
            &record.spec.scope.scope_id,
            json!({
                "event_kind": "automation_cancelled",
                "kind": "automation_cancelled",
                "automation_id": record.automation_id,
                "name": record.spec.name,
            }),
        )
        .await?;
        Ok(record)
    }

    async fn upsert_automation_record(&self, record: &AutomationRecord) -> Result<()> {
        let mut transaction = self.pool.begin().await?;
        upsert_automation_record_in_tx(&mut transaction, record).await?;
        transaction.commit().await?;
        Ok(())
    }

    async fn automation_rows(&self) -> Result<Vec<AutomationRecord>> {
        let rows = sqlx::query("SELECT payload_blob FROM automations ORDER BY created_at_ms DESC")
            .fetch_all(&self.pool)
            .await?;
        let mut records = Vec::new();
        for row in rows {
            let payload: Vec<u8> = row.try_get("payload_blob")?;
            records.push(AutomationRecord::decode(&payload)?);
        }
        Ok(records)
    }
}

async fn find_active_automation_by_idempotency_key_in_tx(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    spec: &AutomationSpec,
) -> Result<Option<AutomationRecord>> {
    if spec.idempotency_key.trim().is_empty() {
        return Ok(None);
    }
    let row = sqlx::query(
        "SELECT payload_blob FROM automations WHERE scope_kind = $1 AND scope_id = $2 AND idempotency_key = $3 AND state = 'active' ORDER BY created_at_ms DESC LIMIT 1",
    )
    .bind(&spec.scope.scope_kind)
    .bind(&spec.scope.scope_id)
    .bind(&spec.idempotency_key)
    .fetch_optional(transaction.as_mut())
    .await?;
    row.map(|row| -> Result<AutomationRecord> {
        let payload: Vec<u8> = row.try_get("payload_blob")?;
        AutomationRecord::decode(&payload)
    })
    .transpose()
}

async fn lock_agent_automation_source(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    source_job_id: &str,
) -> Result<()> {
    let row = sqlx::query("SELECT kind FROM jobs WHERE job_id = $1 FOR UPDATE")
        .bind(source_job_id)
        .fetch_optional(transaction.as_mut())
        .await?;
    let Some(row) = row else {
        anyhow::bail!("agent-owned automation source job does not exist: {source_job_id}");
    };
    let kind: String = row.try_get("kind")?;
    if kind != JobKind::AgentTask.as_str() {
        anyhow::bail!(
            "agent-owned automation source job {source_job_id} is {kind}, not agent_task"
        );
    }
    Ok(())
}

async fn find_active_agent_automation_by_source_in_tx(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    spec: &AutomationSpec,
) -> Result<Option<AutomationRecord>> {
    let Some(source_job_id) = agent_owner_source_job_id(&spec.owner) else {
        return Ok(None);
    };
    let rows = sqlx::query(
        r#"
        SELECT payload_blob
        FROM automations
        WHERE scope_kind = $1
          AND scope_id = $2
          AND state = 'active'
        ORDER BY created_at_ms, automation_id
        "#,
    )
    .bind(&spec.scope.scope_kind)
    .bind(&spec.scope.scope_id)
    .fetch_all(transaction.as_mut())
    .await?;
    for row in rows {
        let payload: Vec<u8> = row.try_get("payload_blob")?;
        let record = AutomationRecord::decode(&payload)?;
        if agent_owner_source_job_id(&record.spec.owner) == Some(source_job_id) {
            return Ok(Some(record));
        }
    }
    Ok(None)
}

fn agent_owner_source_job_id(owner: &AutomationOwner) -> Option<&str> {
    match owner {
        AutomationOwner::Agent { source_job_id, .. } => Some(source_job_id.as_str()),
        AutomationOwner::User { .. } | AutomationOwner::System => None,
    }
    .filter(|source_job_id| !source_job_id.trim().is_empty())
}

async fn upsert_automation_record_in_tx(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    record: &AutomationRecord,
) -> Result<()> {
    let created_ms = instant_ms_str(Some(&record.created_at)).unwrap_or(0);
    let updated_ms = instant_ms_str(Some(&record.updated_at)).unwrap_or(created_ms);
    let expires_at_ms = record.spec.expiry.expires_at.as_deref().and_then(|value| {
        parse_instant(value)
            .and_then(|expires_at| instant_ms_str(Some(&isoformat_z(Some(expires_at)))))
    });
    sqlx::query(
        r#"
            INSERT INTO automations(
              automation_id,
              scope_kind,
              guild_id,
              scope_id,
              state,
              idempotency_key,
              created_at_ms,
              updated_at_ms,
              expires_at_ms,
              fire_count,
              max_fires,
              payload_blob
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
            ON CONFLICT(automation_id) DO UPDATE SET
              scope_kind = EXCLUDED.scope_kind,
              guild_id = EXCLUDED.guild_id,
              scope_id = EXCLUDED.scope_id,
              state = EXCLUDED.state,
              idempotency_key = EXCLUDED.idempotency_key,
              updated_at_ms = EXCLUDED.updated_at_ms,
              expires_at_ms = EXCLUDED.expires_at_ms,
              fire_count = EXCLUDED.fire_count,
              max_fires = EXCLUDED.max_fires,
              payload_blob = EXCLUDED.payload_blob
            "#,
    )
    .bind(&record.automation_id)
    .bind(&record.spec.scope.scope_kind)
    .bind(&record.spec.scope.guild_id)
    .bind(&record.spec.scope.scope_id)
    .bind(record.state.as_str())
    .bind(&record.spec.idempotency_key)
    .bind(created_ms)
    .bind(updated_ms)
    .bind(expires_at_ms)
    .bind(record.fire_count as i64)
    .bind(record.spec.expiry.max_fires.map(|value| value as i64))
    .bind(record.encode()?)
    .execute(transaction.as_mut())
    .await?;
    Ok(())
}
