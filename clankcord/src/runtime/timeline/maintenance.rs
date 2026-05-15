use super::*;

impl TimelineStore {
    pub fn channel_dirs(
        &self,
        guild_id: &str,
        voice_channel_id: Option<&str>,
    ) -> Result<Vec<PathBuf>> {
        if let Some(channel_id) = voice_channel_id.filter(|value| !value.is_empty()) {
            let path = self.channel_dir(guild_id, channel_id);
            return Ok(if path.exists() {
                vec![path]
            } else {
                Vec::new()
            });
        }
        let db = self.connect()?;
        let mut statement = db.prepare("SELECT DISTINCT voice_channel_id FROM voice_rooms WHERE guild_id = ? ORDER BY voice_channel_id")?;
        let rows = statement.query_map(params![guild_id], |row| row.get::<_, String>(0))?;
        let mut paths = Vec::new();
        for row in rows {
            let channel_id = row?;
            let path = self.channel_dir(guild_id, &channel_id);
            fs::create_dir_all(&path)?;
            paths.push(path);
        }
        Ok(paths)
    }

    pub fn channel_id_from_dir(path: &Path) -> String {
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        name.strip_prefix("channel-").unwrap_or(&name).to_string()
    }

    pub fn search(
        &self,
        guild_id: &str,
        voice_channel_id: Option<&str>,
        query: &str,
        since: Option<DateTime<Utc>>,
        prefer_refined: bool,
        limit: usize,
    ) -> Result<Vec<Value>> {
        let needle = query.trim().to_lowercase();
        if needle.is_empty() {
            return Ok(Vec::new());
        }
        let mut hits = Vec::new();
        for channel_dir in self.channel_dirs(guild_id, voice_channel_id)? {
            let channel_id = Self::channel_id_from_dir(&channel_dir);
            let spans = if prefer_refined {
                self.list_spans(guild_id, &channel_id, since, None)?
            } else {
                Vec::new()
            };
            for span in &spans {
                let artifact = PathBuf::from(string_field(span, "text_artifact_path"));
                if !artifact.is_file() {
                    continue;
                }
                let content = fs::read_to_string(&artifact).unwrap_or_default();
                if !content.to_lowercase().contains(&needle) {
                    continue;
                }
                hits.push(serde_json::json!({
                    "kind": "refined_span",
                    "guild_id": guild_id,
                    "voice_channel_id": channel_id,
                    "span_id": string_field(span, "span_id"),
                    "window_id": string_field(span, "window_id"),
                    "start_time": string_field(span, "start_time"),
                    "end_time": string_field(span, "end_time"),
                    "excerpt": excerpt(&content, &needle, 160)
                }));
            }
            let events =
                self.search_draft_events(guild_id, &channel_id, query, since, limit * 2)?;
            for event in events {
                if prefer_refined && self.event_covered_by_span(&event, &spans) {
                    continue;
                }
                let text = event_text(&event);
                let started = event_start(&event).unwrap_or_else(utc_now);
                hits.push(serde_json::json!({
                    "kind": "draft_event",
                    "guild_id": guild_id,
                    "voice_channel_id": channel_id,
                    "event_id": first_value_string(&event, &["event_id", "eventId"]),
                    "speaker_label": event_speaker(&event),
                    "start_time": started.to_rfc3339_opts(SecondsFormat::Millis, true),
                    "excerpt": excerpt(&text, &needle, 160)
                }));
            }
        }
        hits.sort_by_key(|hit| std::cmp::Reverse(string_field(hit, "start_time")));
        hits.truncate(limit);
        Ok(hits)
    }

