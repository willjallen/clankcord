use super::*;

impl TimelineStore {
    pub async fn create_window(
        &self,
        guild_id: &str,
        voice_channel_id: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        selection_kind: &str,
        selection_reference: &str,
        scope: &str,
    ) -> Result<Value> {
        let kinds = set(["speech_segment", "transcript"]);
        let events = self
            .load_events(
                guild_id,
                voice_channel_id,
                Some(start),
                Some(end),
                Some(&kinds),
                None,
                false,
            )
            .await?;
        let window_id = new_id("win");
        let capture_runs = sorted_unique(
            events
                .iter()
                .map(|event| first_value_string(event, &["capture_run_id", "captureRunId"])),
        );
        let voice_bots = sorted_unique(
            events
                .iter()
                .map(|event| first_value_string(event, &["voice_bot_id", "botId"])),
        );
        let window = serde_json::json!({
            "window_id": window_id,
            "guild_id": guild_id,
            "scope": scope,
            "voice_channel_id": voice_channel_id,
            "selection_kind": selection_kind,
            "selection_reference": selection_reference,
            "start_time": isoformat_z(Some(start)),
            "end_time": isoformat_z(Some(end)),
            "event_id_start": events.first().map(|event| first_value_string(event, &["event_id", "eventId"])).unwrap_or_default(),
            "event_id_end": events.last().map(|event| first_value_string(event, &["event_id", "eventId"])).unwrap_or_default(),
            "capture_run_ids": capture_runs,
            "voice_bot_ids": voice_bots,
            "quality": "draft",
            "created_at": isoformat_z(None)
        });
        self.ensure_room(guild_id, voice_channel_id, "", "", "")
            .await?;
        sqlx::query(
            "INSERT INTO windows(window_id, scope_kind, guild_id, scope_id, start_ms, end_ms, payload_json) VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(&window_id)
        .bind("voice_channel")
        .bind(guild_id)
        .bind(voice_channel_id)
        .bind(instant_ms_dt(start))
        .bind(instant_ms_dt(end))
        .bind(&window)
        .execute(&self.pool)
        .await?;
        Ok(window)
    }

    pub async fn get_window(&self, window_id: &str) -> Result<Value> {
        self.get_payload_by_id("windows", "window_id", window_id)
            .await
    }

    pub async fn render_transcript(
        &self,
        guild_id: &str,
        voice_channel_id: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        window_id: &str,
        format: &str,
    ) -> Result<RenderedTranscript> {
        let kinds = set(["speech_segment", "transcript"]);
        let events = self
            .load_events(
                guild_id,
                voice_channel_id,
                Some(start),
                Some(end),
                Some(&kinds),
                None,
                false,
            )
            .await?;
        let mut items: Vec<(DateTime<Utc>, &'static str, Value)> = Vec::new();
        for event in &events {
            if let Some(started) = event_start(event) {
                items.push((started, "event", event.clone()));
            }
        }
        items.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));
        let window = serde_json::json!({
            "window_id": window_id,
            "guild_id": guild_id,
            "scope": "single_channel",
            "voice_channel_id": voice_channel_id,
            "start_time": isoformat_z(Some(start)),
            "end_time": isoformat_z(Some(end)),
            "quality": "draft"
        });
        let content = match format {
            "json" => serde_json::to_string_pretty(&serde_json::json!({"events": events}))?,
            "markdown" => render_markdown_transcript(&window, &items, events.len()),
            _ => anyhow::bail!("transcript render format must be json or markdown"),
        };
        Ok(RenderedTranscript {
            window,
            events,
            content,
        })
    }

