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
}

impl TimelineStore {
    pub fn create_job(&self, job: Job) -> Result<Job> {
        let mut db = self.connect()?;
        self.ensure_room(&db, &job.guild_id, &job.voice_channel_id, "", "", "")?;
        let transaction = db.transaction()?;
        upsert_job_rows(&transaction, &job)?;
        transaction.commit()?;
        if !job.kind.is_ephemeral() {
            self.append_event(
                &job.guild_id,
                &job.voice_channel_id,
                serde_json::json!({"event_kind": "job_created", "kind": "job_created", "job_id": job.id, "job_kind": job.kind.as_str(), "state": job.state.as_str()}),
            )?;
        }
        Ok(job)
    }

    pub fn create_wake_probe_job(&self, job: Job) -> Result<Job> {
        self.create_job(job)
    }

    fn queued_wake_probes_for_stream(&self, stream_id: &str) -> Result<Vec<Job>> {
        let db = self.connect()?;
        let mut statement = db.prepare(
            r#"
            SELECT p.payload_blob
            FROM jobs j
            JOIN job_payloads p ON p.job_id = j.job_id
            WHERE j.kind = 'wake_probe'
              AND j.state = 'queued'
              AND j.stream_id = ?1
            ORDER BY j.ready_at_ms, j.created_at_ms, j.job_id
            "#,
        )?;
        let rows = statement.query_map(params![stream_id], |row| row.get::<_, Vec<u8>>(0))?;
        let mut matches = rows
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .map(|payload| Job::decode(&payload))
            .collect::<Result<Vec<_>>>()?;
        sort_jobs_by_created_at(&mut matches);
        Ok(matches)
    }

    pub fn cancel_queued_wake_probes_for_stream(&self, stream_id: &str) -> Result<Vec<Job>> {
        let mut cancelled = Vec::new();
        let queued = self.queued_wake_probes_for_stream(stream_id)?;
        for mut job in queued.into_iter().skip(1) {
            if job.kind != crate::runtime::JobKind::WakeProbe {
                continue;
            }
            job.mark_cancelled();
            job.metadata.error =
                "duplicate queued wake probe for the same speaker stream".to_string();
            self.update_job(&job)?;
            cancelled.push(job);
        }
        Ok(cancelled)
    }

    pub fn cancel_stale_wake_probe_jobs(&self, max_age_seconds: i64) -> Result<Vec<Value>> {
        let max_age = chrono::Duration::seconds(max_age_seconds.max(1));
        let now = utc_now();
        let mut cancelled = Vec::new();
        for state in [
            crate::runtime::JobState::Queued,
            crate::runtime::JobState::Running,
        ] {
            let cutoff_ms = instant_ms_dt(now - max_age);
            for mut job in self.list_wake_probe_jobs_stale_in_state(state, cutoff_ms)? {
                if state == crate::runtime::JobState::Running {
                    job.set_state(crate::runtime::JobState::FailedTimeout);
                    job.metadata.error = "stale wake probe exceeded queue age limit".to_string();
                    job.metadata.timed_out_at = isoformat_z(Some(now));
                } else {
                    job.mark_cancelled();
                    job.metadata.error = "stale queued wake probe was dropped".to_string();
                }
                self.update_job(&job)?;
                cancelled.push(job.to_value());
            }
        }
        Ok(cancelled)
    }

