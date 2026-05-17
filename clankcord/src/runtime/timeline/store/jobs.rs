use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobVisibility {
    Visible,
    IncludeEphemeral,
    OnlyEphemeral,
}

#[derive(Debug, Clone)]
struct JobProjection {
    created_at_ms: i64,
    updated_at_ms: i64,
    ready_at_ms: i64,
    started_at_ms: Option<i64>,
    completed_at_ms: Option<i64>,
    gc_after_ms: Option<i64>,
    terminal: bool,
    failed: bool,
    ephemeral: bool,
    cancellable: bool,
    lane: &'static str,
    ordering_key: String,
    command_kind: String,
    source_job_id: String,
    stream_id: String,
    target_job_id: String,
    speaker_user_id: String,
    segment_end_ms: Option<i64>,
}

#[derive(Debug, Clone)]
struct JobSummary {
    id: String,
    kind: crate::runtime::JobKind,
    state: crate::runtime::JobState,
}

impl TimelineStore {
    pub async fn create_job(&self, job: Job) -> Result<Job> {
        self.ensure_job_voice_room(&job).await?;
        let mut transaction = self.pool.begin().await?;
        upsert_job_rows(&mut transaction, &job).await?;
        transaction.commit().await?;
        if !job.kind.is_ephemeral() {
            let scope = job.scope();
            self.append_scope_event(
                &scope,
                serde_json::json!({
                    "event_kind": "job_created",
                    "kind": "job_created",
                    "job_id": job.id,
                    "job_kind": job.kind.as_str(),
                    "state": job.state.as_str()
                }),
            )
            .await?;
        }
        Ok(job)
    }

    pub async fn create_wake_probe_job(&self, job: Job) -> Result<Job> {
        self.create_job(job).await
    }

    pub async fn cancel_queued_wake_probes_for_stream(&self, stream_id: &str) -> Result<Vec<Job>> {
        let queued = self.queued_wake_probes_for_stream(stream_id).await?;
        let mut cancelled = Vec::new();
        for mut job in queued.into_iter().skip(1) {
            job.mark_cancelled();
            job.metadata.error =
                "duplicate queued wake probe for the same speaker stream".to_string();
            self.update_job(&job).await?;
            cancelled.push(job);
        }
        Ok(cancelled)
    }

    pub async fn cancel_stale_wake_probe_jobs(&self, max_age_seconds: i64) -> Result<Vec<Value>> {
        let max_age = chrono::Duration::seconds(max_age_seconds.max(1));
        let now = utc_now();
        let mut cancelled = Vec::new();
        for state in [
            crate::runtime::JobState::Queued,
            crate::runtime::JobState::Running,
        ] {
            let cutoff_ms = instant_ms_dt(now - max_age);
            for mut job in self
                .list_wake_probe_jobs_stale_in_state(state, cutoff_ms)
                .await?
            {
                if state == crate::runtime::JobState::Running {
                    job.set_state(crate::runtime::JobState::FailedTimeout);
                    job.metadata.error = "stale wake probe exceeded queue age limit".to_string();
                    job.metadata.timed_out_at = isoformat_z(Some(now));
                } else {
                    job.mark_cancelled();
                    job.metadata.error = "stale queued wake probe was dropped".to_string();
                }
                self.update_job(&job).await?;
                cancelled.push(job.to_value());
            }
        }
        Ok(cancelled)
    }

    pub async fn create_child_job(&self, parent: &Job, mut child: Job) -> Result<Job> {
        child.attach_to_parent(parent)?;
        self.ensure_dependency_is_acyclic(&parent.id, &child.id)
            .await?;
        self.ensure_job_voice_room(&child).await?;
        let mut transaction = self.pool.begin().await?;
        upsert_job_rows(&mut transaction, &child).await?;
        sqlx::query(
            r#"
            INSERT INTO job_dependencies(parent_job_id, child_job_id, dependency_kind, created_at_ms, resolution_policy)
            VALUES ($1, $2, 'required', $3, 'parent_resumes')
            ON CONFLICT(parent_job_id, child_job_id) DO NOTHING
            "#,
        )
        .bind(&parent.id)
        .bind(&child.id)
        .bind(instant_ms_dt(utc_now()))
        .execute(transaction.as_mut())
        .await?;
        if !parent.state.is_terminal() {
            let mut waiting_parent = parent.clone();
            waiting_parent.mark_waiting();
            upsert_job_rows(&mut transaction, &waiting_parent).await?;
        }
        transaction.commit().await?;
        if !child.kind.is_ephemeral() {
            let scope = child.scope();
            self.append_scope_event(
                &scope,
                serde_json::json!({
                    "event_kind": "job_created",
                    "kind": "job_created",
                    "job_id": child.id,
                    "job_kind": child.kind.as_str(),
                    "state": child.state.as_str()
                }),
            )
            .await?;
        }
        Ok(child)
    }

    pub async fn get_job(&self, job_id: &str) -> Result<Job> {
        let row = sqlx::query("SELECT payload_blob FROM job_payloads WHERE job_id = $1")
            .bind(job_id)
            .fetch_one(&self.pool)
            .await?;
        let payload: Vec<u8> = row.try_get("payload_blob")?;
        Job::decode(&payload)
    }

    pub async fn update_job(&self, job: &Job) -> Result<()> {
        let payload = job.touched();
        let mut transaction = self.pool.begin().await?;
        upsert_job_rows(&mut transaction, &payload).await?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn due_job_kinds(&self) -> Result<BTreeSet<crate::runtime::JobKind>> {
        let now_ms = instant_ms_dt(utc_now());
        let rows = sqlx::query(
            r#"
            SELECT DISTINCT kind
            FROM jobs
            WHERE state = 'queued'
              AND ready_at_ms <= $1
            "#,
        )
        .bind(now_ms)
        .fetch_all(&self.pool)
        .await?;
        let mut kinds = BTreeSet::new();
        for row in rows {
            let raw: String = row.try_get("kind")?;
            if let Ok(kind) = raw.parse::<crate::runtime::JobKind>() {
                kinds.insert(kind);
            }
        }
        Ok(kinds)
    }

    pub async fn next_queued_job_ready_at(&self) -> Result<Option<DateTime<Utc>>> {
        let row = sqlx::query(
            r#"
            SELECT ready_at_ms
            FROM jobs
            WHERE state = 'queued'
            ORDER BY ready_at_ms, created_at_ms, job_id
            LIMIT 1
            "#,
        )
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| {
            let ready_at_ms: i64 = row.try_get("ready_at_ms")?;
            ms_to_datetime(ready_at_ms)
                .ok_or_else(|| anyhow::anyhow!("queued job has invalid ready_at_ms"))
        })
        .transpose()
    }

