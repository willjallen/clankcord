//! Agent session views shared by the dashboard agents list and the
//! agent-job detail page.

use serde_json::{Value, json};
use sqlx::Row;

use crate::Result;
use crate::domain::Ctx;
use crate::model::agents::task_session_key;
use crate::model::job::{Job, JobState};
use crate::time::{isoformat_z, ms_to_datetime};

/// The per-scope agent-session rollup: one implementation, one status
/// precedence (an active job wins; otherwise the latest job's failure
/// shows; otherwise idle). `scope` narrows to a single guild/scope pair.
pub(crate) async fn agent_session_rollups(
    ctx: &Ctx,
    scope: Option<(&str, &str)>,
) -> Result<Vec<Value>> {
    let scope_filter = if scope.is_some() {
        " AND guild_id = $1 AND scope_id = $2"
    } else {
        ""
    };
    let sql = format!(
        r#"
            WITH grouped AS MATERIALIZED (
              SELECT scope_kind, guild_id, scope_id,
                     COUNT(*)::BIGINT AS invocation_count,
                     MIN(created_at_ms) AS created_at_ms,
                     MAX(updated_at_ms) AS last_used_at_ms,
                     (ARRAY_AGG(job_id ORDER BY updated_at_ms DESC, job_id DESC))[1] AS latest_job_id,
                     (ARRAY_AGG(state ORDER BY updated_at_ms DESC, job_id DESC))[1] AS latest_state,
                     (ARRAY_AGG(job_id ORDER BY updated_at_ms DESC, job_id DESC)
                       FILTER (WHERE terminal = FALSE))[1] AS active_job_id
              FROM jobs
              WHERE kind = 'agent_task'{scope_filter}
              GROUP BY scope_kind, guild_id, scope_id
            ), latest AS MATERIALIZED (
              SELECT DISTINCT ON (j.scope_kind, j.guild_id, j.scope_id)
                     j.scope_kind, j.guild_id, j.scope_id, p.payload_blob
              FROM jobs j
              JOIN job_payloads p ON p.job_id = j.job_id
              WHERE j.kind = 'agent_task'{scope_filter2}
              ORDER BY j.scope_kind, j.guild_id, j.scope_id,
                       j.updated_at_ms DESC, j.job_id DESC
            )
            SELECT grouped.*, latest.payload_blob
            FROM grouped
            JOIN latest USING (scope_kind, guild_id, scope_id)
            ORDER BY grouped.last_used_at_ms DESC, grouped.guild_id, grouped.scope_id
            "#,
        scope_filter = scope_filter,
        scope_filter2 = scope_filter
            .replace(" guild_id", " j.guild_id")
            .replace(" scope_id", " j.scope_id"),
    );
    let mut query = sqlx::query(&sql);
    if let Some((guild_id, scope_id)) = scope {
        query = query.bind(guild_id).bind(scope_id);
    }
    let rows = query.fetch_all(&ctx.store.pool).await?;
    rows.into_iter()
        .map(|row| {
            let guild_id: String = row.try_get("guild_id")?;
            let scope_id: String = row.try_get("scope_id")?;
            let latest_state: String = row.try_get("latest_state")?;
            let active_job_id: Option<String> = row.try_get("active_job_id")?;
            let latest = Job::decode(&row.try_get::<Vec<u8>, _>("payload_blob")?)?;
            let task = latest.metadata.agent_task();
            let session_id = task
                .map(|task| task.agent.session_id.clone())
                .unwrap_or_default();
            let last_error = task
                .map(|task| task.dispatch_error.clone())
                .unwrap_or_default();
            let status = if active_job_id.is_some() {
                "running"
            } else if latest_state.parse::<JobState>()?.is_failed() {
                "failed"
            } else {
                "idle"
            };
            Ok(json!({
                "key": task_session_key(&guild_id, &scope_id),
                "role": "task",
                "scope_kind": row.try_get::<String, _>("scope_kind")?,
                "guild_id": guild_id,
                "scope_id": scope_id,
                "session_id": session_id,
                "active_job_id": active_job_id.unwrap_or_default(),
                "latest_job_id": row.try_get::<String, _>("latest_job_id")?,
                "status": status,
                "invocation_count": row.try_get::<i64, _>("invocation_count")?,
                "created_at": timestamp(row.try_get::<i64, _>("created_at_ms")?),
                "last_used_at": timestamp(row.try_get::<i64, _>("last_used_at_ms")?),
                "last_error": last_error,
            }))
        })
        .collect()
}

fn timestamp(milliseconds: i64) -> String {
    isoformat_z(Some(
        ms_to_datetime(milliseconds).expect("validated session timestamp"),
    ))
}
