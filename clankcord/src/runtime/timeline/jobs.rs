use super::*;

impl TimelineStore {
    pub fn create_job(&self, job: Job) -> Result<Job> {
        let created_ms = instant_ms_str(Some(&job.created_at)).unwrap_or(0);
        let next_run_at_ms = job
            .next_run_at
            .as_deref()
            .and_then(|value| instant_ms_str(Some(value)));
        let db = self.connect()?;
        self.ensure_room(&db, &job.guild_id, &job.voice_channel_id, "", "", "")?;
        db.execute(
            "INSERT INTO transcript_jobs(job_id, guild_id, voice_channel_id, kind, state, created_at_ms, updated_at_ms, next_run_at_ms, payload_blob) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                &job.id,
                &job.guild_id,
                &job.voice_channel_id,
                job.kind.as_str(),
                job.state.as_str(),
                created_ms,
                created_ms,
                next_run_at_ms,
                job.encode()?
            ],
        )?;
        self.append_event(
            &job.guild_id,
            &job.voice_channel_id,
            serde_json::json!({"event_kind": "job_created", "kind": "job_created", "job_id": job.id, "job_kind": job.kind.as_str(), "state": job.state.as_str()}),
        )?;
        Ok(job)
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
            "SELECT payload_blob FROM transcript_jobs WHERE job_id = ?1",
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
        let updated_ms = instant_ms_str(Some(&payload.updated_at)).unwrap_or(0);
        let created_ms = instant_ms_str(Some(&payload.created_at)).unwrap_or(updated_ms);
        let db = self.connect()?;
        db.execute(
            r#"
            INSERT INTO transcript_jobs(job_id, guild_id, voice_channel_id, kind, state, created_at_ms, updated_at_ms, next_run_at_ms, payload_blob)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
            ON CONFLICT(job_id) DO UPDATE SET
              kind = excluded.kind,
              state = excluded.state,
              updated_at_ms = excluded.updated_at_ms,
              next_run_at_ms = excluded.next_run_at_ms,
              payload_blob = excluded.payload_blob
            "#,
            params![
                &payload.id,
                &payload.guild_id,
                &payload.voice_channel_id,
                payload.kind.as_str(),
                payload.state.as_str(),
                created_ms,
                updated_ms,
                payload
                    .next_run_at
                    .as_deref()
                    .and_then(|value| instant_ms_str(Some(value))),
                payload.encode()?
            ],
        )?;
        Ok(())
    }

    pub fn claim_due_jobs(
        &self,
        kind: crate::runtime::JobKind,
        limit: usize,
        mut skip: impl FnMut(&Job) -> bool,
    ) -> Result<Vec<Job>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let now = utc_now();
        let mut candidates = self
            .list_jobs(None, Some(crate::runtime::JobState::Queued))?
            .into_iter()
            .filter(|job| job.kind == kind && !job.id.trim().is_empty())
            .filter(|job| {
                job.next_run_at
                    .as_deref()
                    .and_then(parse_instant)
                    .is_none_or(|next_run_at| next_run_at <= now)
            })
            .filter(|job| !skip(job))
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| {
            let left_due = left
                .next_run_at
                .as_deref()
                .and_then(parse_instant)
                .or_else(|| parse_instant(&left.created_at));
            let right_due = right
                .next_run_at
                .as_deref()
                .and_then(parse_instant)
                .or_else(|| parse_instant(&right.created_at));
            left_due
                .cmp(&right_due)
                .then_with(|| left.created_at.cmp(&right.created_at))
                .then_with(|| left.id.cmp(&right.id))
        });

        let mut db = self.connect()?;
        let transaction = db.transaction()?;
        let mut claimed = Vec::new();
        for mut job in candidates {
            if claimed.len() >= limit {
                break;
            }
            job.mark_running();
            let payload = job.touched();
            let updated_ms = instant_ms_str(Some(&payload.updated_at)).unwrap_or(0);
            let next_run_at_ms = payload
                .next_run_at
                .as_deref()
                .and_then(|value| instant_ms_str(Some(value)));
            let changed = transaction.execute(
                r#"
                UPDATE transcript_jobs
                SET state = ?1,
                    updated_at_ms = ?2,
                    next_run_at_ms = ?3,
                    payload_blob = ?4
                WHERE job_id = ?5 AND state = ?6
                "#,
                params![
                    payload.state.as_str(),
                    updated_ms,
                    next_run_at_ms,
                    payload.encode()?,
                    &payload.id,
                    crate::runtime::JobState::Queued.as_str(),
                ],
            )?;
            if changed == 1 {
                claimed.push(payload);
            }
        }
        transaction.commit()?;
        Ok(claimed)
    }

    pub fn list_jobs(
        &self,
        guild_id: Option<&str>,
        state: Option<crate::runtime::JobState>,
    ) -> Result<Vec<Job>> {
        let mut conditions = Vec::new();
        let mut params_values: Vec<Box<dyn ToSql>> = Vec::new();
        if let Some(guild_id) = guild_id.filter(|value| !value.is_empty()) {
            conditions.push("guild_id = ?".to_string());
            params_values.push(Box::new(guild_id.to_string()));
        }
        if let Some(state) = state {
            conditions.push("state = ?".to_string());
            params_values.push(Box::new(state.as_str().to_string()));
        }
        let where_clause = if conditions.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", conditions.join(" AND "))
        };
        let sql = format!(
            "SELECT payload_blob FROM transcript_jobs{where_clause} ORDER BY COALESCE(created_at_ms, updated_at_ms) DESC"
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

    pub fn list_child_jobs(&self, parent_job_id: &str) -> Result<Vec<Job>> {
        let child_ids = self.list_child_job_ids(parent_job_id)?;
        let mut children = if child_ids.is_empty() {
            self.list_jobs(None, None)?
                .into_iter()
                .filter(|job| job.parent_job_id.as_deref() == Some(parent_job_id))
                .collect::<Vec<_>>()
        } else {
            let mut children = Vec::new();
            for child_id in child_ids {
                if let Ok(child) = self.get_job(&child_id) {
                    children.push(child);
                }
            }
            children
        };
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