    pub async fn next_queued_job_ready_after(
        &self,
        after: DateTime<Utc>,
    ) -> Result<Option<DateTime<Utc>>> {
        let row = sqlx::query(
            r#"
            SELECT ready_at_ms
            FROM jobs
            WHERE state = 'queued'
              AND ready_at_ms > $1
            ORDER BY ready_at_ms, created_at_ms, job_id
            LIMIT 1
            "#,
        )
        .bind(instant_ms_dt(after))
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| {
            let ready_at_ms: i64 = row.try_get("ready_at_ms")?;
            ms_to_datetime(ready_at_ms)
                .ok_or_else(|| anyhow::anyhow!("queued job has invalid ready_at_ms"))
        })
        .transpose()
    }

    pub async fn active_ordering_keys(&self) -> Result<BTreeSet<String>> {
        let rows = sqlx::query(
            r#"
            SELECT DISTINCT ordering_key
            FROM jobs
            WHERE ordering_key <> ''
              AND terminal = FALSE
              AND (
                state IN ('running', 'cancel_requested')
                OR (state = 'waiting' AND kind = 'agent_task')
              )
            "#,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| row.try_get::<String, _>("ordering_key"))
            .collect::<std::result::Result<BTreeSet<_>, _>>()?)
    }

    pub async fn claim_due_jobs(
        &self,
        kind: crate::runtime::JobKind,
        limit: usize,
        blocked_ordering_keys: &mut BTreeSet<String>,
    ) -> Result<Vec<Job>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let now_ms = instant_ms_dt(utc_now());
        let candidate_limit = limit.saturating_mul(8).clamp(limit, 512) as i64;
        let blocked = blocked_ordering_keys
            .iter()
            .filter(|key| !key.trim().is_empty())
            .cloned()
            .collect::<Vec<_>>();
        let mut transaction = self.pool.begin().await?;
        let rows = sqlx::query(
            r#"
            SELECT j.job_id, j.ordering_key, p.payload_blob
            FROM jobs j
            JOIN job_payloads p ON p.job_id = j.job_id
            WHERE j.state = 'queued'
              AND j.kind = $1
              AND j.ready_at_ms <= $2
              AND (cardinality($3::text[]) = 0 OR j.ordering_key = '' OR NOT (j.ordering_key = ANY($3)))
            ORDER BY j.ready_at_ms, j.created_at_ms, j.job_id
            LIMIT $4
            FOR UPDATE OF j SKIP LOCKED
            "#,
        )
        .bind(kind.as_str())
        .bind(now_ms)
        .bind(&blocked)
        .bind(candidate_limit)
        .fetch_all(transaction.as_mut())
        .await?;
        let mut claimed = Vec::new();
        for row in rows {
            if claimed.len() >= limit {
                break;
            }
            let ordering_key: String = row.try_get("ordering_key")?;
            if !ordering_key.trim().is_empty() && blocked_ordering_keys.contains(&ordering_key) {
                continue;
            }
            let payload_blob: Vec<u8> = row.try_get("payload_blob")?;
            let mut job = Job::decode(&payload_blob)?;
            job.mark_running();
            let payload = job.touched();
            let projection = project_job(&payload);
            let changed = sqlx::query(
                r#"
                UPDATE jobs
                SET state = $1,
                    updated_at_ms = $2,
                    ready_at_ms = $3,
                    started_at_ms = $4,
                    terminal = $5,
                    failed = $6,
                    cancellable = $7,
                    gc_after_ms = $8
                WHERE job_id = $9 AND state = 'queued'
                "#,
            )
            .bind(payload.state.as_str())
            .bind(projection.updated_at_ms)
            .bind(projection.ready_at_ms)
            .bind(projection.started_at_ms)
            .bind(projection.terminal)
            .bind(projection.failed)
            .bind(projection.cancellable)
            .bind(projection.gc_after_ms)
            .bind(&payload.id)
            .execute(transaction.as_mut())
            .await?
            .rows_affected();
            if changed == 1 {
                sqlx::query(
                    r#"
                    INSERT INTO job_payloads(job_id, payload_blob)
                    VALUES ($1, $2)
                    ON CONFLICT(job_id) DO UPDATE SET payload_blob = EXCLUDED.payload_blob
                    "#,
                )
                .bind(&payload.id)
                .bind(payload.encode()?)
                .execute(transaction.as_mut())
                .await?;
                if !ordering_key.trim().is_empty() {
                    blocked_ordering_keys.insert(ordering_key);
                }
                claimed.push(payload);
            }
        }
        transaction.commit().await?;
        Ok(claimed)
    }

    pub async fn replace_runtime_maintenance_job(&self, job: Job) -> Result<Job> {
        sqlx::query(
            r#"
            DELETE FROM jobs
            WHERE kind = $1
              AND terminal = FALSE
              AND ephemeral = TRUE
            "#,
        )
        .bind(crate::runtime::JobKind::RuntimeMaintenance.as_str())
        .execute(&self.pool)
        .await?;
        self.create_job(job).await
    }

    pub async fn list_jobs(
        &self,
        guild_id: Option<&str>,
        state: Option<crate::runtime::JobState>,
    ) -> Result<Vec<Job>> {
        self.list_jobs_with_visibility(guild_id, state, JobVisibility::Visible)
            .await
    }

    pub async fn list_jobs_with_visibility(
        &self,
        guild_id: Option<&str>,
        state: Option<crate::runtime::JobState>,
        visibility: JobVisibility,
    ) -> Result<Vec<Job>> {
        let mut query = QueryBuilder::<Postgres>::new(
            "SELECT p.payload_blob FROM jobs j JOIN job_payloads p ON p.job_id = j.job_id",
        );
        let mut has_where = false;
        if let Some(guild_id) = guild_id.filter(|value| !value.is_empty()) {
            push_filter_prefix(&mut query, &mut has_where);
            query.push("j.guild_id = ").push_bind(guild_id);
        }
        if let Some(state) = state {
            push_filter_prefix(&mut query, &mut has_where);
            query.push("j.state = ").push_bind(state.as_str());
        }
        push_visibility_filter(&mut query, &mut has_where, visibility);
        query.push(" ORDER BY j.created_at_ms DESC, j.job_id DESC");
        decode_job_rows(query.build().fetch_all(&self.pool).await?)
    }

    pub async fn list_jobs_by_scope_kind(
        &self,
        guild_id: &str,
        scope_id: &str,
        kind: crate::runtime::JobKind,
    ) -> Result<Vec<Job>> {
        let rows = sqlx::query(
            r#"
            SELECT p.payload_blob
            FROM jobs j
            JOIN job_payloads p ON p.job_id = j.job_id
            WHERE j.guild_id = $1
              AND j.scope_kind = 'voice_channel'
              AND j.scope_id = $2
              AND j.kind = $3
            ORDER BY j.updated_at_ms DESC, j.created_at_ms DESC, j.job_id
            "#,
        )
        .bind(guild_id)
        .bind(scope_id)
        .bind(kind.as_str())
        .fetch_all(&self.pool)
        .await?;
        decode_job_rows(rows)
    }

    pub async fn list_cancellable_jobs_for_scope(
        &self,
        guild_id: &str,
        scope_id: &str,
        limit: usize,
    ) -> Result<Vec<Job>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let rows = sqlx::query(
            r#"
            SELECT p.payload_blob
            FROM jobs j
            JOIN job_payloads p ON p.job_id = j.job_id
            WHERE j.guild_id = $1
              AND j.scope_kind = 'voice_channel'
              AND j.scope_id = $2
              AND j.terminal = FALSE
              AND j.ephemeral = FALSE
              AND j.cancellable = TRUE
            ORDER BY j.updated_at_ms DESC, j.created_at_ms DESC, j.job_id DESC
            LIMIT $3
            "#,
        )
        .bind(guild_id)
        .bind(scope_id)
        .bind(limit.clamp(1, 500) as i64)
        .fetch_all(&self.pool)
        .await?;
        decode_job_rows(rows)
    }

    pub async fn list_recent_agent_task_jobs_for_scope(
        &self,
        guild_id: &str,
        scope_id: &str,
        requester_user_id: &str,
        limit: usize,
    ) -> Result<Vec<Job>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = limit.clamp(1, 500);
        let requester = requester_user_id.trim();
        if requester.is_empty() {
            let rows = sqlx::query(
                r#"
                SELECT p.payload_blob
                FROM jobs j
                JOIN job_payloads p ON p.job_id = j.job_id
                WHERE j.guild_id = $1
                  AND j.scope_kind = 'voice_channel'
                  AND j.scope_id = $2
                  AND j.kind = 'agent_task'
                  AND j.ephemeral = FALSE
                  AND j.state IN (
                    'queued',
                    'running',
                    'waiting',
                    'cancel_requested',
                    'complete',
                    'failed',
                    'failed_timeout'
                  )
                ORDER BY j.updated_at_ms DESC, j.created_at_ms DESC, j.job_id DESC
                LIMIT $3
                "#,
            )
            .bind(guild_id)
            .bind(scope_id)
            .bind(limit as i64)
            .fetch_all(&self.pool)
            .await?;
            return decode_job_rows(rows);
        }
        let preferred_rows = sqlx::query(
            r#"
            SELECT p.payload_blob
            FROM jobs j
            JOIN job_payloads p ON p.job_id = j.job_id
            WHERE j.guild_id = $1
              AND j.scope_kind = 'voice_channel'
              AND j.scope_id = $2
              AND j.kind = 'agent_task'
              AND j.ephemeral = FALSE
              AND j.state IN (
                'queued',
                'running',
                'waiting',
                'cancel_requested',
                'complete',
                'failed',
                'failed_timeout'
              )
              AND j.requested_by_user_id = $3
            ORDER BY j.updated_at_ms DESC, j.created_at_ms DESC, j.job_id DESC
            LIMIT $4
            "#,
        )
        .bind(guild_id)
        .bind(scope_id)
        .bind(requester)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;
        let mut jobs = decode_job_rows(preferred_rows)?;
        if jobs.len() < limit {
            let remaining = (limit - jobs.len()) as i64;
            let other_rows = sqlx::query(
                r#"
                SELECT p.payload_blob
                FROM jobs j
                JOIN job_payloads p ON p.job_id = j.job_id
                WHERE j.guild_id = $1
                  AND j.scope_kind = 'voice_channel'
                  AND j.scope_id = $2
                  AND j.kind = 'agent_task'
                  AND j.ephemeral = FALSE
                  AND j.state IN (
                    'queued',
                    'running',
                    'waiting',
                    'cancel_requested',
                    'complete',
                    'failed',
                    'failed_timeout'
                  )
                  AND j.requested_by_user_id <> $3
                ORDER BY j.updated_at_ms DESC, j.created_at_ms DESC, j.job_id DESC
                LIMIT $4
                "#,
            )
            .bind(guild_id)
            .bind(scope_id)
            .bind(requester)
            .bind(remaining)
            .fetch_all(&self.pool)
            .await?;
            jobs.extend(decode_job_rows(other_rows)?);
        }
        Ok(jobs)
    }

    pub async fn list_active_jobs_by_scope_kind(
        &self,
        guild_id: &str,
        scope_id: &str,
        kind: crate::runtime::JobKind,
    ) -> Result<Vec<Job>> {
        let rows = sqlx::query(
            r#"
            SELECT p.payload_blob
            FROM jobs j
            JOIN job_payloads p ON p.job_id = j.job_id
            WHERE j.guild_id = $1
              AND j.scope_kind = 'voice_channel'
              AND j.scope_id = $2
              AND j.kind = $3
              AND j.terminal = FALSE
            ORDER BY j.updated_at_ms DESC, j.created_at_ms DESC, j.job_id
            "#,
        )
        .bind(guild_id)
        .bind(scope_id)
        .bind(kind.as_str())
        .fetch_all(&self.pool)
        .await?;
        decode_job_rows(rows)
    }

    pub async fn list_jobs_by_states(
        &self,
        guild_id: Option<&str>,
        states: &[crate::runtime::JobState],
    ) -> Result<Vec<Job>> {
        self.list_jobs_by_states_with_visibility(guild_id, states, JobVisibility::Visible)
            .await
    }

    pub async fn list_jobs_by_states_with_visibility(
        &self,
        guild_id: Option<&str>,
        states: &[crate::runtime::JobState],
        visibility: JobVisibility,
    ) -> Result<Vec<Job>> {
        if states.is_empty() {
            return Ok(Vec::new());
        }
        let mut query = QueryBuilder::<Postgres>::new(
            "SELECT p.payload_blob FROM jobs j JOIN job_payloads p ON p.job_id = j.job_id",
        );
        let mut has_where = false;
        if let Some(guild_id) = guild_id.filter(|value| !value.is_empty()) {
            push_filter_prefix(&mut query, &mut has_where);
            query.push("j.guild_id = ").push_bind(guild_id);
        }
        push_filter_prefix(&mut query, &mut has_where);
        query.push("j.state IN (");
        let mut separated = query.separated(", ");
        for state in states {
            separated.push_bind(state.as_str());
        }
        separated.push_unseparated(")");
        push_visibility_filter(&mut query, &mut has_where, visibility);
        query.push(" ORDER BY j.updated_at_ms DESC, j.created_at_ms DESC, j.job_id DESC");
        decode_job_rows(query.build().fetch_all(&self.pool).await?)
    }

    pub async fn list_recent_jobs(&self, guild_id: Option<&str>, limit: usize) -> Result<Vec<Job>> {
        self.list_recent_jobs_with_visibility(guild_id, limit, JobVisibility::Visible)
            .await
    }

    pub async fn list_jobs_updated_between(
        &self,
        since: DateTime<Utc>,
        until: DateTime<Utc>,
        limit: usize,
    ) -> Result<Vec<Job>> {
        let limit = limit.clamp(1, 500) as i64;
        let rows = sqlx::query(
            r#"
            SELECT p.payload_blob
            FROM jobs j
            JOIN job_payloads p ON p.job_id = j.job_id
            WHERE j.ephemeral = FALSE
              AND j.updated_at_ms >= $1
              AND j.updated_at_ms <= $2
            ORDER BY j.updated_at_ms DESC, j.created_at_ms DESC, j.job_id DESC
            LIMIT $3
            "#,
        )
        .bind(instant_ms_dt(since))
        .bind(instant_ms_dt(until))
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        decode_job_rows(rows)
    }

    pub async fn list_recent_jobs_with_visibility(
        &self,
        guild_id: Option<&str>,
        limit: usize,
        visibility: JobVisibility,
    ) -> Result<Vec<Job>> {
        let limit = limit.clamp(1, 500) as i64;
        let mut query = QueryBuilder::<Postgres>::new(
            "SELECT p.payload_blob FROM jobs j JOIN job_payloads p ON p.job_id = j.job_id",
        );
        let mut has_where = false;
        if let Some(guild_id) = guild_id.filter(|value| !value.is_empty()) {
            push_filter_prefix(&mut query, &mut has_where);
            query.push("j.guild_id = ").push_bind(guild_id);
        }
        push_visibility_filter(&mut query, &mut has_where, visibility);
        query.push(" ORDER BY j.updated_at_ms DESC, j.created_at_ms DESC, j.job_id DESC LIMIT ");
        query.push_bind(limit);
        decode_job_rows(query.build().fetch_all(&self.pool).await?)
    }

    pub async fn list_jobs_by_kind(
        &self,
        kind: crate::runtime::JobKind,
        limit: usize,
    ) -> Result<Vec<Job>> {
        self.list_jobs_by_kind_with_visibility(kind, limit, JobVisibility::Visible)
            .await
    }

    pub async fn list_jobs_by_kind_with_visibility(
        &self,
        kind: crate::runtime::JobKind,
        limit: usize,
        visibility: JobVisibility,
    ) -> Result<Vec<Job>> {
        let mut query = QueryBuilder::<Postgres>::new(
            r#"
            SELECT p.payload_blob
            FROM jobs j
            JOIN job_payloads p ON p.job_id = j.job_id
            WHERE j.kind =
            "#,
        );
        query.push_bind(kind.as_str());
        let mut has_where = true;
        push_visibility_filter(&mut query, &mut has_where, visibility);
        query.push(" ORDER BY j.updated_at_ms DESC, j.created_at_ms DESC, j.job_id DESC LIMIT ");
        query.push_bind(limit.clamp(1, 500) as i64);
        decode_job_rows(query.build().fetch_all(&self.pool).await?)
    }

    pub async fn list_jobs_for_trigger(
        &self,
        guild_id: &str,
        scope_id: &str,
        kinds: &[crate::runtime::JobKind],
        states: &[crate::runtime::JobState],
        updated_after: Option<DateTime<Utc>>,
    ) -> Result<Vec<Job>> {
        if guild_id.trim().is_empty()
            || scope_id.trim().is_empty()
            || kinds.is_empty()
            || states.is_empty()
        {
            return Ok(Vec::new());
        }
        let mut query = QueryBuilder::<Postgres>::new(
            "SELECT p.payload_blob FROM jobs j JOIN job_payloads p ON p.job_id = j.job_id WHERE j.scope_kind = 'voice_channel' AND j.guild_id = ",
        );
        query.push_bind(guild_id);
        query.push(" AND j.scope_id = ").push_bind(scope_id);
        query.push(" AND j.kind IN (");
        let mut kind_sep = query.separated(", ");
        for kind in kinds {
            kind_sep.push_bind(kind.as_str());
        }
        kind_sep.push_unseparated(") AND j.state IN (");
        let mut state_sep = query.separated(", ");
        for state in states {
            state_sep.push_bind(state.as_str());
        }
        state_sep.push_unseparated(")");
        if let Some(updated_after) = updated_after {
            query
                .push(" AND j.updated_at_ms > ")
                .push_bind(instant_ms_dt(updated_after));
        }
        query.push(" ORDER BY j.updated_at_ms, j.created_at_ms, j.job_id");
        decode_job_rows(query.build().fetch_all(&self.pool).await?)
    }

    pub async fn list_text_delivery_jobs_for_source(
        &self,
        source_job_id: &str,
    ) -> Result<Vec<Job>> {
        if source_job_id.trim().is_empty() {
            return Ok(Vec::new());
        }
        let rows = sqlx::query(
            r#"
            SELECT p.payload_blob
            FROM jobs j
            JOIN job_payloads p ON p.job_id = j.job_id
            WHERE j.kind = 'text_delivery'
              AND j.source_job_id = $1
            ORDER BY j.updated_at_ms DESC, j.job_id DESC
            "#,
        )
        .bind(source_job_id)
        .fetch_all(&self.pool)
        .await?;
        decode_job_rows(rows)
    }

    pub async fn garbage_collect_ephemeral_jobs(&self, limit: usize) -> Result<Value> {
        let limit = limit.clamp(1, 1000) as i64;
        let now_ms = instant_ms_dt(utc_now());
        let rows = sqlx::query(
            r#"
            WITH doomed AS (
              SELECT j.job_id
              FROM jobs j
              WHERE j.ephemeral = TRUE
                AND j.terminal = TRUE
                AND j.gc_after_ms IS NOT NULL
                AND j.gc_after_ms <= $1
                AND NOT EXISTS (
                  SELECT 1
                  FROM job_dependencies d
                  JOIN jobs parent ON parent.job_id = d.parent_job_id
                  WHERE d.child_job_id = j.job_id
                    AND parent.terminal = FALSE
                )
              ORDER BY j.gc_after_ms, j.job_id
              LIMIT $2
            )
            DELETE FROM jobs
            WHERE job_id IN (SELECT job_id FROM doomed)
            RETURNING job_id
            "#,
        )
        .bind(now_ms)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(serde_json::json!({"deleted": rows.len(), "limit": limit}))
    }

    pub async fn list_child_jobs(&self, parent_job_id: &str) -> Result<Vec<Job>> {
        let rows = sqlx::query(
            r#"
            SELECT p.payload_blob
            FROM job_dependencies d
            JOIN job_payloads p ON p.job_id = d.child_job_id
            JOIN jobs j ON j.job_id = d.child_job_id
            WHERE d.parent_job_id = $1
            ORDER BY d.created_at_ms, d.child_job_id
            "#,
        )
        .bind(parent_job_id)
        .fetch_all(&self.pool)
        .await?;
        decode_job_rows(rows)
    }

    pub async fn has_child_jobs(&self, parent_job_id: &str) -> Result<bool> {
        let row = sqlx::query(
            "SELECT EXISTS(SELECT 1 FROM job_dependencies WHERE parent_job_id = $1) AS exists",
        )
        .bind(parent_job_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.try_get::<bool, _>("exists")?)
    }

    pub async fn has_pending_audio_segment_for_speaker(
        &self,
        guild_id: &str,
        scope_id: &str,
        speaker_user_id: &str,
        segment_end_at_or_after: DateTime<Utc>,
    ) -> Result<bool> {
        let row = sqlx::query(
            r#"
            SELECT EXISTS(
              SELECT 1
              FROM jobs
              WHERE guild_id = $1
                AND scope_kind = 'voice_channel'
                AND scope_id = $2
                AND speaker_user_id = $3
                AND segment_end_ms >= $4
                AND kind = 'audio_segment'
                AND terminal = FALSE
                AND state IN ('queued', 'running', 'waiting', 'cancel_requested')
            ) AS exists
            "#,
        )
        .bind(guild_id)
        .bind(scope_id)
        .bind(speaker_user_id)
        .bind(instant_ms_dt(segment_end_at_or_after))
        .fetch_one(&self.pool)
        .await?;
        Ok(row.try_get::<bool, _>("exists")?)
    }

    pub async fn has_pending_audio_segment_for_speaker_until(
        &self,
        guild_id: &str,
        scope_id: &str,
        speaker_user_id: &str,
        segment_end_at_or_after: DateTime<Utc>,
        segment_end_at_or_before: DateTime<Utc>,
    ) -> Result<bool> {
        let row = sqlx::query(
            r#"
            SELECT EXISTS(
              SELECT 1
              FROM jobs
              WHERE guild_id = $1
                AND scope_kind = 'voice_channel'
                AND scope_id = $2
                AND speaker_user_id = $3
                AND segment_end_ms >= $4
                AND segment_end_ms <= $5
                AND kind = 'audio_segment'
                AND terminal = FALSE
                AND state IN ('queued', 'running', 'waiting', 'cancel_requested')
            ) AS exists
            "#,
        )
        .bind(guild_id)
        .bind(scope_id)
        .bind(speaker_user_id)
        .bind(instant_ms_dt(segment_end_at_or_after))
        .bind(instant_ms_dt(segment_end_at_or_before))
        .fetch_one(&self.pool)
        .await?;
        Ok(row.try_get::<bool, _>("exists")?)
    }

    pub async fn resolve_waiting_jobs(&self) -> Result<Vec<Value>> {
        let parent_rows = sqlx::query(
            r#"
            SELECT job_id, kind, state
            FROM jobs
            WHERE state = 'waiting'
            ORDER BY updated_at_ms, created_at_ms, job_id
            "#,
        )
        .fetch_all(&self.pool)
        .await?;
        let mut resolved = Vec::new();
        for row in parent_rows {
            let parent_summary = job_summary_from_row(&row)?;
            let children = self.list_child_job_summaries(&parent_summary.id).await?;
            if children.is_empty() || children.iter().any(|job| !job.state.is_terminal()) {
                continue;
            }
            let mut parent = self.get_job(&parent_summary.id).await?;
            if matches!(
                parent_summary.kind,
                crate::runtime::JobKind::RoomAgentPlacement
                    | crate::runtime::JobKind::DiscordVoicePlayback
                    | crate::runtime::JobKind::TextDelivery
                    | crate::runtime::JobKind::ConfirmationRequired
                    | crate::runtime::JobKind::AgentSessionStart
                    | crate::runtime::JobKind::AgentSessionResume
                    | crate::runtime::JobKind::AgentThreadTitleRefresh
                    | crate::runtime::JobKind::AgentTask
                    | crate::runtime::JobKind::TranscriptPublication
                    | crate::runtime::JobKind::VoiceStatusSync
                    | crate::runtime::JobKind::DiscordTypingIndicator
            ) {
                parent.set_state(crate::runtime::JobState::Queued);
                parent.next_run_at = None;
            } else if children
                .iter()
                .all(|job| job.state == crate::runtime::JobState::Complete)
            {
                parent.mark_complete();
            } else if children
                .iter()
                .any(|job| job.state == crate::runtime::JobState::Cancelled)
            {
                parent.mark_cancelled();
            } else {
                parent.set_state(
                    if parent_summary.kind == crate::runtime::JobKind::ConfirmationRequired {
                        crate::runtime::JobState::ApprovalFailed
                    } else {
                        crate::runtime::JobState::Failed
                    },
                );
                parent.metadata.error = children
                    .iter()
                    .filter(|job| job.state != crate::runtime::JobState::Complete)
                    .map(|job| format!("{} {}", job.id, job.state))
                    .collect::<Vec<_>>()
                    .join("; ");
            }
            self.update_job(&parent).await?;
            resolved.push(parent.to_value());
        }
        Ok(resolved)
    }

    async fn queued_wake_probes_for_stream(&self, stream_id: &str) -> Result<Vec<Job>> {
        let rows = sqlx::query(
            r#"
            SELECT p.payload_blob
            FROM jobs j
            JOIN job_payloads p ON p.job_id = j.job_id
            WHERE j.kind = 'wake_probe'
              AND j.state = 'queued'
              AND j.stream_id = $1
            ORDER BY j.ready_at_ms, j.created_at_ms, j.job_id
            "#,
        )
        .bind(stream_id)
        .fetch_all(&self.pool)
        .await?;
        let mut jobs = decode_job_rows(rows)?;
        sort_jobs_by_created_at(&mut jobs);
        Ok(jobs)
    }

    async fn list_wake_probe_jobs_stale_in_state(
        &self,
        state: crate::runtime::JobState,
        cutoff_ms: i64,
    ) -> Result<Vec<Job>> {
        let rows = sqlx::query(
            r#"
            SELECT p.payload_blob
            FROM jobs j
            JOIN job_payloads p ON p.job_id = j.job_id
            WHERE j.kind = 'wake_probe'
              AND j.state = $1
              AND j.updated_at_ms < $2
            ORDER BY j.updated_at_ms, j.job_id
            "#,
        )
        .bind(state.as_str())
        .bind(cutoff_ms)
        .fetch_all(&self.pool)
        .await?;
        decode_job_rows(rows)
    }

    async fn ensure_dependency_is_acyclic(
        &self,
        parent_job_id: &str,
        child_job_id: &str,
    ) -> Result<()> {
        if parent_job_id == child_job_id {
            anyhow::bail!("job dependency cycle rejected: job cannot depend on itself");
        }
        if self.job_depends_on(child_job_id, parent_job_id).await? {
            anyhow::bail!(
                "job dependency cycle rejected: {child_job_id} already depends on {parent_job_id}"
            );
        }
        Ok(())
    }

    async fn job_depends_on(&self, start_job_id: &str, target_job_id: &str) -> Result<bool> {
        let mut stack = vec![start_job_id.to_string()];
        let mut seen = BTreeSet::new();
        while let Some(job_id) = stack.pop() {
            if !seen.insert(job_id.clone()) {
                continue;
            }
            if job_id == target_job_id {
                return Ok(true);
            }
            stack.extend(self.list_child_job_ids(&job_id).await?);
        }
        Ok(false)
    }

    async fn list_child_job_summaries(&self, parent_job_id: &str) -> Result<Vec<JobSummary>> {
        let rows = sqlx::query(
            r#"
            SELECT j.job_id, j.kind, j.state
            FROM job_dependencies d
            JOIN jobs j ON j.job_id = d.child_job_id
            WHERE d.parent_job_id = $1
            ORDER BY d.created_at_ms, d.child_job_id
            "#,
        )
        .bind(parent_job_id)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(job_summary_from_row).collect()
    }

    async fn list_child_job_ids(&self, parent_job_id: &str) -> Result<Vec<String>> {
        let rows = sqlx::query(
            r#"
            SELECT child_job_id
            FROM job_dependencies
            WHERE parent_job_id = $1
            ORDER BY created_at_ms, child_job_id
            "#,
        )
        .bind(parent_job_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| row.try_get::<String, _>("child_job_id"))
            .collect::<std::result::Result<Vec<_>, _>>()?)
    }

    async fn ensure_job_voice_room(&self, job: &Job) -> Result<()> {
        if job.scope_kind == crate::runtime::RuntimeScopeKind::VoiceChannel {
            self.ensure_room(&job.guild_id, &job.scope_id, "", "", "")
                .await?;
        }
        Ok(())
    }
}