    fn list_wake_probe_jobs_stale_in_state(
        &self,
        state: crate::runtime::JobState,
        cutoff_ms: i64,
    ) -> Result<Vec<Job>> {
        let db = self.connect()?;
        let mut statement = db.prepare(
            r#"
            SELECT p.payload_blob
            FROM jobs j
            JOIN job_payloads p ON p.job_id = j.job_id
            WHERE j.kind = 'wake_probe'
              AND j.state = ?1
              AND j.updated_at_ms < ?2
            ORDER BY j.updated_at_ms, j.job_id
            "#,
        )?;
        let rows = statement.query_map(params![state.as_str(), cutoff_ms], |row| {
            row.get::<_, Vec<u8>>(0)
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .map(|payload| Job::decode(&payload))
            .collect()
    }

    pub fn create_child_job(&self, parent: &Job, mut child: Job) -> Result<Job> {
        child.attach_to_parent(parent)?;
        self.ensure_dependency_is_acyclic(&parent.id, &child.id)?;
        let child = self.create_job(child)?;
        self.create_job_dependency(&parent.id, &child.id)?;
        let mut waiting_parent = parent.clone();
        if !waiting_parent.state.is_terminal() {
            waiting_parent.mark_waiting();
            self.update_job(&waiting_parent)?;
        }
        Ok(child)
    }

    fn create_job_dependency(&self, parent_job_id: &str, child_job_id: &str) -> Result<()> {
        let db = self.connect()?;
        db.execute(
            r#"
            INSERT INTO job_dependencies(parent_job_id, child_job_id, dependency_kind, created_at_ms, resolution_policy)
            VALUES (?1, ?2, 'required', ?3, 'parent_resumes')
            ON CONFLICT(parent_job_id, child_job_id) DO NOTHING
            "#,
            params![parent_job_id, child_job_id, instant_ms_dt(utc_now())],
        )?;
        Ok(())
    }

    fn ensure_dependency_is_acyclic(&self, parent_job_id: &str, child_job_id: &str) -> Result<()> {
        if parent_job_id == child_job_id {
            anyhow::bail!("job dependency cycle rejected: job cannot depend on itself");
        }
        if self.job_depends_on(child_job_id, parent_job_id)? {
            anyhow::bail!(
                "job dependency cycle rejected: {child_job_id} already depends on {parent_job_id}"
            );
        }
        Ok(())
    }

    fn job_depends_on(&self, start_job_id: &str, target_job_id: &str) -> Result<bool> {
        let mut stack = vec![start_job_id.to_string()];
        let mut seen = BTreeSet::new();
        while let Some(job_id) = stack.pop() {
            if !seen.insert(job_id.clone()) {
                continue;
            }
            if job_id == target_job_id {
                return Ok(true);
            }
            stack.extend(self.list_child_job_ids(&job_id)?);
        }
        Ok(false)
    }

    pub fn get_job(&self, job_id: &str) -> Result<Job> {
        let db = self.connect()?;
        let payload = db.query_row(
            "SELECT payload_blob FROM job_payloads WHERE job_id = ?1",
            params![job_id],
            |row| row.get::<_, Vec<u8>>(0),
        )?;
        Job::decode(&payload)
    }

    pub fn update_job(&self, job: &Job) -> Result<()> {
        if job.id.is_empty() || job.guild_id.is_empty() || job.voice_channel_id.is_empty() {
            return Ok(());
        }
        let payload = job.touched();
        let mut db = self.connect()?;
        let transaction = db.transaction()?;
        upsert_job_rows(&transaction, &payload)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn claim_due_jobs(
        &self,
        kind: crate::runtime::JobKind,
        limit: usize,
        blocked_ordering_keys: &mut BTreeSet<String>,
    ) -> Result<Vec<Job>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let now_ms = instant_ms_dt(utc_now());
        let candidate_limit = limit.saturating_mul(8).clamp(limit, 512);
        let db = self.connect()?;
        let mut conditions = vec![
            "j.state = 'queued'".to_string(),
            "j.kind = ?".to_string(),
            "j.ready_at_ms <= ?".to_string(),
        ];
        let mut params_values: Vec<Box<dyn ToSql>> =
            vec![Box::new(kind.as_str().to_string()), Box::new(now_ms)];
        let blocked = blocked_ordering_keys
            .iter()
            .filter(|key| !key.trim().is_empty())
            .cloned()
            .collect::<Vec<_>>();
        if !blocked.is_empty() {
            conditions.push(format!(
                "(j.ordering_key = '' OR j.ordering_key NOT IN ({}))",
                placeholders(blocked.len())
            ));
            for key in blocked {
                params_values.push(Box::new(key));
            }
        }
        params_values.push(Box::new(candidate_limit as i64));
        let sql = format!(
            r#"
            SELECT j.ordering_key, p.payload_blob
            FROM jobs j
            JOIN job_payloads p ON p.job_id = j.job_id
            WHERE {}
            ORDER BY j.ready_at_ms, j.created_at_ms, j.job_id
            LIMIT ?
            "#,
            conditions.join(" AND ")
        );
        let mut statement = db.prepare(&sql)?;
        let rows = statement.query_map(
            params_from_iter(params_values.iter().map(|value| &**value)),
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?)),
        )?;
        let mut candidates = Vec::new();
        for (ordering_key, payload) in rows.collect::<rusqlite::Result<Vec<_>>>()? {
            if !ordering_key.trim().is_empty() && blocked_ordering_keys.contains(&ordering_key) {
                continue;
            }
            let job = Job::decode(&payload)?;
            if job.id.trim().is_empty() {
                continue;
            }
            if !ordering_key.trim().is_empty() {
                blocked_ordering_keys.insert(ordering_key);
            }
            candidates.push(job);
            if candidates.len() >= limit {
                break;
            }
        }
        if candidates.is_empty() {
            return Ok(Vec::new());
        }

        drop(statement);
        drop(db);
        let mut db = self.connect()?;
        let transaction = db.transaction()?;
        let mut claimed = Vec::new();
        for mut job in candidates {
            if claimed.len() >= limit {
                break;
            }
            job.mark_running();
            let payload = job.touched();
            let projection = project_job(&payload);
            let changed = transaction.execute(
                r#"
                UPDATE jobs
                SET state = ?1,
                    updated_at_ms = ?2,
                    ready_at_ms = ?3,
                    started_at_ms = ?4,
                    terminal = ?5,
                    failed = ?6,
                    cancellable = ?7,
                    gc_after_ms = ?8
                WHERE job_id = ?9 AND state = ?10
                "#,
                params![
                    payload.state.as_str(),
                    projection.updated_at_ms,
                    projection.ready_at_ms,
                    projection.started_at_ms,
                    bool_int(projection.terminal),
                    bool_int(projection.failed),
                    bool_int(projection.cancellable),
                    projection.gc_after_ms,
                    &payload.id,
                    crate::runtime::JobState::Queued.as_str(),
                ],
            )?;
            if changed == 1 {
                transaction.execute(
                    r#"
                    INSERT INTO job_payloads(job_id, payload_blob)
                    VALUES (?1, ?2)
                    ON CONFLICT(job_id) DO UPDATE SET payload_blob = excluded.payload_blob
                    "#,
                    params![&payload.id, payload.encode()?],
                )?;
                claimed.push(payload);
            }
        }
        transaction.commit()?;
        Ok(claimed)
    }

