//! Replay of the v0.x schema-migration chain over frozen legacy blobs and rows.

use chrono::{Duration, Utc};
use serde_json::json;

use clankcord::domain::Ctx;
use clankcord::model::job::{
    BinaryPayload, CommandRequest, DiscordVoiceStatusSnapshotOutput, Job, JobOutput, JobPayload,
    JobState, OpaqueValue, TextDeliveryPayload,
};
use clankcord::model::scope::{RuntimeScope, RuntimeScopeKind};

use crate::support::job_wire::{
    encode_current_agent_task, encode_job_with_blob_version, encode_pre_v0_3_0_job,
    encode_pre_v0_6_0_agent_task_job, encode_pre_v0_7_0_text_delivery_job,
    encode_pre_v0_10_0_voice_status_snapshot_job,
};
use crate::support::jobs::{create_audio_segment_slot, wake_activation_payload};
use crate::support::{initialize_test_config, test_store};

#[tokio::test(flavor = "current_thread")]
async fn timeline_initialize_records_registered_schema_migrations() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(&raw.path().join("voice")).await;

    let rows = sqlx::query(
        r#"
        SELECT version, name, clankcord_version
        FROM clankcord_schema_migrations
        ORDER BY
          split_part(version, '.', 1)::int,
          split_part(version, '.', 2)::int,
          split_part(version, '.', 3)::int
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
                "1.0.0".to_string()
            ),
            (
                "0.3.0".to_string(),
                "generic runtime scope projections".to_string(),
                "1.0.0".to_string()
            ),
            (
                "0.4.0".to_string(),
                "database hard-cut performance contracts".to_string(),
                "1.0.0".to_string()
            ),
            (
                "0.5.0".to_string(),
                "policy-driven durable retention".to_string(),
                "1.0.0".to_string()
            ),
            (
                "0.6.0".to_string(),
                "job payload blob agent invocation metadata".to_string(),
                "1.0.0".to_string()
            ),
            (
                "0.7.0".to_string(),
                "job payload blob text response attachments".to_string(),
                "1.0.0".to_string()
            ),
            (
                "0.8.0".to_string(),
                "transcription source mux slots".to_string(),
                "1.0.0".to_string()
            ),
            (
                "0.9.0".to_string(),
                "durable transcription mux planner".to_string(),
                "1.0.0".to_string()
            ),
            (
                "0.10.0".to_string(),
                "voice status snapshot payload state".to_string(),
                "1.0.0".to_string()
            ),
            (
                "0.11.0".to_string(),
                "bounded timeline dashboard reads".to_string(),
                "1.0.0".to_string()
            ),
            (
                "0.12.0".to_string(),
                "bounded wake transcription settlement".to_string(),
                "1.0.0".to_string()
            ),
            (
                "0.13.0".to_string(),
                "durable operational outcomes and canonical event scopes".to_string(),
                "1.0.0".to_string()
            ),
            (
                "1.0.0".to_string(),
                "job spec authority, schedules, durable wake circuit, typed agent outcomes"
                    .to_string(),
                "1.0.0".to_string()
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
        "DELETE FROM clankcord_schema_migrations WHERE version IN ('0.4.0', '0.5.0', '0.6.0', '0.7.0', '0.8.0', '0.9.0', '0.10.0', '0.11.0', '0.12.0', '0.13.0', '1.0.0')",
    )
    .execute(&store.pool)
    .await
    .unwrap();

    let applied = store.run_pending_schema_migrations().await.unwrap();

    assert_eq!(applied.len(), 11);
    assert_eq!(applied[0].version, "0.4.0");
    assert_eq!(applied[1].version, "0.5.0");
    assert_eq!(applied[2].version, "0.6.0");
    assert_eq!(applied[3].version, "0.7.0");
    assert_eq!(applied[4].version, "0.8.0");
    assert_eq!(applied[5].version, "0.9.0");
    assert_eq!(applied[6].version, "0.10.0");
    assert_eq!(applied[7].version, "0.11.0");
    assert_eq!(applied[8].version, "0.12.0");
    assert_eq!(applied[9].version, "0.13.0");
    assert_eq!(applied[10].version, "1.0.0");
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

    sqlx::query(
        "DELETE FROM clankcord_schema_migrations WHERE version IN ('0.5.0', '0.6.0', '0.7.0', '0.8.0', '0.9.0', '0.10.0', '0.11.0', '0.12.0', '0.13.0', '1.0.0')",
    )
    .execute(&store.pool)
    .await
    .unwrap();

    let applied = store.run_pending_schema_migrations().await.unwrap();

    assert_eq!(applied.len(), 10);
    assert_eq!(applied[0].version, "0.5.0");
    assert_eq!(applied[1].version, "0.6.0");
    assert_eq!(applied[2].version, "0.7.0");
    assert_eq!(applied[3].version, "0.8.0");
    assert_eq!(applied[4].version, "0.9.0");
    assert_eq!(applied[5].version, "0.10.0");
    assert_eq!(applied[6].version, "0.11.0");
    assert_eq!(applied[7].version, "0.12.0");
    assert_eq!(applied[8].version, "0.13.0");
    assert_eq!(applied[9].version, "1.0.0");
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
    sqlx::query(
        "DELETE FROM clankcord_schema_migrations WHERE version IN ('0.6.0', '0.7.0', '0.8.0', '0.9.0', '0.10.0', '0.11.0', '0.12.0', '0.13.0', '1.0.0')",
    )
    .execute(&store.pool)
    .await
    .unwrap();

    let applied = store.run_pending_schema_migrations().await.unwrap();

    assert_eq!(applied.len(), 9);
    assert_eq!(applied[0].version, "0.6.0");
    assert_eq!(applied[1].version, "0.7.0");
    assert_eq!(applied[2].version, "0.8.0");
    assert_eq!(applied[3].version, "0.9.0");
    assert_eq!(applied[4].version, "0.10.0");
    assert_eq!(applied[5].version, "0.11.0");
    assert_eq!(applied[6].version, "0.12.0");
    assert_eq!(applied[7].version, "0.13.0");
    assert_eq!(applied[8].version, "1.0.0");
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
async fn v0_7_0_schema_migration_rewrites_v4_text_delivery_payload_blob() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(&raw.path().join("voice")).await;
    let created = store
        .create_job(Job::text_delivery(
            RuntimeScope::voice_channel("guild", "code"),
            "user-a",
            TextDeliveryPayload::from_json(&json!({
                "intent": "message",
                "target": "agent_chat",
                "source_job_id": "job_source",
                "requested_by_user_id": "user-a",
                "content": "Attached a benchmark.",
            }))
            .unwrap(),
        ))
        .await
        .unwrap();
    let legacy_blob = encode_pre_v0_7_0_text_delivery_job(
        &created,
        BinaryPayload::from_json(&json!({
            "extra_boundary_field": {"kept": true},
            "attachments": [{"path": "/workspace/old-opaque.zip"}]
        }))
        .unwrap(),
    );

    sqlx::query("UPDATE job_payloads SET payload_blob = $1 WHERE job_id = $2")
        .bind(legacy_blob)
        .bind(&created.id)
        .execute(&store.pool)
        .await
        .unwrap();
    sqlx::query(
        "DELETE FROM clankcord_schema_migrations WHERE version IN ('0.7.0', '0.8.0', '0.9.0', '0.10.0', '0.11.0', '0.12.0', '0.13.0', '1.0.0')",
    )
    .execute(&store.pool)
    .await
    .unwrap();

    let applied = store.run_pending_schema_migrations().await.unwrap();

    assert_eq!(applied.len(), 8);
    assert_eq!(applied[0].version, "0.7.0");
    assert_eq!(applied[1].version, "0.8.0");
    assert_eq!(applied[2].version, "0.9.0");
    assert_eq!(applied[3].version, "0.10.0");
    assert_eq!(applied[4].version, "0.11.0");
    assert_eq!(applied[5].version, "0.12.0");
    assert_eq!(applied[6].version, "0.13.0");
    assert_eq!(applied[7].version, "1.0.0");
    let migrated = store.get_job(&created.id).await.unwrap();
    let payload = migrated.text_delivery_payload().unwrap();
    assert!(payload.attachments.is_empty());
    assert_eq!(
        payload.to_json()["extra_boundary_field"]["kept"],
        json!(true)
    );
    assert!(payload.to_json()["attachments"].is_null());
    let row = sqlx::query("SELECT payload_blob FROM job_payloads WHERE job_id = $1")
        .bind(&created.id)
        .fetch_one(&store.pool)
        .await
        .unwrap();
    let payload_blob: Vec<u8> = sqlx::Row::try_get(&row, "payload_blob").unwrap();
    assert!(Job::is_current_payload_blob(&payload_blob));
}
#[tokio::test(flavor = "current_thread")]
async fn v0_10_0_schema_migration_rewrites_v7_voice_status_snapshot_outputs() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(&raw.path().join("voice")).await;
    let mut old_snapshot = Job::discord_voice_status_snapshot("job_source");
    old_snapshot.mark_complete();
    let old_snapshot = store.create_job(old_snapshot).await.unwrap();
    let mut new_snapshot = Job::discord_voice_status_snapshot("job_source");
    new_snapshot.mark_complete();
    new_snapshot.metadata.output = Some(JobOutput::DiscordVoiceStatusSnapshot(
        DiscordVoiceStatusSnapshotOutput {
            bots: Vec::new(),
            sessions: Vec::new(),
            voice_state_guild_ids: vec!["guild".to_string()],
            voice_states: vec![OpaqueValue::from_json(&json!({
                "guild_id": "guild",
                "user_id": "user-a",
                "voice_channel_id": "code",
            }))],
        },
    ));
    let new_snapshot = store.create_job(new_snapshot).await.unwrap();

    sqlx::query("UPDATE job_payloads SET payload_blob = $1 WHERE job_id = $2")
        .bind(encode_pre_v0_10_0_voice_status_snapshot_job(&old_snapshot))
        .bind(&old_snapshot.id)
        .execute(&store.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE job_payloads SET payload_blob = $1 WHERE job_id = $2")
        .bind(encode_job_with_blob_version(&new_snapshot, 7))
        .bind(&new_snapshot.id)
        .execute(&store.pool)
        .await
        .unwrap();
    sqlx::query(
        "DELETE FROM clankcord_schema_migrations WHERE version IN ('0.10.0', '0.11.0', '0.12.0', '0.13.0', '1.0.0')",
    )
    .execute(&store.pool)
    .await
    .unwrap();

    let applied = store.run_pending_schema_migrations().await.unwrap();

    assert_eq!(applied.len(), 5);
    assert_eq!(applied[0].version, "0.10.0");
    assert_eq!(applied[1].version, "0.11.0");
    assert_eq!(applied[2].version, "0.12.0");
    assert_eq!(applied[3].version, "0.13.0");
    assert_eq!(applied[4].version, "1.0.0");
    let migrated_old = store.get_job(&old_snapshot.id).await.unwrap();
    let Some(JobOutput::DiscordVoiceStatusSnapshot(output)) = migrated_old.metadata.output else {
        panic!("migrated old status snapshot output");
    };
    assert!(output.voice_state_guild_ids.is_empty());
    assert!(output.voice_states.is_empty());
    let migrated_new = store.get_job(&new_snapshot.id).await.unwrap();
    let Some(JobOutput::DiscordVoiceStatusSnapshot(output)) = migrated_new.metadata.output else {
        panic!("migrated new status snapshot output");
    };
    assert_eq!(output.voice_state_guild_ids, vec!["guild".to_string()]);
    assert_eq!(output.voice_states.len(), 1);
    assert_eq!(output.voice_states[0].to_json()["voice_channel_id"], "code");
    for job_id in [&old_snapshot.id, &new_snapshot.id] {
        let row = sqlx::query("SELECT payload_blob FROM job_payloads WHERE job_id = $1")
            .bind(job_id)
            .fetch_one(&store.pool)
            .await
            .unwrap();
        let payload_blob: Vec<u8> = sqlx::Row::try_get(&row, "payload_blob").unwrap();
        assert!(Job::is_current_payload_blob(&payload_blob));
    }
}
#[tokio::test(flavor = "current_thread")]
async fn v0_11_0_schema_migration_creates_recent_unforgotten_timeline_index() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(&raw.path().join("voice")).await;

    sqlx::query("DROP INDEX idx_timeline_recent_unforgotten")
        .execute(&store.pool)
        .await
        .unwrap();
    sqlx::query(
        "DELETE FROM clankcord_schema_migrations WHERE version IN ('0.11.0', '0.12.0', '0.13.0', '1.0.0')",
    )
    .execute(&store.pool)
    .await
    .unwrap();
    assert!(!index_exists(&store.pool, "idx_timeline_recent_unforgotten").await);

    let applied = store.run_pending_schema_migrations().await.unwrap();

    assert_eq!(applied.len(), 4);
    assert_eq!(applied[0].version, "0.11.0");
    assert_eq!(applied[1].version, "0.12.0");
    assert_eq!(applied[2].version, "0.13.0");
    assert_eq!(applied[3].version, "1.0.0");
    let definition = index_definition(&store.pool, "idx_timeline_recent_unforgotten").await;
    assert!(definition.contains("started_at_ms DESC, sequence DESC, event_id DESC"));
    assert!(definition.contains("WHERE (forgotten = false)"));
}
#[tokio::test(flavor = "current_thread")]
async fn v0_12_0_schema_migration_terminalizes_stranded_wakes_and_preserves_failed_slots() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(&raw.path().join("voice")).await;
    let runtime = Ctx::new(store.clone());
    let activation = store
        .create_job(Job::wake_activation(wake_activation_payload(
            "guild", "code",
        )))
        .await
        .unwrap();
    let source_job_id = create_audio_segment_slot(
        &store,
        &runtime,
        raw.path(),
        "user-b",
        Utc::now() - Duration::minutes(5),
        Duration::seconds(2),
        912,
    )
    .await;
    sqlx::query(
        r#"
        UPDATE transcription_slots
        SET state = 'failed',
            payload_json = payload_json || jsonb_build_object(
              'state', 'failed',
              'error', 'provider returned malformed timestamp payload'
            )
        WHERE source_job_id = $1
        "#,
    )
    .bind(&source_job_id)
    .execute(&store.pool)
    .await
    .unwrap();

    sqlx::raw_sql(
        r#"
        DROP TABLE wake_activation_progress;
        DROP INDEX idx_transcription_slots_scope_state_interval;
        ALTER TABLE transcription_slots DROP COLUMN requires_single_slot;
        DELETE FROM clankcord_schema_migrations WHERE version IN ('0.12.0', '0.13.0', '1.0.0');
        "#,
    )
    .execute(&store.pool)
    .await
    .unwrap();

    let applied = store.run_pending_schema_migrations().await.unwrap();

    assert_eq!(applied.len(), 3);
    assert_eq!(applied[0].version, "0.12.0");
    assert_eq!(applied[1].version, "0.13.0");
    assert_eq!(applied[2].version, "1.0.0");
    assert!(column_exists(&store.pool, "transcription_slots", "requires_single_slot").await);
    assert!(index_exists(&store.pool, "idx_transcription_slots_scope_state_interval").await);
    let migrated = store.get_job(&activation.id).await.unwrap();
    assert_eq!(migrated.state, JobState::Failed);
    assert_eq!(
        migrated.metadata.error,
        "expired_transcription_wait: activation predates bounded transcription settlement"
    );
    let projection = sqlx::query("SELECT state, terminal, failed FROM jobs WHERE job_id = $1")
        .bind(&activation.id)
        .fetch_one(&store.pool)
        .await
        .unwrap();
    assert_eq!(
        sqlx::Row::try_get::<String, _>(&projection, "state").unwrap(),
        "failed"
    );
    assert!(sqlx::Row::try_get::<bool, _>(&projection, "terminal").unwrap());
    assert!(sqlx::Row::try_get::<bool, _>(&projection, "failed").unwrap());
    let slot = sqlx::query(
        "SELECT state, payload_json->>'error' AS error FROM transcription_slots WHERE source_job_id = $1",
    )
    .bind(&source_job_id)
    .fetch_one(&store.pool)
    .await
    .unwrap();
    assert_eq!(
        sqlx::Row::try_get::<String, _>(&slot, "state").unwrap(),
        "failed"
    );
    assert_eq!(
        sqlx::Row::try_get::<String, _>(&slot, "error").unwrap(),
        "provider returned malformed timestamp payload"
    );
}
#[tokio::test(flavor = "current_thread")]
async fn v0_13_0_schema_migration_backfills_retained_terminal_job_outcomes() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(&raw.path().join("voice")).await;
    let failed = store
        .create_job(Job::agent_task_for_session(
            "ags_failure_reason",
            RuntimeScope::dm("user-a"),
            "user-a",
            CommandRequest::agent_task("", "user-a", "user-a", "test nested dispatch error"),
        ))
        .await
        .unwrap();
    let encoded = encode_current_agent_task(
        &failed,
        "",
        "test nested dispatch error",
        "provider dispatch rejected model gpt-test",
    );
    sqlx::query("UPDATE job_payloads SET payload_blob = $1 WHERE job_id = $2")
        .bind(encoded)
        .bind(&failed.id)
        .execute(&store.pool)
        .await
        .unwrap();
    let mut failed = store.get_job(&failed.id).await.unwrap();
    failed.set_state(JobState::Failed);
    failed.touch();
    store.update_job(&failed).await.unwrap();
    assert!(failed.metadata.error.is_empty());

    sqlx::raw_sql(
        r#"
        DROP TABLE operational_job_outcomes;
        DELETE FROM runtime_metadata
          WHERE key = 'operational_job_outcomes_coverage_start_ms';
        DELETE FROM clankcord_schema_migrations WHERE version IN ('0.13.0', '1.0.0');
        "#,
    )
    .execute(&store.pool)
    .await
    .unwrap();

    let applied = store.run_pending_schema_migrations().await.unwrap();

    assert_eq!(applied.len(), 2);
    assert_eq!(applied[0].version, "0.13.0");
    assert_eq!(applied[1].version, "1.0.0");
    assert!(column_exists(&store.pool, "operational_job_outcomes", "observed_at_ms").await);
    assert!(index_exists(&store.pool, "idx_operational_job_outcomes_observed_kind").await);
    assert!(index_exists(&store.pool, "idx_operational_job_outcomes_job_observed").await);
    assert!(index_exists(&store.pool, "idx_operational_job_outcomes_transition").await);
    assert!(index_exists(&store.pool, "idx_operational_job_failures_observed").await);
    let outcome = sqlx::query(
        r#"
        SELECT job_id, kind, state, failed, error_text
        FROM operational_job_outcomes
        WHERE job_id = $1
        "#,
    )
    .bind(&failed.id)
    .fetch_one(&store.pool)
    .await
    .unwrap();
    assert_eq!(
        sqlx::Row::try_get::<String, _>(&outcome, "kind").unwrap(),
        "agent_task"
    );
    assert_eq!(
        sqlx::Row::try_get::<String, _>(&outcome, "state").unwrap(),
        "failed"
    );
    assert!(sqlx::Row::try_get::<bool, _>(&outcome, "failed").unwrap());
    assert_eq!(
        sqlx::Row::try_get::<String, _>(&outcome, "error_text").unwrap(),
        "provider dispatch rejected model gpt-test"
    );
    let coverage_start = sqlx::query_scalar::<_, String>(
        "SELECT value FROM runtime_metadata WHERE key = 'operational_job_outcomes_coverage_start_ms'",
    )
    .fetch_one(&store.pool)
    .await
    .unwrap();
    assert!(coverage_start.parse::<i64>().unwrap() > 0);

    let runtime = Ctx::new(store);
    let overview =
        clankcord::views::operations::dashboard_health_payload(&runtime, json!({}), json!({}))
            .await
            .unwrap();
    let dashboard_failure = overview["health"]["failures"]["recent"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["jobId"] == failed.id)
        .unwrap();
    assert_eq!(
        dashboard_failure["reason"],
        json!("provider dispatch rejected model gpt-test")
    );
}
#[tokio::test(flavor = "current_thread")]
async fn v0_13_0_schema_migration_canonicalizes_generic_timeline_event_scopes() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(&raw.path().join("event-scopes")).await;

    let dm_job = store
        .create_job(Job::new(
            RuntimeScope::dm("dm-user"),
            "dm-user",
            JobState::Queued,
            JobPayload::Command(clankcord::model::job::CommandPayload {
                command: CommandRequest::agent_task("", "dm-user", "dm-user", "dm feedback"),
            }),
        ))
        .await
        .unwrap();
    let text_job = store
        .create_job(Job::new(
            RuntimeScope::text_channel("guild-a", "text-a"),
            "user-a",
            JobState::Queued,
            JobPayload::Command(clankcord::model::job::CommandPayload {
                command: CommandRequest::agent_task("guild-a", "text-a", "user-a", "text delivery"),
            }),
        ))
        .await
        .unwrap();
    let voice_job = store
        .create_job(Job::new(
            RuntimeScope::voice_channel("guild-a", "voice-a"),
            "user-a",
            JobState::Queued,
            JobPayload::Command(clankcord::model::job::CommandPayload {
                command: CommandRequest::agent_task("guild-a", "voice-a", "user-a", "voice typing"),
            }),
        ))
        .await
        .unwrap();

    let dm_event = store
        .append_event(
            "dm",
            "legacy-dm-channel",
            json!({
                "event_kind": "feedback",
                "job_id": dm_job.id,
                "voice_channel_id": "legacy-dm-channel",
                "channelId": "legacy-dm-channel",
            }),
        )
        .await
        .unwrap();
    let text_event = store
        .append_event(
            "guild-a",
            "legacy-text-channel",
            json!({
                "event_kind": "text_delivered",
                "job_id": text_job.id,
                "voice_channel_id": "legacy-text-channel",
                "channelId": "legacy-text-channel",
            }),
        )
        .await
        .unwrap();
    let voice_event = store
        .append_event(
            "guild-a",
            "voice-a",
            json!({
                "event_kind": "discord_typing_indicator",
                "job_id": voice_job.id,
            }),
        )
        .await
        .unwrap();
    let dm_session_event = store
        .append_event(
            "dm",
            "dm-session-user",
            json!({
                "event_kind": "agent_session_retired",
                "agent_session": {
                    "route_kind": "dm",
                    "guild_id": "dm",
                    "scope_id": "dm-session-user",
                    "dm_user_id": "dm-session-user",
                },
            }),
        )
        .await
        .unwrap();
    let thread_session_event = store
        .append_event(
            "guild-a",
            "legacy-thread-parent",
            json!({
                "event_kind": "agent_session_thread_unavailable",
                "agent_session": {
                    "route_kind": "thread",
                    "guild_id": "guild-a",
                    "scope_id": "legacy-thread-parent",
                    "discord_thread_id": "thread-a",
                },
            }),
        )
        .await
        .unwrap();

    sqlx::raw_sql(
        r#"
        DROP TABLE operational_job_outcomes;
        DELETE FROM runtime_metadata
          WHERE key = 'operational_job_outcomes_coverage_start_ms';
        DELETE FROM clankcord_schema_migrations WHERE version IN ('0.13.0', '1.0.0');
        "#,
    )
    .execute(&store.pool)
    .await
    .unwrap();

    let applied = store.run_pending_schema_migrations().await.unwrap();
    assert_eq!(applied.len(), 2);
    assert_eq!(applied[0].version, "0.13.0");
    assert_eq!(applied[1].version, "1.0.0");

    for (event, scope_kind, guild_id, scope_id) in [
        (&dm_event, "dm", "", "dm-user"),
        (&text_event, "text_channel", "guild-a", "text-a"),
        (&voice_event, "voice_channel", "guild-a", "voice-a"),
        (&dm_session_event, "dm", "", "dm-session-user"),
        (&thread_session_event, "thread", "guild-a", "thread-a"),
    ] {
        let event_id = event["event_id"].as_str().unwrap();
        let migrated = store.get_event(event_id).await.unwrap();
        assert_eq!(migrated["event_id"], json!(event_id));
        assert_eq!(migrated["scope_kind"], json!(scope_kind));
        assert_eq!(migrated["scope_id"], json!(scope_id));
        assert_eq!(migrated["guild_id"], json!(guild_id));
        if scope_kind == "voice_channel" {
            assert_eq!(migrated["voice_channel_id"], json!(scope_id));
            assert_eq!(migrated["channelId"], json!(scope_id));
        } else {
            assert!(migrated.get("voice_channel_id").is_none());
            assert!(migrated.get("voiceChannelId").is_none());
            assert!(migrated.get("channelId").is_none());
        }
    }

    let dm_event_id = dm_event["event_id"].as_str().unwrap();
    sqlx::query(
        r#"
        UPDATE timeline_events
        SET payload_json = payload_json || jsonb_build_object(
          'event_id', 'payload-event-id',
          'eventId', 'payload-event-id',
          'event_kind', 'payload-kind',
          'kind', 'payload-kind',
          'scope_kind', 'voice_channel',
          'scope_id', 'payload-scope',
          'guild_id', 'payload-guild',
          'voice_channel_id', 'payload-scope',
          'channelId', 'payload-scope'
        )
        WHERE event_id = $1
        "#,
    )
    .bind(dm_event_id)
    .execute(&store.pool)
    .await
    .unwrap();
    let canonical = store.get_event(dm_event_id).await.unwrap();
    assert_eq!(canonical["event_id"], json!(dm_event_id));
    assert_eq!(canonical["event_kind"], json!("feedback"));
    assert_eq!(canonical["kind"], json!("feedback"));
    assert_eq!(canonical["scope_kind"], json!("dm"));
    assert_eq!(canonical["scope_id"], json!("dm-user"));
    assert_eq!(canonical["guild_id"], json!(""));
    assert!(canonical.get("voice_channel_id").is_none());
    assert!(canonical.get("channelId").is_none());
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
async fn index_definition(pool: &sqlx::PgPool, index: &str) -> String {
    let row = sqlx::query(
        r#"
        SELECT indexdef
        FROM pg_indexes
        WHERE schemaname = current_schema()
          AND indexname = $1
        "#,
    )
    .bind(index)
    .fetch_one(pool)
    .await
    .unwrap();
    sqlx::Row::try_get(&row, "indexdef").unwrap()
}