fn push_filter_prefix(query: &mut QueryBuilder<'_, Postgres>, has_where: &mut bool) {
    if *has_where {
        query.push(" AND ");
    } else {
        query.push(" WHERE ");
        *has_where = true;
    }
}

fn push_visibility_filter(
    query: &mut QueryBuilder<'_, Postgres>,
    has_where: &mut bool,
    visibility: JobVisibility,
) {
    match visibility {
        JobVisibility::Visible => {
            push_filter_prefix(query, has_where);
            query.push("j.ephemeral = FALSE");
        }
        JobVisibility::IncludeEphemeral => {}
        JobVisibility::OnlyEphemeral => {
            push_filter_prefix(query, has_where);
            query.push("j.ephemeral = TRUE");
        }
    }
}

fn decode_job_rows(rows: Vec<PgRow>) -> Result<Vec<Job>> {
    rows.into_iter()
        .map(|row| {
            let payload: Vec<u8> = row.try_get("payload_blob")?;
            Job::decode(&payload)
        })
        .collect()
}

fn job_summary_from_row(row: &PgRow) -> Result<JobSummary> {
    let kind = row
        .try_get::<String, _>("kind")?
        .parse::<crate::runtime::JobKind>()?;
    let state = row
        .try_get::<String, _>("state")?
        .parse::<crate::runtime::JobState>()?;
    Ok(JobSummary {
        id: row.try_get("job_id")?,
        kind,
        state,
    })
}