    pub fn due_job_kinds(&self) -> Result<BTreeSet<crate::runtime::JobKind>> {
        let now_ms = instant_ms_dt(utc_now());
        let db = self.connect()?;
        let mut statement = db.prepare(
            r#"
            SELECT DISTINCT kind
            FROM jobs
            WHERE state = 'queued'
              AND ready_at_ms <= ?1
            "#,
        )?;
        let rows = statement.query_map(params![now_ms], |row| row.get::<_, String>(0))?;
        let mut kinds = BTreeSet::new();
        for raw in rows.collect::<rusqlite::Result<Vec<_>>>()? {
            if let Ok(kind) = raw.parse::<crate::runtime::JobKind>() {
                kinds.insert(kind);
            }
        }
        Ok(kinds)
    }

    pub fn resolve_waiting_jobs(&self) -> Result<Vec<Value>> {
        let mut resolved = Vec::new();
        for mut parent in self.list_jobs_with_visibility(
            None,
            Some(crate::runtime::JobState::Waiting),
            JobVisibility::IncludeEphemeral,
        )? {
            let children = self.list_child_jobs(&parent.id)?;
            if children.is_empty() || children.iter().any(|job| !job.state.is_terminal()) {
                continue;
            }
            if matches!(
                parent.kind,
                crate::runtime::JobKind::RoomAgentPlacement
                    | crate::runtime::JobKind::DiscordVoicePlayback
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
                    if parent.kind == crate::runtime::JobKind::ConfirmationRequired {
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
            self.update_job(&parent)?;
            resolved.push(parent.to_value());
        }
        Ok(resolved)
    }

    pub fn list_jobs(
        &self,
        guild_id: Option<&str>,
        state: Option<crate::runtime::JobState>,
    ) -> Result<Vec<Job>> {
        self.list_jobs_with_visibility(guild_id, state, JobVisibility::Visible)
    }

    pub fn list_jobs_with_visibility(
        &self,
        guild_id: Option<&str>,
        state: Option<crate::runtime::JobState>,
        visibility: JobVisibility,
    ) -> Result<Vec<Job>> {
        let mut conditions = Vec::new();
        let mut params_values: Vec<Box<dyn ToSql>> = Vec::new();
        if let Some(guild_id) = guild_id.filter(|value| !value.is_empty()) {
            conditions.push("j.guild_id = ?".to_string());
            params_values.push(Box::new(guild_id.to_string()));
        }
        if let Some(state) = state {
            conditions.push("j.state = ?".to_string());
            params_values.push(Box::new(state.as_str().to_string()));
        }
        push_visibility_condition(&mut conditions, visibility);
        let where_clause = if conditions.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", conditions.join(" AND "))
        };
        let sql = format!(
            "SELECT p.payload_blob FROM jobs j JOIN job_payloads p ON p.job_id = j.job_id{where_clause} ORDER BY j.created_at_ms DESC, j.job_id DESC"
        );
        let db = self.connect()?;
        let mut statement = db.prepare(&sql)?;
        let rows = statement.query_map(
            params_from_iter(params_values.iter().map(|value| &**value)),
            |row| row.get::<_, Vec<u8>>(0),
        )?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .map(|payload| Job::decode(&payload))
            .collect()
    }

    pub fn list_jobs_by_scope_kind(
        &self,
        guild_id: &str,
        voice_channel_id: &str,
        kind: crate::runtime::JobKind,
    ) -> Result<Vec<Job>> {
        let db = self.connect()?;
        let mut statement = db.prepare(
            r#"
            SELECT p.payload_blob
            FROM jobs j
            JOIN job_payloads p ON p.job_id = j.job_id
            WHERE j.guild_id = ?1
              AND j.voice_channel_id = ?2
              AND j.kind = ?3
            ORDER BY j.updated_at_ms DESC, j.created_at_ms DESC, j.job_id
            "#,
        )?;
        let rows = statement
            .query_map(params![guild_id, voice_channel_id, kind.as_str()], |row| {
                row.get::<_, Vec<u8>>(0)
            })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .map(|payload| Job::decode(&payload))
            .collect()
    }

    pub fn list_jobs_by_states(
        &self,
        guild_id: Option<&str>,
        states: &[crate::runtime::JobState],
    ) -> Result<Vec<Job>> {
        self.list_jobs_by_states_with_visibility(guild_id, states, JobVisibility::Visible)
    }

    pub fn list_jobs_by_states_with_visibility(
        &self,
        guild_id: Option<&str>,
        states: &[crate::runtime::JobState],
        visibility: JobVisibility,
    ) -> Result<Vec<Job>> {
        if states.is_empty() {
            return Ok(Vec::new());
        }
        let mut conditions = Vec::new();
        let mut params_values: Vec<Box<dyn ToSql>> = Vec::new();
        if let Some(guild_id) = guild_id.filter(|value| !value.is_empty()) {
            conditions.push("j.guild_id = ?".to_string());
            params_values.push(Box::new(guild_id.to_string()));
        }
        conditions.push(format!("j.state IN ({})", placeholders(states.len())));
        for state in states {
            params_values.push(Box::new(state.as_str().to_string()));
        }
        push_visibility_condition(&mut conditions, visibility);
        let sql = format!(
            "SELECT p.payload_blob FROM jobs j JOIN job_payloads p ON p.job_id = j.job_id WHERE {} ORDER BY j.updated_at_ms DESC, j.created_at_ms DESC, j.job_id DESC",
            conditions.join(" AND ")
        );
        let db = self.connect()?;
        let mut statement = db.prepare(&sql)?;
        let rows = statement.query_map(
            params_from_iter(params_values.iter().map(|value| &**value)),
            |row| row.get::<_, Vec<u8>>(0),
        )?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .map(|payload| Job::decode(&payload))
            .collect()
    }

    pub fn list_recent_jobs(&self, guild_id: Option<&str>, limit: usize) -> Result<Vec<Job>> {
        self.list_recent_jobs_with_visibility(guild_id, limit, JobVisibility::Visible)
    }

    pub fn list_recent_jobs_with_visibility(
        &self,
        guild_id: Option<&str>,
        limit: usize,
        visibility: JobVisibility,
    ) -> Result<Vec<Job>> {
        let limit = limit.clamp(1, 500);
        let mut conditions = Vec::new();
        let mut params_values: Vec<Box<dyn ToSql>> = Vec::new();
        if let Some(guild_id) = guild_id.filter(|value| !value.is_empty()) {
            conditions.push("j.guild_id = ?".to_string());
            params_values.push(Box::new(guild_id.to_string()));
        }
        push_visibility_condition(&mut conditions, visibility);
        params_values.push(Box::new(limit as i64));
        let where_clause = if conditions.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", conditions.join(" AND "))
        };
        let sql = format!(
            "SELECT p.payload_blob FROM jobs j JOIN job_payloads p ON p.job_id = j.job_id{where_clause} ORDER BY j.updated_at_ms DESC, j.created_at_ms DESC, j.job_id DESC LIMIT ?"
        );
        let db = self.connect()?;
        let mut statement = db.prepare(&sql)?;
        let rows = statement.query_map(
            params_from_iter(params_values.iter().map(|value| &**value)),
            |row| row.get::<_, Vec<u8>>(0),
        )?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .map(|payload| Job::decode(&payload))
            .collect()
    }