    pub async fn materialize(
        &self,
        guild_id: &str,
        voice_channel_id: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        selection_kind: &str,
        selection_reference: &str,
        created_by_user_id: &str,
        publish: &str,
        live: bool,
        parent_job_id: Option<&str>,
    ) -> Result<Value> {
        let window = self
            .create_window(
                guild_id,
                voice_channel_id,
                start,
                end,
                selection_kind,
                selection_reference,
                "single_channel",
            )
            .await?;
        let rendered = self
            .render_transcript(
                guild_id,
                voice_channel_id,
                start,
                end,
                &string_field(&window, "window_id"),
                "markdown",
            )
            .await?;
        let publication_id = new_id("pub");
        let artifact_dir = self.durable_publications_dir().join(&publication_id);
        fs::create_dir_all(&artifact_dir)?;
        let draft_path = artifact_dir.join("transcript.draft.txt");
        fs::write(
            &draft_path,
            if rendered.content.trim().is_empty() {
                String::new()
            } else {
                format!("{}\n", rendered.content.trim())
            },
        )?;
        let publication = serde_json::json!({
            "publication_id": publication_id,
            "window_id": string_field(&window, "window_id"),
            "guild_id": guild_id,
            "voice_channel_id": voice_channel_id,
            "discord_thread_id": "",
            "discord_message_ids": [],
            "state": if live { "live_draft_created" } else { "draft_created" },
            "publish": publish,
            "parent_job_id": parent_job_id.unwrap_or(""),
            "created_by_user_id": created_by_user_id,
            "created_at": isoformat_z(None),
            "draft_artifact_path": draft_path.display().to_string()
        });
        write_json_file(
            &artifact_dir.join("metadata.json"),
            &serde_json::json!({"window": window, "publication": publication}),
        )?;
        self.update_publication(&publication).await?;
        self.append_event(
            guild_id,
            voice_channel_id,
            serde_json::json!({
                "event_kind": "publication_created",
                "kind": "publication_created",
                "publication_id": publication_id,
                "window_id": string_field(&window, "window_id"),
                "start_time": isoformat_z(Some(start)),
                "end_time": isoformat_z(Some(end)),
                "state": string_field(&publication, "state"),
                "publish": publish
            }),
        )
        .await?;
        let job = Value::Null;
        Ok(serde_json::json!({"window": window, "publication": publication, "job": job}))
    }
}