pub(crate) async fn upsert_job_rows(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    job: &Job,
) -> Result<()> {
    let projection = project_job(job);
    sqlx::query(
        r#"
        INSERT INTO jobs(
          job_id,
          scope_kind,
          guild_id,
          scope_id,
          kind,
          state,
          terminal,
          failed,
          ephemeral,
          cancellable,
          lane,
          ordering_key,
          ready_at_ms,
          created_at_ms,
          updated_at_ms,
          started_at_ms,
          completed_at_ms,
          gc_after_ms,
          root_job_id,
          parent_job_id,
          lineage_depth,
          requested_by_user_id,
          command_kind,
          source_job_id,
          stream_id,
          target_job_id,
          speaker_user_id,
          segment_end_ms
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21, $22, $23, $24, $25, $26, $27, $28)
        ON CONFLICT(job_id) DO UPDATE SET
          scope_kind = EXCLUDED.scope_kind,
          guild_id = EXCLUDED.guild_id,
          scope_id = EXCLUDED.scope_id,
          kind = EXCLUDED.kind,
          state = EXCLUDED.state,
          terminal = EXCLUDED.terminal,
          failed = EXCLUDED.failed,
          ephemeral = EXCLUDED.ephemeral,
          cancellable = EXCLUDED.cancellable,
          lane = EXCLUDED.lane,
          ordering_key = EXCLUDED.ordering_key,
          ready_at_ms = EXCLUDED.ready_at_ms,
          created_at_ms = EXCLUDED.created_at_ms,
          updated_at_ms = EXCLUDED.updated_at_ms,
          started_at_ms = EXCLUDED.started_at_ms,
          completed_at_ms = EXCLUDED.completed_at_ms,
          gc_after_ms = EXCLUDED.gc_after_ms,
          root_job_id = EXCLUDED.root_job_id,
          parent_job_id = EXCLUDED.parent_job_id,
          lineage_depth = EXCLUDED.lineage_depth,
          requested_by_user_id = EXCLUDED.requested_by_user_id,
          command_kind = EXCLUDED.command_kind,
          source_job_id = EXCLUDED.source_job_id,
          stream_id = EXCLUDED.stream_id,
          target_job_id = EXCLUDED.target_job_id,
          speaker_user_id = EXCLUDED.speaker_user_id,
          segment_end_ms = EXCLUDED.segment_end_ms
        "#,
    )
    .bind(&job.id)
    .bind(job.scope_kind.as_str())
    .bind(&job.guild_id)
    .bind(&job.scope_id)
    .bind(job.kind.as_str())
    .bind(job.state.as_str())
    .bind(projection.terminal)
    .bind(projection.failed)
    .bind(projection.ephemeral)
    .bind(projection.cancellable)
    .bind(projection.lane)
    .bind(&projection.ordering_key)
    .bind(projection.ready_at_ms)
    .bind(projection.created_at_ms)
    .bind(projection.updated_at_ms)
    .bind(projection.started_at_ms)
    .bind(projection.completed_at_ms)
    .bind(projection.gc_after_ms)
    .bind(&job.root_job_id)
    .bind(job.parent_job_id.as_deref())
    .bind(job.lineage_depth as i64)
    .bind(&job.requested_by_user_id)
    .bind(&projection.command_kind)
    .bind(&projection.source_job_id)
    .bind(&projection.stream_id)
    .bind(&projection.target_job_id)
    .bind(&projection.speaker_user_id)
    .bind(projection.segment_end_ms)
    .execute(transaction.as_mut())
    .await?;
    sqlx::query(
        r#"
        INSERT INTO job_payloads(job_id, payload_blob)
        VALUES ($1, $2)
        ON CONFLICT(job_id) DO UPDATE SET payload_blob = EXCLUDED.payload_blob
        "#,
    )
    .bind(&job.id)
    .bind(job.encode()?)
    .execute(transaction.as_mut())
    .await?;
    Ok(())
}