    pub fn list_jobs_by_kind(
        &self,
        kind: crate::runtime::JobKind,
        limit: usize,
    ) -> Result<Vec<Job>> {
        let limit = limit.clamp(1, 500);
        let db = self.connect()?;
        let mut statement = db.prepare(
            r#"
            SELECT p.payload_blob
            FROM jobs j
            JOIN job_payloads p ON p.job_id = j.job_id
            WHERE j.kind = ?1
            ORDER BY j.updated_at_ms DESC, j.created_at_ms DESC, j.job_id DESC
            LIMIT ?2
            "#,
        )?;
        let rows = statement.query_map(params![kind.as_str(), limit as i64], |row| {
            row.get::<_, Vec<u8>>(0)
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .map(|payload| Job::decode(&payload))
            .collect()
    }

    pub fn list_jobs_for_trigger(
        &self,
        guild_id: &str,
        voice_channel_id: &str,
        kinds: &[crate::runtime::JobKind],
        states: &[crate::runtime::JobState],
        updated_after: Option<DateTime<Utc>>,
    ) -> Result<Vec<Job>> {
        if guild_id.trim().is_empty()
            || voice_channel_id.trim().is_empty()
            || kinds.is_empty()
            || states.is_empty()
        {
            return Ok(Vec::new());
        }
        let mut conditions = vec![
            "j.guild_id = ?".to_string(),
            "j.voice_channel_id = ?".to_string(),
            format!("j.kind IN ({})", placeholders(kinds.len())),
            format!("j.state IN ({})", placeholders(states.len())),
        ];
        let mut params_values: Vec<Box<dyn ToSql>> = vec![
            Box::new(guild_id.to_string()),
            Box::new(voice_channel_id.to_string()),
        ];
        for kind in kinds {
            params_values.push(Box::new(kind.as_str().to_string()));
        }
        for state in states {
            params_values.push(Box::new(state.as_str().to_string()));
        }
        if let Some(updated_after) = updated_after {
            conditions.push("j.updated_at_ms > ?".to_string());
            params_values.push(Box::new(instant_ms_dt(updated_after)));
        }
        let sql = format!(
            "SELECT p.payload_blob FROM jobs j JOIN job_payloads p ON p.job_id = j.job_id WHERE {} ORDER BY j.updated_at_ms, j.created_at_ms, j.job_id",
            conditions.join(" AND ")
        );
        let db = self.connect()?;
        let mut statement = db.prepare(&sql)?;
        let rows = statement.query_map(
            params_from_iter(params_values.iter().map(|value| &**value)),
            |row| row.get::<_, Vec<u8>>(0),
        )?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .map(|payload| Job::decode(&payload))
            .collect()
    }

    pub fn list_response_jobs_for_source(&self, source_job_id: &str) -> Result<Vec<Job>> {
        if source_job_id.trim().is_empty() {
            return Ok(Vec::new());
        }
        let db = self.connect()?;
        let mut statement = db.prepare(
            r#"
            SELECT p.payload_blob
            FROM jobs j
            JOIN job_payloads p ON p.job_id = j.job_id
            WHERE j.kind = 'response'
              AND j.source_job_id = ?1
            ORDER BY j.updated_at_ms DESC, j.job_id DESC
            "#,
        )?;
        let rows = statement.query_map(params![source_job_id], |row| row.get::<_, Vec<u8>>(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .map(|payload| Job::decode(&payload))
            .collect()
    }

    pub fn garbage_collect_ephemeral_jobs(&self, limit: usize) -> Result<Value> {
        let limit = limit.clamp(1, 1000);
        let now_ms = instant_ms_dt(utc_now());
        match self.garbage_collect_ephemeral_jobs_inner(now_ms, limit) {
            Ok(value) => Ok(value),
            Err(error) if sqlite_busy_or_locked(&error) => {
                Ok(serde_json::json!({"deleted": 0, "limit": limit, "skipped": "sqlite_busy"}))
            }
            Err(error) => Err(error.into()),
        }
    }

    fn garbage_collect_ephemeral_jobs_inner(
        &self,
        now_ms: i64,
        limit: usize,
    ) -> rusqlite::Result<Value> {
        let mut db = self
            .connect_with_busy_timeout(1)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(error.into()))?;
        let job_ids = {
            let mut statement = db.prepare(
                r#"
                SELECT j.job_id
                FROM jobs j
                WHERE j.ephemeral = 1
                  AND j.terminal = 1
                  AND j.gc_after_ms IS NOT NULL
                  AND j.gc_after_ms <= ?1
                  AND NOT EXISTS (
                    SELECT 1
                    FROM job_dependencies d
                    JOIN jobs parent ON parent.job_id = d.parent_job_id
                    WHERE d.child_job_id = j.job_id
                      AND parent.terminal = 0
                  )
                ORDER BY j.gc_after_ms, j.job_id
                LIMIT ?2
                "#,
            )?;
            let rows = statement
                .query_map(params![now_ms, limit as i64], |row| row.get::<_, String>(0))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        if job_ids.is_empty() {
            return Ok(serde_json::json!({"deleted": 0, "limit": limit}));
        }
        let transaction = db.transaction()?;
        let placeholders = placeholders(job_ids.len());
        transaction.execute(
            &format!("DELETE FROM jobs WHERE job_id IN ({placeholders})"),
            params_from_iter(job_ids.iter()),
        )?;
        transaction.commit()?;
        Ok(serde_json::json!({"deleted": job_ids.len(), "limit": limit}))
    }

    pub fn list_child_jobs(&self, parent_job_id: &str) -> Result<Vec<Job>> {
        let child_ids = self.list_child_job_ids(parent_job_id)?;
        let mut children = Vec::new();
        for child_id in child_ids {
            if let Ok(child) = self.get_job(&child_id) {
                children.push(child);
            }
        }
        children.sort_by(|left, right| left.created_at.cmp(&right.created_at));
        Ok(children)
    }

    fn list_child_job_ids(&self, parent_job_id: &str) -> Result<Vec<String>> {
        let db = self.connect()?;
        let mut statement = db.prepare(
            "SELECT child_job_id FROM job_dependencies WHERE parent_job_id = ?1 ORDER BY created_at_ms, child_job_id",
        )?;
        let rows = statement.query_map(params![parent_job_id], |row| row.get::<_, String>(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

fn wake_probe_stream_id(job: &Job) -> Option<&str> {
    match &job.payload {
        crate::runtime::JobPayload::WakeProbe(payload) => Some(payload.stream_id.as_str()),
        _ => None,
    }
}

fn placeholders(count: usize) -> String {
    (0..count).map(|_| "?").collect::<Vec<_>>().join(", ")
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

fn upsert_job_rows(transaction: &rusqlite::Transaction<'_>, job: &Job) -> Result<()> {
    let projection = project_job(job);
    transaction.execute(
        r#"
        INSERT INTO jobs(
          job_id,
          guild_id,
          voice_channel_id,
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
          target_job_id
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25)
        ON CONFLICT(job_id) DO UPDATE SET
          guild_id = excluded.guild_id,
          voice_channel_id = excluded.voice_channel_id,
          kind = excluded.kind,
          state = excluded.state,
          terminal = excluded.terminal,
          failed = excluded.failed,
          ephemeral = excluded.ephemeral,
          cancellable = excluded.cancellable,
          lane = excluded.lane,
          ordering_key = excluded.ordering_key,
          ready_at_ms = excluded.ready_at_ms,
          created_at_ms = excluded.created_at_ms,
          updated_at_ms = excluded.updated_at_ms,
          started_at_ms = excluded.started_at_ms,
          completed_at_ms = excluded.completed_at_ms,
          gc_after_ms = excluded.gc_after_ms,
          root_job_id = excluded.root_job_id,
          parent_job_id = excluded.parent_job_id,
          lineage_depth = excluded.lineage_depth,
          requested_by_user_id = excluded.requested_by_user_id,
          command_kind = excluded.command_kind,
          source_job_id = excluded.source_job_id,
          stream_id = excluded.stream_id,
          target_job_id = excluded.target_job_id
        "#,
        params![
            &job.id,
            &job.guild_id,
            &job.voice_channel_id,
            job.kind.as_str(),
            job.state.as_str(),
            bool_int(projection.terminal),
            bool_int(projection.failed),
            bool_int(projection.ephemeral),
            bool_int(projection.cancellable),
            projection.lane,
            &projection.ordering_key,
            projection.ready_at_ms,
            projection.created_at_ms,
            projection.updated_at_ms,
            projection.started_at_ms,
            projection.completed_at_ms,
            projection.gc_after_ms,
            &job.root_job_id,
            job.parent_job_id.as_deref(),
            job.lineage_depth as i64,
            &job.requested_by_user_id,
            &projection.command_kind,
            &projection.source_job_id,
            &projection.stream_id,
            &projection.target_job_id,
        ],
    )?;
    transaction.execute(
        r#"
        INSERT INTO job_payloads(job_id, payload_blob)
        VALUES (?1, ?2)
        ON CONFLICT(job_id) DO UPDATE SET payload_blob = excluded.payload_blob
        "#,
        params![&job.id, job.encode()?],
    )?;
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
    }
}

fn push_visibility_condition(conditions: &mut Vec<String>, visibility: JobVisibility) {
    match visibility {
        JobVisibility::Visible => conditions.push("j.ephemeral = 0".to_string()),
        JobVisibility::IncludeEphemeral => {}
        JobVisibility::OnlyEphemeral => conditions.push("j.ephemeral = 1".to_string()),
    }
}

fn bool_int(value: bool) -> i64 {
    if value { 1 } else { 0 }
}

fn is_failed_job_state(state: crate::runtime::JobState) -> bool {
    matches!(
        state,
        crate::runtime::JobState::ApprovalFailed
            | crate::runtime::JobState::Failed
            | crate::runtime::JobState::FailedTimeout
            | crate::runtime::JobState::AgentDispatchFailed
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
        | crate::runtime::JobKind::DiscordVoicePlayAudio => "voice_control",
        crate::runtime::JobKind::Response => "response",
        crate::runtime::JobKind::RefineTranscript => "refinement",
        crate::runtime::JobKind::AgentTask => "agent",
        _ => "general_async",
    }
}

fn job_ordering_key(job: &Job) -> String {
    match &job.payload {
        crate::runtime::JobPayload::WakeProbe(payload) => {
            format!("wake:stream:{}", payload.stream_id)
        }
        crate::runtime::JobPayload::AgentTask(_) => {
            format!(
                "agent:task:{}:{}",
                normalize_key_part(&job.guild_id),
                normalize_key_part(&job.voice_channel_id)
            )
        }
        crate::runtime::JobPayload::RoomAgentPlacement(payload) => {
            let room_key = if payload.room_id.trim().is_empty() {
                job.voice_channel_id.as_str()
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
        crate::runtime::JobPayload::DiscordVoicePlayAudio(payload) => {
            format!("voice:session:{}", payload.session_id)
        }
        _ => String::new(),
    }
}

fn source_job_id(job: &Job) -> String {
    match &job.payload {
        crate::runtime::JobPayload::Response(payload) => payload.source_job_id.clone(),
        crate::runtime::JobPayload::DiscordVoicePlayback(payload) => payload.source_job_id.clone(),
        crate::runtime::JobPayload::DiscordVoiceMute(payload) => payload.source_job_id.clone(),
        crate::runtime::JobPayload::DiscordVoicePlayAudio(payload) => payload.source_job_id.clone(),
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

fn sqlite_busy_or_locked(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(failure, _)
            if matches!(
                failure.code,
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
            )
    )
}
