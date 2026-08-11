//! Replay of the v0.3.0 automation scope/blob schema migration over legacy rows.

use serde::{Deserialize, Serialize};

use clankcord::model::automations::{
    AutomationAction, AutomationCondition, AutomationDelay, AutomationExpiry, AutomationOwner,
    AutomationPendingRecheck, AutomationRecord, AutomationState, AutomationTrigger,
};

use crate::support::automations::{insert_agent_source_job, reminder_spec};
use crate::support::test_store;

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
#[tokio::test(flavor = "current_thread")]
async fn v0_3_0_schema_migration_rewrites_legacy_automation_scope_projection_and_blob() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    insert_agent_source_job(&store).await;
    let record = store
        .create_automation(reminder_spec("job_1:migrate-automation"))
        .await
        .unwrap();
    let legacy_blob = bincode::serialize(&PreV0_3_0AutomationRecord::from_current(&record))
        .expect("legacy automation record serializes");

    sqlx::raw_sql(
        r#"
        ALTER TABLE automations ADD COLUMN voice_channel_id TEXT NOT NULL DEFAULT '';
        UPDATE automations SET voice_channel_id = scope_id;
        ALTER TABLE automations DROP COLUMN scope_kind CASCADE;
        ALTER TABLE automations DROP COLUMN scope_id CASCADE;
        "#,
    )
    .execute(&store.pool)
    .await
    .unwrap();
    sqlx::query("UPDATE automations SET payload_blob = $1 WHERE automation_id = $2")
        .bind(legacy_blob)
        .bind(&record.automation_id)
        .execute(&store.pool)
        .await
        .unwrap();
    sqlx::query(
        "DELETE FROM clankcord_schema_migrations WHERE version IN ('0.3.0', '0.4.0', '0.5.0', '0.6.0', '0.7.0', '0.8.0', '0.9.0', '0.10.0', '0.11.0', '0.12.0', '0.13.0', '1.0.0')",
    )
    .execute(&store.pool)
    .await
    .unwrap();

    let applied = store.run_pending_schema_migrations().await.unwrap();

    assert_eq!(applied.len(), 12);
    assert_eq!(applied[0].version, "0.3.0");
    assert_eq!(applied[1].version, "0.4.0");
    assert_eq!(applied[2].version, "0.5.0");
    assert_eq!(applied[3].version, "0.6.0");
    assert_eq!(applied[4].version, "0.7.0");
    assert_eq!(applied[5].version, "0.8.0");
    assert_eq!(applied[6].version, "0.9.0");
    assert_eq!(applied[7].version, "0.10.0");
    assert_eq!(applied[8].version, "0.11.0");
    assert_eq!(applied[9].version, "0.12.0");
    assert_eq!(applied[10].version, "0.13.0");
    assert_eq!(applied[11].version, "1.0.0");
    assert!(!column_exists(&store.pool, "automations", "voice_channel_id").await);
    let row = sqlx::query("SELECT scope_kind, scope_id FROM automations WHERE automation_id = $1")
        .bind(&record.automation_id)
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
    let migrated = store.get_automation(&record.automation_id).await.unwrap();
    assert_eq!(migrated.spec.scope.scope_kind, "voice_channel");
    assert_eq!(migrated.spec.scope.guild_id, "guild");
    assert_eq!(migrated.spec.scope.scope_id, "code");
}
impl PreV0_3_0AutomationRecord {
    fn from_current(record: &AutomationRecord) -> Self {
        Self {
            automation_id: record.automation_id.clone(),
            state: record.state,
            created_at: record.created_at.clone(),
            updated_at: record.updated_at.clone(),
            last_evaluated_at: record.last_evaluated_at.clone(),
            last_fired_at: record.last_fired_at.clone(),
            fire_count: record.fire_count,
            pending_recheck: record.pending_recheck.clone(),
            spec: PreV0_3_0AutomationSpec {
                schema: record.spec.schema.clone(),
                name: record.spec.name.clone(),
                idempotency_key: record.spec.idempotency_key.clone(),
                owner: record.spec.owner.clone(),
                scope: PreV0_3_0AutomationScope {
                    guild_id: record.spec.scope.guild_id.clone(),
                    voice_channel_id: record.spec.scope.scope_id.clone(),
                },
                trigger: record.spec.trigger.clone(),
                condition: record.spec.condition.clone(),
                delay: record.spec.delay.clone(),
                expiry: record.spec.expiry.clone(),
                actions: record.spec.actions.clone(),
            },
        }
    }
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