fn render_markdown_transcript(
    window: &Value,
    items: &[(DateTime<Utc>, &'static str, Value)],
    event_count: usize,
) -> String {
    let mut lines = vec![
        "# Transcript".to_string(),
        String::new(),
        format!("window_id: {}", string_field(window, "window_id")),
        format!("guild_id: {}", string_field(window, "guild_id")),
        format!(
            "voice_channel_id: {}",
            string_field(window, "voice_channel_id")
        ),
        format!("start_time: {}", string_field(window, "start_time")),
        format!("end_time: {}", string_field(window, "end_time")),
        format!("event_count: {event_count}"),
        format!(
            "first_event_id: {}",
            items
                .first()
                .map(|(_, _, event)| first_value_string(event, &["event_id", "eventId"]))
                .unwrap_or_default()
        ),
        format!(
            "last_event_id: {}",
            items
                .last()
                .map(|(_, _, event)| first_value_string(event, &["event_id", "eventId"]))
                .unwrap_or_default()
        ),
        String::new(),
        "participants:".to_string(),
    ];
    for (speaker_user_id, labels) in transcript_participants(items) {
        lines.push(format!(
            "- {}: {}",
            speaker_user_id,
            labels.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }
    lines.push(String::new());
    lines.push("## Conversation".to_string());
    lines.push(String::new());
    for (_, _kind, payload) in items {
        let text = event_text(payload);
        if text.is_empty() {
            continue;
        }
        let stamp = event_start(payload)
            .map(|value| value.to_rfc3339_opts(SecondsFormat::Secs, true))
            .unwrap_or_default();
        let prefix = if stamp.is_empty() {
            String::new()
        } else {
            format!("[{stamp}] ")
        };
        lines.push(format!("{prefix}{}: {text}", event_speaker(payload)));
    }
    lines.join("\n").trim().to_string()
}

fn transcript_participants(
    items: &[(DateTime<Utc>, &'static str, Value)],
) -> BTreeMap<String, BTreeSet<String>> {
    let mut participants = BTreeMap::new();
    for (_, _, event) in items {
        let speaker = event_speaker(event);
        let speaker_user_id = non_empty(
            first_value_string(event, &["speaker_user_id", "speakerId"]),
            "unknown".to_string(),
        );
        participants
            .entry(speaker_user_id)
            .or_insert_with(BTreeSet::new)
            .insert(speaker);
    }
    participants
}

impl TimelineStore {
    pub async fn get_publication(&self, publication_id: &str) -> Result<Value> {
        self.get_payload_by_id("publications", "publication_id", publication_id)
            .await
    }

    pub async fn list_publications(
        &self,
        guild_id: Option<&str>,
        voice_channel_id: Option<&str>,
        state: Option<&str>,
    ) -> Result<Vec<Value>> {
        let mut query = QueryBuilder::<Postgres>::new("SELECT payload_json FROM publications");
        let mut has_where = false;
        if let Some(guild_id) = guild_id.filter(|value| !value.is_empty()) {
            push_filter_prefix(&mut query, &mut has_where);
            query.push("guild_id = ").push_bind(guild_id);
        }
        if let Some(channel_id) = voice_channel_id.filter(|value| !value.is_empty()) {
            push_filter_prefix(&mut query, &mut has_where);
            query
                .push("scope_kind = 'voice_channel' AND scope_id = ")
                .push_bind(channel_id);
        }
        if let Some(state) = state.filter(|value| !value.is_empty()) {
            push_filter_prefix(&mut query, &mut has_where);
            query.push("state = ").push_bind(state);
        }
        query.push(" ORDER BY COALESCE(created_at_ms, updated_at_ms) DESC");
        let rows = query.build().fetch_all(&self.pool).await?;
        rows.iter()
            .map(|row| json_value(row, "payload_json"))
            .collect()
    }

    pub async fn update_publication(&self, publication: &Value) -> Result<()> {
        let publication_id = string_field(publication, "publication_id");
        let guild_id = string_field(publication, "guild_id");
        let channel_id = string_field(publication, "voice_channel_id");
        if publication_id.is_empty() || guild_id.is_empty() || channel_id.is_empty() {
            return Ok(());
        }
        let now_ms = instant_ms_dt(utc_now());
        let created_ms =
            instant_ms_str(Some(&string_field(publication, "created_at"))).unwrap_or(now_ms);
        self.ensure_room(&guild_id, &channel_id, "", "", "").await?;
        sqlx::query(
            r#"
            INSERT INTO publications(publication_id, scope_kind, guild_id, scope_id, window_id, state, created_at_ms, updated_at_ms, payload_json)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            ON CONFLICT(publication_id) DO UPDATE SET
              scope_kind = EXCLUDED.scope_kind,
              guild_id = EXCLUDED.guild_id,
              scope_id = EXCLUDED.scope_id,
              window_id = EXCLUDED.window_id,
              state = EXCLUDED.state,
              updated_at_ms = EXCLUDED.updated_at_ms,
              payload_json = EXCLUDED.payload_json
            "#,
        )
        .bind(&publication_id)
        .bind("voice_channel")
        .bind(&guild_id)
        .bind(&channel_id)
        .bind(string_field(publication, "window_id"))
        .bind(string_field(publication, "state"))
        .bind(created_ms)
        .bind(now_ms)
        .bind(publication)
        .execute(&self.pool)
        .await?;
        let artifact_dir = self.durable_publications_dir().join(&publication_id);
        let metadata_path = artifact_dir.join("metadata.json");
        let mut metadata = read_json_file(&metadata_path, serde_json::json!({}));
        if !metadata.is_object() {
            metadata = serde_json::json!({});
        }
        metadata
            .as_object_mut()
            .unwrap()
            .insert("publication".to_string(), publication.clone());
        write_json_file(&metadata_path, &metadata)?;
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