fn project_job(job: &Job) -> JobProjection {
    let created_at_ms = instant_ms_str(Some(&job.created_at)).unwrap_or(0);
    let updated_at_ms = instant_ms_str(Some(&job.updated_at)).unwrap_or(created_at_ms);
    let ready_at_ms = job
        .next_run_at
        .as_deref()
        .and_then(|value| instant_ms_str(Some(value)))
        .unwrap_or(created_at_ms);
    let terminal = job.state.is_terminal();
    let failed = is_failed_job_state(job.state);
    let ephemeral = job.kind.is_ephemeral();
    JobProjection {
        created_at_ms,
        updated_at_ms,
        ready_at_ms,
        started_at_ms: job
            .started_at
            .as_deref()
            .and_then(|value| instant_ms_str(Some(value))),
        completed_at_ms: job
            .completed_at
            .as_deref()
            .and_then(|value| instant_ms_str(Some(value))),
        gc_after_ms: ephemeral_gc_after_ms(job, updated_at_ms, terminal, failed),
        terminal,
        failed,
        ephemeral,
        cancellable: job.state.is_cancellable(),
        lane: job_lane(job.kind),
        ordering_key: job_ordering_key(job),
        command_kind: job.command_kind(),
        source_job_id: source_job_id(job),
        stream_id: wake_probe_stream_id(job).unwrap_or_default().to_string(),
        target_job_id: target_job_id(job),
        speaker_user_id: speaker_user_id(job),
        segment_end_ms: audio_segment_end_ms(job),
    }
}

