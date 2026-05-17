use super::*;

use crate::runtime::{AgentSessionRecord, AgentSessionRecordState, AgentSessionRouteKind};

impl TimelineStore {
    pub async fn create_agent_session_record(
        &self,
        record: AgentSessionRecord,
    ) -> Result<AgentSessionRecord> {
        upsert_agent_session(&self.pool, &record).await?;
        Ok(record)
    }

    pub async fn update_agent_session_record(&self, record: &AgentSessionRecord) -> Result<()> {
        upsert_agent_session(&self.pool, record).await
    }

    pub async fn get_agent_session_record(
        &self,
        agent_session_id: &str,
    ) -> Result<AgentSessionRecord> {
        let row =
            sqlx::query("SELECT payload_blob FROM agent_sessions WHERE agent_session_id = $1")
                .bind(agent_session_id)
                .fetch_one(&self.pool)
                .await?;
        let payload_blob: Vec<u8> = row.try_get("payload_blob")?;
        AgentSessionRecord::decode(&payload_blob)
    }

    pub async fn maybe_agent_session_record(
        &self,
        agent_session_id: &str,
    ) -> Result<Option<AgentSessionRecord>> {
        let row =
            sqlx::query("SELECT payload_blob FROM agent_sessions WHERE agent_session_id = $1")
                .bind(agent_session_id)
                .fetch_optional(&self.pool)
                .await?;
        row.map(|row| {
            let payload_blob: Vec<u8> = row.try_get("payload_blob")?;
            AgentSessionRecord::decode(&payload_blob)
        })
        .transpose()
    }

    pub async fn active_agent_session_for_route(
        &self,
        route_key: &str,
    ) -> Result<Option<AgentSessionRecord>> {
        let now_ms = instant_ms_dt(utc_now());
        let row = sqlx::query(
            r#"
            SELECT payload_blob
            FROM agent_sessions
            WHERE route_key = $1
              AND state = 'active'
              AND max_active_until_ms > $2
            ORDER BY last_activity_at_ms DESC, created_at_ms DESC, agent_session_id DESC
            LIMIT 1
            "#,
        )
        .bind(route_key)
        .bind(now_ms)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| {
            let payload_blob: Vec<u8> = row.try_get("payload_blob")?;
            AgentSessionRecord::decode(&payload_blob)
        })
        .transpose()
    }

    pub async fn starting_agent_session_for_route(
        &self,
        route_key: &str,
    ) -> Result<Option<AgentSessionRecord>> {
        let now_ms = instant_ms_dt(utc_now());
        let row = sqlx::query(
            r#"
            SELECT payload_blob
            FROM agent_sessions
            WHERE route_key = $1
              AND state = 'starting'
              AND max_active_until_ms > $2
            ORDER BY created_at_ms DESC, agent_session_id DESC
            LIMIT 1
            "#,
        )
        .bind(route_key)
        .bind(now_ms)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| {
            let payload_blob: Vec<u8> = row.try_get("payload_blob")?;
            AgentSessionRecord::decode(&payload_blob)
        })
        .transpose()
    }

    pub async fn agent_session_for_thread(
        &self,
        thread_id: &str,
    ) -> Result<Option<AgentSessionRecord>> {
        let row = sqlx::query(
            r#"
            SELECT payload_blob
            FROM agent_sessions
            WHERE discord_thread_id = $1
            ORDER BY created_at_ms DESC, agent_session_id DESC
            LIMIT 1
            "#,
        )
        .bind(thread_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| {
            let payload_blob: Vec<u8> = row.try_get("payload_blob")?;
            AgentSessionRecord::decode(&payload_blob)
        })
        .transpose()
    }

    pub async fn list_agent_session_records(
        &self,
        guild_id: &str,
        scope_id: &str,
        state: &str,
        limit: usize,
    ) -> Result<Vec<AgentSessionRecord>> {
        let mut query = QueryBuilder::<Postgres>::new(
            r#"
            SELECT payload_blob
            FROM agent_sessions
            WHERE TRUE
            "#,
        );
        if !guild_id.trim().is_empty() {
            query.push(" AND guild_id = ").push_bind(guild_id);
        }
        if !scope_id.trim().is_empty() {
            query.push(" AND scope_id = ").push_bind(scope_id);
        }
        if !state.trim().is_empty() {
            query.push(" AND state = ").push_bind(state);
        }
        query.push(" ORDER BY created_at_ms DESC, agent_session_id DESC LIMIT ");
        query.push_bind(limit.max(1).min(500) as i64);
        let rows = query.build().fetch_all(&self.pool).await?;
        rows.into_iter()
            .map(|row| {
                let payload_blob: Vec<u8> = row.try_get("payload_blob")?;
                AgentSessionRecord::decode(&payload_blob)
            })
            .collect()
    }

    pub async fn retire_due_agent_sessions(&self) -> Result<Vec<AgentSessionRecord>> {
        let now = utc_now();
        let now_ms = instant_ms_dt(now);
        let rows = sqlx::query(
            r#"
            SELECT payload_blob
            FROM agent_sessions
            WHERE state IN ('starting', 'active')
            "#,
        )
        .fetch_all(&self.pool)
        .await?;
        let active_capture_session_ids = self
            .list_active_capture_sessions()
            .await?
            .into_iter()
            .map(|session| session.session_id)
            .collect::<BTreeSet<_>>();
        let mut retired = Vec::new();
        for row in rows {
            let payload_blob: Vec<u8> = row.try_get("payload_blob")?;
            let mut record = AgentSessionRecord::decode(&payload_blob)?;
            let reason = if instant_ms_str(Some(&record.max_active_until))
                .map(|deadline| deadline <= now_ms)
                .unwrap_or(false)
            {
                "max_duration"
            } else if record.route_kind == AgentSessionRouteKind::Voice
                && !record.voice_capture_session_id.trim().is_empty()
                && !active_capture_session_ids.contains(&record.voice_capture_session_id)
            {
                "voice_session_ended"
            } else {
                continue;
            };
            record.state = AgentSessionRecordState::Retired;
            record.retired_at = isoformat_z(Some(now));
            record.retirement_reason = reason.to_string();
            upsert_agent_session(&self.pool, &record).await?;
            retired.push(record);
        }
        Ok(retired)
    }
}

