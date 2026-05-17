use crate::Result;
use crate::runtime::AgentSessionRecord;
use crate::runtime::automations::AutomationRecord;

pub(super) async fn run(transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>) -> Result<()> {
    assert_timeline_event_time_contract(transaction).await?;
    sqlx::raw_sql(
        r#"
        ALTER TABLE timeline_events
          ALTER COLUMN started_at_ms SET NOT NULL,
          ALTER COLUMN ended_at_ms SET NOT NULL
        "#,
    )
    .execute(transaction.as_mut())
    .await?;
    assert_agent_session_payload_envelopes(transaction).await?;
    assert_automation_payload_envelopes(transaction).await?;
    Ok(())
}

async fn assert_timeline_event_time_contract(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<()> {
    let row = sqlx::query(
        r#"
        SELECT event_id
        FROM timeline_events
        WHERE started_at_ms IS NULL OR ended_at_ms IS NULL
        ORDER BY sequence
        LIMIT 1
        "#,
    )
    .fetch_optional(transaction.as_mut())
    .await?;
    if let Some(row) = row {
        let event_id: String = sqlx::Row::try_get(&row, "event_id")?;
        anyhow::bail!(
            "timeline event {event_id} violates v0.4.0 timestamp contract: started_at_ms and ended_at_ms are required"
        );
    }
    Ok(())
}

async fn assert_agent_session_payload_envelopes(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<()> {
    let rows = sqlx::query(
        r#"
        SELECT agent_session_id, payload_blob
        FROM agent_sessions
        ORDER BY agent_session_id
        "#,
    )
    .fetch_all(transaction.as_mut())
    .await?;
    for row in rows {
        let agent_session_id: String = sqlx::Row::try_get(&row, "agent_session_id")?;
        let payload_blob: Vec<u8> = sqlx::Row::try_get(&row, "payload_blob")?;
        if !AgentSessionRecord::is_current_payload_blob(&payload_blob) {
            anyhow::bail!(
                "agent session {agent_session_id} violates v0.4.0 payload envelope contract"
            );
        }
    }
    Ok(())
}

async fn assert_automation_payload_envelopes(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<()> {
    let rows = sqlx::query(
        r#"
        SELECT automation_id, payload_blob
        FROM automations
        ORDER BY automation_id
        "#,
    )
    .fetch_all(transaction.as_mut())
    .await?;
    for row in rows {
        let automation_id: String = sqlx::Row::try_get(&row, "automation_id")?;
        let payload_blob: Vec<u8> = sqlx::Row::try_get(&row, "payload_blob")?;
        if !AutomationRecord::is_current_payload_blob(&payload_blob) {
            anyhow::bail!("automation {automation_id} violates v0.4.0 payload envelope contract");
        }
    }
    Ok(())
}