fn wake_probe_stream_id(job: &Job) -> Option<&str> {
    match &job.payload {
        crate::runtime::JobPayload::WakeProbe(payload) => Some(payload.stream_id.as_str()),
        _ => None,
    }
}

fn sort_jobs_by_created_at(jobs: &mut [Job]) {
    jobs.sort_by(|left, right| {
        job_order_time(left)
            .cmp(&job_order_time(right))
            .then_with(|| left.id.cmp(&right.id))
    });
}

fn job_order_time(job: &Job) -> Option<DateTime<Utc>> {
    match &job.payload {
        crate::runtime::JobPayload::WakeProbe(payload) => Some(payload.probe_start_time),
        _ => parse_instant(&job.created_at),
    }
}

fn is_failed_job_state(state: crate::runtime::JobState) -> bool {
    matches!(
        state,
        crate::runtime::JobState::ApprovalFailed
            | crate::runtime::JobState::Failed
            | crate::runtime::JobState::FailedTimeout
            | crate::runtime::JobState::FailedDraftRetained
    )
}

fn ephemeral_gc_after_ms(
    job: &Job,
    updated_at_ms: i64,
    terminal: bool,
    failed: bool,
) -> Option<i64> {
    if !job.kind.is_ephemeral() || !terminal {
        return None;
    }
    let seconds = match (job.kind, failed) {
        (crate::runtime::JobKind::WakeProbe, false) => 60,
        (crate::runtime::JobKind::WakeProbe, true) => 300,
        (crate::runtime::JobKind::AudioSegment, false) => 300,
        (crate::runtime::JobKind::AudioSegment, true) => 1800,
        _ => 300,
    };
    Some(updated_at_ms.saturating_add(seconds * 1000))
}