    pub fn search_draft_events(
        &self,
        guild_id: &str,
        voice_channel_id: &str,
        query: &str,
        since: Option<DateTime<Utc>>,
        limit: usize,
    ) -> Result<Vec<Value>> {
        let fts_query = self.fts_query(query);
        if self.fts_enabled && !fts_query.is_empty() {
            let mut conditions = vec![
                "e.guild_id = ?".to_string(),
                "e.voice_channel_id = ?".to_string(),
                "e.forgotten = 0".to_string(),
            ];
            let mut params_values: Vec<Box<dyn ToSql>> = vec![
                Box::new(guild_id.to_string()),
                Box::new(voice_channel_id.to_string()),
            ];
            if let Some(since) = since {
                conditions.push("COALESCE(e.started_at_ms, e.created_at_ms) >= ?".to_string());
                params_values.push(Box::new(instant_ms_dt(since)));
            }
            params_values.push(Box::new(fts_query));
            params_values.push(Box::new(limit.max(1) as i64));
            let sql = format!(
                r#"
                SELECT e.*,
                       r.guild_slug AS room_guild_slug,
                       r.voice_channel_name AS room_voice_channel_name,
                       r.voice_channel_slug AS room_voice_channel_slug
                FROM timeline_events e
                JOIN transcript_events_fts ON transcript_events_fts.event_id = e.event_id
                LEFT JOIN voice_rooms r
                  ON r.guild_id = e.guild_id AND r.voice_channel_id = e.voice_channel_id
                WHERE {} AND transcript_events_fts MATCH ?
                ORDER BY COALESCE(e.started_at_ms, e.created_at_ms) DESC LIMIT ?
                "#,
                conditions.join(" AND ")
            );
            let result = (|| -> Result<Vec<Value>> {
                let db = self.connect()?;
                let mut statement = db.prepare(&sql)?;
                let rows = statement.query_map(
                    params_from_iter(params_values.iter().map(|value| &**value)),
                    |row| timeline_event_payload(row),
                )?;
                Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
            })();
            if result.is_ok() {
                return result;
            }
        }
        let kinds = set(["speech_segment", "transcript"]);
        let mut events = self.load_events(
            guild_id,
            voice_channel_id,
            since,
            None,
            Some(&kinds),
            None,
            false,
        )?;
        let needle = query.trim().to_lowercase();
        events.retain(|event| event_text(event).to_lowercase().contains(&needle));
        events.truncate(limit);
        Ok(events)
    }