async fn upsert_agent_session(pool: &sqlx::PgPool, record: &AgentSessionRecord) -> Result<()> {
    let created_at_ms =
        instant_ms_str(Some(&record.created_at)).unwrap_or_else(|| instant_ms_dt(utc_now()));
    let last_activity_at_ms =
        instant_ms_str(Some(&record.last_activity_at)).unwrap_or(created_at_ms);
    let max_active_until_ms =
        instant_ms_str(Some(&record.max_active_until)).unwrap_or(created_at_ms);
    sqlx::query(
        r#"
        INSERT INTO agent_sessions(
          agent_session_id, codex_session_id, route_kind, route_key, guild_id, scope_id,
          dm_user_id, voice_capture_session_id, discord_thread_id, discord_parent_channel_id, text_target_kind,
          text_channel_id, text_user_id, state, created_at_ms, last_activity_at_ms,
          max_active_until_ms, retired_at_ms, retirement_reason, retired_by_user_id,
          resumed_from_agent_session_id, payload_blob
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21, $22)
        ON CONFLICT(agent_session_id) DO UPDATE SET
          codex_session_id = EXCLUDED.codex_session_id,
          route_kind = EXCLUDED.route_kind,
          route_key = EXCLUDED.route_key,
          guild_id = EXCLUDED.guild_id,
          scope_id = EXCLUDED.scope_id,
          dm_user_id = EXCLUDED.dm_user_id,
          voice_capture_session_id = EXCLUDED.voice_capture_session_id,
          discord_thread_id = EXCLUDED.discord_thread_id,
          discord_parent_channel_id = EXCLUDED.discord_parent_channel_id,
          text_target_kind = EXCLUDED.text_target_kind,
          text_channel_id = EXCLUDED.text_channel_id,
          text_user_id = EXCLUDED.text_user_id,
          state = EXCLUDED.state,
          created_at_ms = EXCLUDED.created_at_ms,
          last_activity_at_ms = EXCLUDED.last_activity_at_ms,
          max_active_until_ms = EXCLUDED.max_active_until_ms,
          retired_at_ms = EXCLUDED.retired_at_ms,
          retirement_reason = EXCLUDED.retirement_reason,
          retired_by_user_id = EXCLUDED.retired_by_user_id,
          resumed_from_agent_session_id = EXCLUDED.resumed_from_agent_session_id,
          payload_blob = EXCLUDED.payload_blob
        "#,
    )
    .bind(&record.agent_session_id)
    .bind(&record.codex_session_id)
    .bind(record.route_kind.as_str())
    .bind(&record.route_key)
    .bind(&record.guild_id)
    .bind(&record.scope_id)
    .bind(&record.dm_user_id)
    .bind(&record.voice_capture_session_id)
    .bind(&record.discord_thread_id)
    .bind(&record.discord_parent_channel_id)
    .bind(record.text_target.kind.as_str())
    .bind(&record.text_target.channel_id)
    .bind(&record.text_target.user_id)
    .bind(record.state.as_str())
    .bind(created_at_ms)
    .bind(last_activity_at_ms)
    .bind(max_active_until_ms)
    .bind(instant_ms_str(Some(&record.retired_at)))
    .bind(&record.retirement_reason)
    .bind(&record.retired_by_user_id)
    .bind(&record.resumed_from_agent_session_id)
    .bind(record.encode()?)
    .execute(pool)
    .await?;
    Ok(())
}