fn job_lane(kind: crate::runtime::JobKind) -> &'static str {
    match kind {
        crate::runtime::JobKind::WakeProbe => "wake",
        crate::runtime::JobKind::AudioSegment => "audio",
        crate::runtime::JobKind::DiscordVoiceJoin
        | crate::runtime::JobKind::DiscordVoiceLeave
        | crate::runtime::JobKind::DiscordVoicePlayback
        | crate::runtime::JobKind::DiscordVoiceMute
        | crate::runtime::JobKind::DiscordVoiceDeafen
        | crate::runtime::JobKind::DiscordVoicePlayAudio => "voice_control",
        crate::runtime::JobKind::TextDelivery
        | crate::runtime::JobKind::ConfirmationRequired
        | crate::runtime::JobKind::AgentSessionStart
        | crate::runtime::JobKind::AgentSessionSunset
        | crate::runtime::JobKind::AgentSessionResume
        | crate::runtime::JobKind::TranscriptPublication => "general_async",
        crate::runtime::JobKind::DiscordTextSend
        | crate::runtime::JobKind::DiscordForumThreadCreate
        | crate::runtime::JobKind::DiscordForumThreadRename
        | crate::runtime::JobKind::DiscordTypingIndicator => "discord_text",
        crate::runtime::JobKind::RefineTranscript => "refinement",
        crate::runtime::JobKind::AgentTask | crate::runtime::JobKind::AgentThreadTitleRefresh => {
            "agent"
        }
        crate::runtime::JobKind::DiscordTextMessage => "general_async",
        crate::runtime::JobKind::DiscordSlashCommand => "general_async",
        crate::runtime::JobKind::RuntimeMaintenance
        | crate::runtime::JobKind::VoiceStatusSync
        | crate::runtime::JobKind::DiscordVoiceStatusSnapshot
        | crate::runtime::JobKind::AutomationEvaluation
        | crate::runtime::JobKind::AgentSessionRetirement
        | crate::runtime::JobKind::StaleWakeProbeSweep
        | crate::runtime::JobKind::StaleRunningJobSweep
        | crate::runtime::JobKind::EphemeralJobGc => "maintenance",
        _ => "general_async",
    }
}

fn job_ordering_key(job: &Job) -> String {
    match &job.payload {
        crate::runtime::JobPayload::WakeProbe(payload) => {
            format!("wake:stream:{}", payload.stream_id)
        }
        crate::runtime::JobPayload::AgentTask(payload) => {
            format!(
                "agent:session:{}",
                normalize_key_part(&payload.agent_session_id)
            )
        }
        crate::runtime::JobPayload::WakeActivation(payload) => {
            voice_agent_route_ordering_key(&payload.guild_id, &payload.voice_channel_id)
        }
        crate::runtime::JobPayload::Command(payload)
            if payload.command.command_kind == crate::runtime::CommandKind::AgentTask =>
        {
            voice_agent_route_ordering_key(&payload.command.guild_id, &payload.command.scope_id)
        }
        crate::runtime::JobPayload::DiscordTextMessage(payload) => {
            if payload.guild_id.trim().is_empty() {
                format!(
                    "agent:route:{}",
                    crate::runtime::dm_route_key(&payload.author_user_id)
                )
            } else {
                format!("discord:text:{}", normalize_key_part(&payload.channel_id))
            }
        }
        crate::runtime::JobPayload::DiscordSlashCommand(payload) => {
            if payload.guild_id.trim().is_empty() {
                format!("discord:slash:dm:{}", normalize_key_part(&payload.user_id))
            } else {
                format!(
                    "discord:slash:{}:{}",
                    normalize_key_part(&payload.guild_id),
                    normalize_key_part(&payload.channel_id)
                )
            }
        }
        crate::runtime::JobPayload::TextDelivery(payload) => {
            if payload.target.kind == crate::runtime::TextTargetKind::AgentSession {
                return format!(
                    "text:session_route:{}:{}",
                    normalize_key_part(&job.guild_id),
                    normalize_key_part(&job.scope_id)
                );
            }
            let target_id = if payload.target.kind == crate::runtime::TextTargetKind::Dm {
                payload.target.user_id.as_str()
            } else {
                payload.target.channel_id.as_str()
            };
            if payload.source_job_id.trim().is_empty() {
                format!(
                    "text:target:{}:{}",
                    payload.target.kind.as_str(),
                    normalize_key_part(target_id),
                )
            } else {
                format!("text:source:{}", normalize_key_part(&payload.source_job_id))
            }
        }
        crate::runtime::JobPayload::DiscordTextSend(payload) => {
            let target_id = if payload.target.kind == crate::runtime::TextTargetKind::Dm {
                payload.target.user_id.as_str()
            } else {
                payload.target.channel_id.as_str()
            };
            format!(
                "discord:text:{}:{}",
                payload.target.kind.as_str(),
                normalize_key_part(target_id)
            )
        }
        crate::runtime::JobPayload::DiscordForumThreadCreate(payload) => {
            format!(
                "discord:forum_thread:{}",
                normalize_key_part(&payload.parent_channel_id)
            )
        }
        crate::runtime::JobPayload::DiscordForumThreadRename(payload) => {
            format!("discord:thread:{}", normalize_key_part(&payload.thread_id))
        }
        crate::runtime::JobPayload::DiscordTypingIndicator(payload) => {
            if payload.target.kind == crate::runtime::TextTargetKind::AgentSession {
                return format!(
                    "discord:typing:source:{}",
                    normalize_key_part(&payload.source_job_id)
                );
            }
            let target_id = if payload.target.kind == crate::runtime::TextTargetKind::Dm {
                payload.target.user_id.as_str()
            } else {
                payload.target.channel_id.as_str()
            };
            format!(
                "discord:typing:{}:{}",
                payload.target.kind.as_str(),
                normalize_key_part(target_id)
            )
        }
        crate::runtime::JobPayload::ConfirmationRequired(payload) => {
            if payload.confirmation.delivery == "dm" {
                format!(
                    "discord:confirmation:dm:{}",
                    normalize_key_part(&payload.command.requested_by_user_id)
                )
            } else {
                format!(
                    "discord:confirmation:channel:{}",
                    normalize_key_part(&job.scope_id)
                )
            }
        }
        crate::runtime::JobPayload::AgentSessionStart(payload) => {
            voice_agent_route_ordering_key(&payload.guild_id, &payload.voice_channel_id)
        }
        crate::runtime::JobPayload::AgentSessionSunset(payload) => {
            format!(
                "agent:session:{}",
                normalize_key_part(&payload.agent_session_id)
            )
        }
        crate::runtime::JobPayload::AgentSessionResume(payload) => {
            if payload.route_kind == "dm" {
                format!(
                    "agent:route:{}",
                    crate::runtime::dm_route_key(&payload.dm_user_id)
                )
            } else {
                voice_agent_route_ordering_key(&payload.guild_id, &payload.voice_channel_id)
            }
        }
        crate::runtime::JobPayload::AgentThreadTitleRefresh(payload) => {
            format!(
                "agent:session:{}",
                normalize_key_part(&payload.agent_session_id)
            )
        }
        crate::runtime::JobPayload::TranscriptPublication(payload) => {
            format!(
                "publication:{}",
                normalize_key_part(&payload.publication_id)
            )
        }
        crate::runtime::JobPayload::RoomAgentPlacement(payload) => {
            let room_key = if payload.room_id.trim().is_empty() {
                job.scope_id.as_str()
            } else {
                payload.room_id.as_str()
            };
            format!(
                "room:placement:{}:{}",
                normalize_key_part(&job.guild_id),
                normalize_key_part(room_key)
            )
        }
        crate::runtime::JobPayload::DiscordVoiceJoin(payload) => {
            format!("voice:bot:{}", payload.bot_id)
        }
        crate::runtime::JobPayload::DiscordVoiceLeave(payload) => {
            format!("voice:session:{}", payload.session_id)
        }
        crate::runtime::JobPayload::DiscordVoicePlayback(payload) => {
            format!("voice:session:{}", payload.session_id)
        }
        crate::runtime::JobPayload::DiscordVoiceMute(payload) => {
            format!("voice:session:{}", payload.session_id)
        }
        crate::runtime::JobPayload::DiscordVoiceDeafen(payload) => {
            format!("voice:session:{}", payload.session_id)
        }
        crate::runtime::JobPayload::DiscordVoicePlayAudio(payload) => {
            format!("voice:session:{}", payload.session_id)
        }
        crate::runtime::JobPayload::RuntimeMaintenance(_) => "runtime:maintenance".to_string(),
        crate::runtime::JobPayload::VoiceStatusSync(_) => "runtime:maintenance".to_string(),
        crate::runtime::JobPayload::DiscordVoiceStatusSnapshot(_) => {
            "runtime:maintenance".to_string()
        }
        crate::runtime::JobPayload::AutomationEvaluation(_) => "runtime:maintenance".to_string(),
        crate::runtime::JobPayload::AgentSessionRetirement(_) => "runtime:maintenance".to_string(),
        crate::runtime::JobPayload::StaleWakeProbeSweep(_) => "runtime:maintenance".to_string(),
        crate::runtime::JobPayload::StaleRunningJobSweep(_) => "runtime:maintenance".to_string(),
        crate::runtime::JobPayload::EphemeralJobGc(_) => "runtime:maintenance".to_string(),
        _ => String::new(),
    }
}

