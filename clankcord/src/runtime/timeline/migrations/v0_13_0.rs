use sqlx::Row;

use crate::Result;
use crate::runtime::Job;

use super::super::store::OPERATIONAL_JOB_OUTCOME_RETENTION_SECONDS;

const OPERATIONAL_COVERAGE_START_KEY: &str = "operational_job_outcomes_coverage_start_ms";

pub(super) async fn run(transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>) -> Result<()> {
    migrate_timeline_event_scopes(transaction).await?;
    create_operational_job_outcomes(transaction).await?;
    backfill_retained_terminal_outcomes(transaction).await?;
    record_coverage_start(transaction).await
}

async fn migrate_timeline_event_scopes(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<()> {
    migrate_job_linked_event_scopes(transaction).await?;
    migrate_agent_session_event_scopes(transaction).await
}

async fn migrate_job_linked_event_scopes(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<()> {
    sqlx::raw_sql(
        r#"
        WITH scoped_events AS (
          SELECT e.sequence, e.payload_json, j.scope_kind, j.guild_id, j.scope_id
          FROM timeline_events e
          JOIN jobs j ON j.job_id = e.payload_json->>'job_id'
          WHERE e.event_kind IN (
            'discord_slash_command',
            'feedback',
            'text_delivered',
            'discord_typing_indicator',
            'agent_task_result_suppressed',
            'agent_mcp_token_warning'
          )
        ), rewritten AS (
          SELECT sequence, scope_kind, guild_id, scope_id,
                 CASE
                   WHEN scope_kind = 'voice_channel' THEN
                     payload_json || jsonb_build_object(
                       'scope_kind', scope_kind,
                       'scopeKind', scope_kind,
                       'scope_id', scope_id,
                       'scopeId', scope_id,
                       'guild_id', guild_id,
                       'guildId', guild_id,
                       'voice_channel_id', scope_id,
                       'voiceChannelId', scope_id,
                       'channelId', scope_id
                     )
                   ELSE
                     (payload_json
                       - 'voice_channel_id'
                       - 'voiceChannelId'
                       - 'channelId'
                       - 'voice_channel_name'
                       - 'channelName'
                       - 'voice_channel_slug'
                       - 'channelSlug') || jsonb_build_object(
                         'scope_kind', scope_kind,
                         'scopeKind', scope_kind,
                         'scope_id', scope_id,
                         'scopeId', scope_id,
                         'guild_id', guild_id,
                         'guildId', guild_id
                       )
                 END AS payload_json
          FROM scoped_events
        )
        UPDATE timeline_events event
        SET scope_kind = rewritten.scope_kind,
            guild_id = rewritten.guild_id,
            scope_id = rewritten.scope_id,
            payload_json = rewritten.payload_json
        FROM rewritten
        WHERE event.sequence = rewritten.sequence;
        "#,
    )
    .execute(transaction.as_mut())
    .await?;
    Ok(())
}

async fn migrate_agent_session_event_scopes(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<()> {
    let invalid = sqlx::query_scalar::<_, i64>(
        r#"
        SELECT COUNT(*)
        FROM timeline_events
        WHERE event_kind IN (
          'agent_session_resumed',
          'agent_session_retired',
          'agent_session_thread_unavailable'
        )
          AND payload_json->'agent_session'->>'route_kind' IN ('dm', 'thread')
          AND (
            (
              payload_json->'agent_session'->>'route_kind' = 'dm'
              AND COALESCE(
                NULLIF(payload_json->'agent_session'->>'dm_user_id', ''),
                NULLIF(payload_json->'agent_session'->>'scope_id', '')
              ) IS NULL
            )
            OR (
              payload_json->'agent_session'->>'route_kind' = 'thread'
              AND (
                COALESCE(payload_json->'agent_session'->>'guild_id', '') = ''
                OR COALESCE(payload_json->'agent_session'->>'discord_thread_id', '') = ''
              )
            )
          )
        "#,
    )
    .fetch_one(transaction.as_mut())
    .await?;
    if invalid > 0 {
        anyhow::bail!(
            "v0.13.0 cannot migrate {invalid} agent session timeline events with incomplete route identity"
        );
    }

    sqlx::raw_sql(
        r#"
        WITH scoped_events AS (
          SELECT sequence, payload_json,
                 CASE payload_json->'agent_session'->>'route_kind'
                   WHEN 'dm' THEN 'dm'
                   WHEN 'thread' THEN 'thread'
                 END AS scope_kind,
                 CASE payload_json->'agent_session'->>'route_kind'
                   WHEN 'dm' THEN ''
                   WHEN 'thread' THEN payload_json->'agent_session'->>'guild_id'
                 END AS guild_id,
                 CASE payload_json->'agent_session'->>'route_kind'
                   WHEN 'dm' THEN COALESCE(
                     NULLIF(payload_json->'agent_session'->>'dm_user_id', ''),
                     payload_json->'agent_session'->>'scope_id'
                   )
                   WHEN 'thread' THEN payload_json->'agent_session'->>'discord_thread_id'
                 END AS scope_id
          FROM timeline_events
          WHERE event_kind IN (
            'agent_session_resumed',
            'agent_session_retired',
            'agent_session_thread_unavailable'
          )
            AND payload_json->'agent_session'->>'route_kind' IN ('dm', 'thread')
        ), rewritten AS (
          SELECT sequence, scope_kind, guild_id, scope_id,
                 (payload_json
                   - 'voice_channel_id'
                   - 'voiceChannelId'
                   - 'channelId'
                   - 'voice_channel_name'
                   - 'channelName'
                   - 'voice_channel_slug'
                   - 'channelSlug') || jsonb_build_object(
                     'scope_kind', scope_kind,
                     'scopeKind', scope_kind,
                     'scope_id', scope_id,
                     'scopeId', scope_id,
                     'guild_id', guild_id,
                     'guildId', guild_id
                   ) AS payload_json
          FROM scoped_events
        )
        UPDATE timeline_events event
        SET scope_kind = rewritten.scope_kind,
            guild_id = rewritten.guild_id,
            scope_id = rewritten.scope_id,
            payload_json = rewritten.payload_json
        FROM rewritten
        WHERE event.sequence = rewritten.sequence;
        "#,
    )
    .execute(transaction.as_mut())
    .await?;
    Ok(())
}

async fn create_operational_job_outcomes(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<()> {
    sqlx::raw_sql(
        r#"
        CREATE TABLE IF NOT EXISTS operational_job_outcomes (
          observation_id BIGSERIAL PRIMARY KEY,
          job_id TEXT NOT NULL,
          scope_kind TEXT NOT NULL,
          guild_id TEXT NOT NULL,
          scope_id TEXT NOT NULL,
          kind TEXT NOT NULL,
          state TEXT NOT NULL,
          lane TEXT NOT NULL,
          created_at_ms BIGINT NOT NULL,
          ready_at_ms BIGINT NOT NULL,
          started_at_ms BIGINT,
          completed_at_ms BIGINT,
          observed_at_ms BIGINT NOT NULL,
          failed BOOLEAN NOT NULL,
          error_text TEXT NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_operational_job_outcomes_observed_kind
          ON operational_job_outcomes(observed_at_ms DESC, kind, observation_id DESC);
        CREATE INDEX IF NOT EXISTS idx_operational_job_outcomes_job_observed
          ON operational_job_outcomes(job_id, observed_at_ms DESC, observation_id DESC);
        CREATE UNIQUE INDEX IF NOT EXISTS idx_operational_job_outcomes_transition
          ON operational_job_outcomes(job_id, state, observed_at_ms);
        CREATE INDEX IF NOT EXISTS idx_operational_job_failures_observed
          ON operational_job_outcomes(observed_at_ms DESC, observation_id DESC)
          WHERE failed = TRUE;
        "#,
    )
    .execute(transaction.as_mut())
    .await?;
    Ok(())
}

async fn backfill_retained_terminal_outcomes(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<()> {
    let retention_cutoff_ms = chrono::Utc::now()
        .timestamp_millis()
        .saturating_sub(OPERATIONAL_JOB_OUTCOME_RETENTION_SECONDS.saturating_mul(1000));
    let rows = sqlx::query(
        r#"
        SELECT j.job_id, j.scope_kind, j.guild_id, j.scope_id, j.kind, j.state,
               j.lane, j.created_at_ms, j.ready_at_ms, j.started_at_ms,
               j.completed_at_ms, j.updated_at_ms, j.failed, p.payload_blob
        FROM jobs j
        JOIN job_payloads p ON p.job_id = j.job_id
        WHERE j.terminal = TRUE
          AND j.updated_at_ms >= $1
          AND NOT EXISTS (
            SELECT 1
            FROM operational_job_outcomes outcome
            WHERE outcome.job_id = j.job_id
          )
        ORDER BY j.updated_at_ms, j.job_id
        "#,
    )
    .bind(retention_cutoff_ms)
    .fetch_all(transaction.as_mut())
    .await?;

    for row in rows {
        let job = Job::decode(&row.try_get::<Vec<u8>, _>("payload_blob")?)?;
        sqlx::query(
            r#"
            INSERT INTO operational_job_outcomes(
              job_id, scope_kind, guild_id, scope_id, kind, state, lane,
              created_at_ms, ready_at_ms, started_at_ms, completed_at_ms,
              observed_at_ms, failed, error_text
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)
            ON CONFLICT(job_id, state, observed_at_ms) DO NOTHING
            "#,
        )
        .bind(row.try_get::<String, _>("job_id")?)
        .bind(row.try_get::<String, _>("scope_kind")?)
        .bind(row.try_get::<String, _>("guild_id")?)
        .bind(row.try_get::<String, _>("scope_id")?)
        .bind(row.try_get::<String, _>("kind")?)
        .bind(row.try_get::<String, _>("state")?)
        .bind(row.try_get::<String, _>("lane")?)
        .bind(row.try_get::<i64, _>("created_at_ms")?)
        .bind(row.try_get::<i64, _>("ready_at_ms")?)
        .bind(row.try_get::<Option<i64>, _>("started_at_ms")?)
        .bind(row.try_get::<Option<i64>, _>("completed_at_ms")?)
        .bind(row.try_get::<i64, _>("updated_at_ms")?)
        .bind(row.try_get::<bool, _>("failed")?)
        .bind(job.operational_outcome_reason())
        .execute(transaction.as_mut())
        .await?;
    }
    Ok(())
}

async fn record_coverage_start(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<()> {
    let now_ms = chrono::Utc::now().timestamp_millis();
    sqlx::query(
        r#"
        INSERT INTO runtime_metadata(key, value, updated_at_ms)
        VALUES ($1, $2, $3)
        ON CONFLICT(key) DO UPDATE SET
          value = EXCLUDED.value,
          updated_at_ms = EXCLUDED.updated_at_ms
        "#,
    )
    .bind(OPERATIONAL_COVERAGE_START_KEY)
    .bind(now_ms.to_string())
    .bind(now_ms)
    .execute(transaction.as_mut())
    .await?;
    Ok(())
}
