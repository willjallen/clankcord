use super::*;

impl TimelineStore {
    pub fn create_job(&self, job: Job) -> Result<Job> {
        let created_ms = instant_ms_str(Some(&job.created_at)).unwrap_or(0);
        let db = self.connect()?;
        self.ensure_room(&db, &job.guild_id, &job.voice_channel_id, "", "", "")?;
        db.execute(
            "INSERT INTO transcript_jobs(job_id, guild_id, voice_channel_id, kind, state, created_at_ms, updated_at_ms, next_run_at_ms, payload_blob) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, ?8)",
            params![
                &job.id,
                &job.guild_id,
                &job.voice_channel_id,
                job.kind.as_str(),
                job.state.as_str(),
                created_ms,
                created_ms,
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
        let child = self.create_job(child)?;
        let mut waiting_parent = parent.clone();
        if !waiting_parent.state.is_terminal() {
            waiting_parent.mark_waiting();
            self.update_job(&waiting_parent)?;
        }
        Ok(child)
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
        let mut children = self
            .list_jobs(None, None)?
            .into_iter()
            .filter(|job| job.parent_job_id.as_deref() == Some(parent_job_id))
            .collect::<Vec<_>>();
        children.sort_by(|left, right| left.created_at.cmp(&right.created_at));
        Ok(children)
    }
}