    pub fn fts_query(&self, query: &str) -> String {
        Regex::new(r"[\w]+")
            .unwrap()
            .find_iter(&query.to_lowercase())
            .map(|mat| mat.as_str().to_string())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

impl TimelineStore {
    pub fn apply_forget(
        &self,
        guild_id: &str,
        voice_channel_id: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        requested_by_user_id: &str,
        unpublished_only: bool,
    ) -> Result<Value> {
        let kinds = set(["speech_segment", "transcript"]);
        let events = self.load_events(
            guild_id,
            voice_channel_id,
            Some(start),
            Some(end),
            Some(&kinds),
            None,
            false,
        )?;
        let mut deleted_audio = Vec::new();
        let mut event_ids = Vec::new();
        for event in &events {
            let event_id = first_value_string(event, &["event_id", "eventId"]);
            if !event_id.is_empty() {
                event_ids.push(event_id);
            }
            let source = PathBuf::from(first_value_string(
                event,
                &["source_audio_path", "sourceAudioPath"],
            ));
            if source.is_file() && fs::remove_file(&source).is_ok() {
                deleted_audio.push(source.display().to_string());
            }
        }
        self.mark_events_forgotten(&event_ids)?;
        let event = self.append_event(
            guild_id,
            voice_channel_id,
            serde_json::json!({
                "event_kind": "forget_applied",
                "kind": "forget_applied",
                "start_time": isoformat_z(Some(start)),
                "end_time": isoformat_z(Some(end)),
                "requested_by_user_id": requested_by_user_id,
                "unpublished_only": unpublished_only,
                "event_count": events.len(),
                "deleted_audio_paths": deleted_audio
            }),
        )?;
        Ok(serde_json::json!({
            "forgotten_event_count": events.len(),
            "deleted_audio_count": deleted_audio.len(),
            "event": event
        }))
    }

    pub fn mark_events_forgotten(&self, event_ids: &[String]) -> Result<()> {
        let ids: Vec<String> = event_ids
            .iter()
            .filter(|id| !id.is_empty())
            .cloned()
            .collect();
        if ids.is_empty() {
            return Ok(());
        }
        let placeholders = std::iter::repeat("?")
            .take(ids.len())
            .collect::<Vec<_>>()
            .join(",");
        let db = self.connect()?;
        let sql =
            format!("UPDATE timeline_events SET forgotten = 1 WHERE event_id IN ({placeholders})");
        db.execute(&sql, params_from_iter(ids.iter()))?;
        if self.fts_enabled {
            let sql =
                format!("DELETE FROM transcript_events_fts WHERE event_id IN ({placeholders})");
            let _ = db.execute(&sql, params_from_iter(ids.iter()));
        }
        Ok(())
    }

    pub fn participant_trace(
        &self,
        guild_id: &str,
        user_id: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        include_speech_snippets: bool,
    ) -> Result<Vec<Value>> {
        let mut trace = Vec::new();
        for channel_dir in self.channel_dirs(guild_id, None)? {
            let channel_id = Self::channel_id_from_dir(&channel_dir);
            let events = self.load_events(
                guild_id,
                &channel_id,
                Some(start),
                Some(end),
                None,
                None,
                false,
            )?;
            for mut event in events {
                let kind = first_value_string(&event, &["event_kind", "kind"]);
                let actor =
                    first_value_string(&event, &["user_id", "speaker_user_id", "speakerId"]);
                if actor != user_id {
                    continue;
                }
                if SPEECH_KINDS.contains(&kind.as_str()) && !include_speech_snippets {
                    continue;
                }
                if SPEECH_KINDS.contains(&kind.as_str()) {
                    let snippet = event_text(&event).chars().take(240).collect();
                    event
                        .as_object_mut()
                        .unwrap()
                        .insert("speech_snippet".to_string(), Value::String(snippet));
                }
                trace.push(event);
            }
        }
        trace.sort_by_key(|item| {
            first_value_string(item, &["timestamp", "segment_start_time", "startedAt"])
        });
        Ok(trace)
    }
}

impl TimelineStore {
    pub fn retention_sweep(&self, now: Option<DateTime<Utc>>, dry_run: bool) -> Result<Value> {
        let current = now.unwrap_or_else(utc_now);
        let draft_cutoff = current - chrono::Duration::days(7);
        let job_cutoff = current - chrono::Duration::days(30);
        let cutoff_ms = instant_ms_dt(draft_cutoff);
        let db = self.connect()?;
        let mut statement = db.prepare(
            r#"
            SELECT e.*,
                   r.guild_slug AS room_guild_slug,
                   r.voice_channel_name AS room_voice_channel_name,
                   r.voice_channel_slug AS room_voice_channel_slug
            FROM timeline_events e
            LEFT JOIN voice_rooms r
              ON r.guild_id = e.guild_id AND r.voice_channel_id = e.voice_channel_id
            WHERE e.event_kind IN ('speech_segment', 'transcript')
              AND e.forgotten = 0
              AND e.started_at_ms IS NOT NULL
              AND e.started_at_ms < ?1
            ORDER BY e.guild_id, e.voice_channel_id, e.started_at_ms
            "#,
        )?;
        let rows = statement.query_map(params![cutoff_ms], |row| {
            Ok((
                row.get::<_, String>("event_id")?,
                row.get::<_, String>("guild_id")?,
                row.get::<_, String>("voice_channel_id")?,
                timeline_event_payload(row)?,
            ))
        })?;
        let mut event_ids = Vec::new();
        let mut deleted_audio = 0;
        let mut retired_by_channel: BTreeMap<(String, String), usize> = BTreeMap::new();
        for row in rows {
            let (event_id, guild_id, channel_id, event) = row?;
            event_ids.push(event_id);
            *retired_by_channel
                .entry((guild_id, channel_id))
                .or_default() += 1;
            let source = PathBuf::from(first_value_string(
                &event,
                &["source_audio_path", "sourceAudioPath"],
            ));
            if source.is_file() && !dry_run && fs::remove_file(&source).is_ok() {
                deleted_audio += 1;
            }
        }
        let retired_events = event_ids.len();
        drop(statement);
        if !event_ids.is_empty() && !dry_run {
            self.mark_events_forgotten(&event_ids)?;
            for ((guild_id, channel_id), channel_retired) in retired_by_channel {
                self.append_event(
                    &guild_id,
                    &channel_id,
                    serde_json::json!({
                        "event_kind": "retention_retired",
                        "kind": "retention_retired",
                        "cutoff": isoformat_z(Some(draft_cutoff)),
                        "retired_event_count": channel_retired
                    }),
                )?;
            }
        }
        let job_cutoff_ms = instant_ms_dt(job_cutoff);
        let old_jobs: Vec<String> = db
            .prepare(
                "SELECT job_id FROM jobs WHERE created_at_ms IS NOT NULL AND created_at_ms < ?1",
            )?
            .query_map(params![job_cutoff_ms], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let deleted_jobs = old_jobs.len();
        if !old_jobs.is_empty() && !dry_run {
            let placeholders = std::iter::repeat("?")
                .take(old_jobs.len())
                .collect::<Vec<_>>()
                .join(",");
            db.execute(
                &format!("DELETE FROM jobs WHERE job_id IN ({placeholders})"),
                params_from_iter(old_jobs.iter()),
            )?;
        }
        Ok(serde_json::json!({
            "retired_events": retired_events,
            "deleted_audio": deleted_audio,
            "deleted_jobs": deleted_jobs,
            "dry_run": dry_run
        }))
    }
}