fn voice_agent_route_ordering_key(guild_id: &str, voice_channel_id: &str) -> String {
    format!(
        "agent:route:{}",
        crate::runtime::voice_route_key(guild_id, voice_channel_id)
    )
}

fn source_job_id(job: &Job) -> String {
    match &job.payload {
        crate::runtime::JobPayload::TextDelivery(payload) => payload.source_job_id.clone(),
        crate::runtime::JobPayload::DiscordTextSend(payload) => payload.source_job_id.clone(),
        crate::runtime::JobPayload::DiscordForumThreadCreate(payload) => {
            payload.source_job_id.clone()
        }
        crate::runtime::JobPayload::DiscordForumThreadRename(payload) => {
            payload.source_job_id.clone()
        }
        crate::runtime::JobPayload::DiscordTypingIndicator(payload) => {
            payload.source_job_id.clone()
        }
        crate::runtime::JobPayload::DiscordVoicePlayback(payload) => payload.source_job_id.clone(),
        crate::runtime::JobPayload::DiscordVoiceMute(payload) => payload.source_job_id.clone(),
        crate::runtime::JobPayload::DiscordVoiceDeafen(payload) => payload.source_job_id.clone(),
        crate::runtime::JobPayload::DiscordVoicePlayAudio(payload) => payload.source_job_id.clone(),
        crate::runtime::JobPayload::VoiceStatusSync(payload) => payload.source_job_id.clone(),
        crate::runtime::JobPayload::DiscordVoiceStatusSnapshot(payload) => {
            payload.source_job_id.clone()
        }
        crate::runtime::JobPayload::AutomationEvaluation(payload) => payload.source_job_id.clone(),
        crate::runtime::JobPayload::AgentSessionRetirement(payload) => {
            payload.source_job_id.clone()
        }
        crate::runtime::JobPayload::AgentThreadTitleRefresh(payload) => {
            payload.source_job_id.clone()
        }
        crate::runtime::JobPayload::StaleWakeProbeSweep(payload) => payload.source_job_id.clone(),
        crate::runtime::JobPayload::StaleRunningJobSweep(payload) => payload.source_job_id.clone(),
        crate::runtime::JobPayload::EphemeralJobGc(payload) => payload.source_job_id.clone(),
        _ => String::new(),
    }
}

fn target_job_id(job: &Job) -> String {
    match &job.payload {
        crate::runtime::JobPayload::RuntimeControl(payload) => payload.target_job_id.clone(),
        crate::runtime::JobPayload::Command(payload) => payload.command.target_job_id.clone(),
        crate::runtime::JobPayload::AgentTask(payload) => payload.command.target_job_id.clone(),
        crate::runtime::JobPayload::ConfirmationRequired(payload) => {
            payload.command.target_job_id.clone()
        }
        _ => String::new(),
    }
}

fn speaker_user_id(job: &Job) -> String {
    match &job.payload {
        crate::runtime::JobPayload::AudioSegment(payload) => payload.speaker_user_id.clone(),
        crate::runtime::JobPayload::WakeActivation(payload) => payload.speaker_user_id.clone(),
        crate::runtime::JobPayload::WakeProbe(payload) => payload.speaker_user_id.clone(),
        _ => String::new(),
    }
}

fn audio_segment_end_ms(job: &Job) -> Option<i64> {
    match &job.payload {
        crate::runtime::JobPayload::AudioSegment(payload) => {
            Some(instant_ms_dt(payload.segment_end_time))
        }
        _ => None,
    }
}

fn normalize_key_part(value: &str) -> String {
    let normalized = value
        .trim()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    if normalized.is_empty() {
        "unknown".to_string()
    } else {
        normalized
    }
}
