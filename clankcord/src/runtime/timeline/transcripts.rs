use super::*;

impl TimelineStore {
    pub fn create_window(
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
        let events = self.load_events(
            guild_id,
            voice_channel_id,
            Some(start),
            Some(end),
            Some(&kinds),
            None,
            false,
        )?;
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
            "requires_refinement_for_permanent": true,
            "created_at": isoformat_z(None)
        });
        let db = self.connect()?;
        self.ensure_room(&db, guild_id, voice_channel_id, "", "", "")?;
        db.execute(
            "INSERT INTO windows(window_id, guild_id, voice_channel_id, start_ms, end_ms, payload_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![window_id, guild_id, voice_channel_id, instant_ms_dt(start), instant_ms_dt(end), json_dumps(&window)?],
        )?;
        Ok(window)
    }

    pub fn get_window(&self, window_id: &str) -> Result<Value> {
        self.get_payload_by_id("windows", "window_id", window_id)
    }

    pub fn render_transcript(
        &self,
        guild_id: &str,
        voice_channel_id: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        window_id: &str,
        prefer_refined: bool,
        format: &str,
    ) -> Result<RenderedTranscript> {
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
        let spans = if prefer_refined {
            self.list_spans(guild_id, voice_channel_id, Some(start), Some(end))?
        } else {
            Vec::new()
        };
        let mut items: Vec<(DateTime<Utc>, &'static str, Value)> = Vec::new();
        let mut used_spans = BTreeSet::<String>::new();
        for span in &spans {
            let Some(span_start) = parse_instant(&string_field(span, "start_time")) else {
                continue;
            };
            let span_id = first_value_string(span, &["span_id", "authoritative_span_id"]);
            let artifact = PathBuf::from(string_field(span, "text_artifact_path"));
            if !artifact.is_file() || !used_spans.insert(span_id) {
                continue;
            }
            items.push((span_start, "span", span.clone()));
        }
        for event in &events {
            if prefer_refined && self.event_covered_by_span(event, &spans) {
                continue;
            }
            if let Some(started) = event_start(event) {
                items.push((started, "event", event.clone()));
            }
        }
        items.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));
        let content = if format == "json" {
            serde_json::to_string_pretty(
                &serde_json::json!({"events": events, "authoritative_spans": spans}),
            )?
        } else {
            let mut lines = Vec::new();
            for (_, kind, payload) in &items {
                if *kind == "span" {
                    let artifact = PathBuf::from(string_field(payload, "text_artifact_path"));
                    if let Ok(text) = fs::read_to_string(artifact) {
                        let text = text.trim();
                        if !text.is_empty() {
                            lines.push(text.to_string());
                        }
                    }
                    continue;
                }
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
        };
        let window = serde_json::json!({
            "window_id": window_id,
            "guild_id": guild_id,
            "scope": "single_channel",
            "voice_channel_id": voice_channel_id,
            "start_time": isoformat_z(Some(start)),
            "end_time": isoformat_z(Some(end)),
            "quality": if prefer_refined && !spans.is_empty() { "mixed" } else { "draft" }
        });
        Ok(RenderedTranscript {
            window,
            events,
            spans,
            content,
        })
    }

    pub fn materialize(
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
        refine: bool,
        prefer_refined: bool,
        parent_job_id: Option<&str>,
    ) -> Result<Value> {
        let window = self.create_window(
            guild_id,
            voice_channel_id,
            start,
            end,
            selection_kind,
            selection_reference,
            "single_channel",
        )?;
        let rendered = self.render_transcript(
            guild_id,
            voice_channel_id,
            start,
            end,
            &string_field(&window, "window_id"),
            prefer_refined,
            "markdown",
        )?;
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
        let mut publication = serde_json::json!({
            "publication_id": publication_id,
            "window_id": string_field(&window, "window_id"),
            "guild_id": guild_id,
            "voice_channel_id": voice_channel_id,
            "discord_thread_id": "",
            "discord_message_ids": [],
            "state": if live { "live_draft_published" } else { "draft_created" },
            "publish": publish,
            "created_by_user_id": created_by_user_id,
            "created_at": isoformat_z(None),
            "draft_artifact_path": draft_path.display().to_string(),
            "refined_artifact_path": Value::Null,
            "recording_artifact_path": Value::Null,
            "refinement_job_id": "",
            "refine_requested": refine
        });
        write_json_file(
            &artifact_dir.join("metadata.json"),
            &serde_json::json!({"window": window, "publication": publication}),
        )?;
        self.update_publication(&publication)?;
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
                "publish": publish,
                "refine_requested": refine
            }),
        )?;
        let mut job = Value::Null;
        if refine {
            let refinement_job = Job::refine_transcript(
                guild_id,
                voice_channel_id,
                created_by_user_id,
                string_field(&window, "window_id"),
                publication_id.clone(),
            );
            let refinement_job = if let Some(parent_job_id) =
                parent_job_id.filter(|value| !value.trim().is_empty())
            {
                let parent = self.get_job(parent_job_id)?;
                self.create_child_job(&parent, refinement_job)?
            } else {
                self.create_job(refinement_job)?
            };
            publication.as_object_mut().unwrap().insert(
                "refinement_job_id".to_string(),
                Value::String(refinement_job.id.clone()),
            );
            self.update_publication(&publication)?;
            job = refinement_job.to_value();
        }
        Ok(serde_json::json!({"window": window, "publication": publication, "job": job}))
    }
}

impl TimelineStore {
    pub fn get_publication(&self, publication_id: &str) -> Result<Value> {
        self.get_payload_by_id("publications", "publication_id", publication_id)
    }

    pub fn list_publications(
        &self,
        guild_id: Option<&str>,
        voice_channel_id: Option<&str>,
        state: Option<&str>,
    ) -> Result<Vec<Value>> {
        let mut conditions = Vec::new();
        let mut params_values: Vec<Box<dyn ToSql>> = Vec::new();
        if let Some(guild_id) = guild_id.filter(|value| !value.is_empty()) {
            conditions.push("guild_id = ?".to_string());
            params_values.push(Box::new(guild_id.to_string()));
        }
        if let Some(channel_id) = voice_channel_id.filter(|value| !value.is_empty()) {
            conditions.push("voice_channel_id = ?".to_string());
            params_values.push(Box::new(channel_id.to_string()));
        }
        if let Some(state) = state.filter(|value| !value.is_empty()) {
            conditions.push("state = ?".to_string());
            params_values.push(Box::new(state.to_string()));
        }
        let where_clause = if conditions.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", conditions.join(" AND "))
        };
        let sql = format!(
            "SELECT payload_json FROM publications{where_clause} ORDER BY COALESCE(created_at_ms, updated_at_ms) DESC"
        );
        let db = self.connect()?;
        let mut statement = db.prepare(&sql)?;
        let rows = statement.query_map(
            params_from_iter(params_values.iter().map(|value| &**value)),
            |row| payload_from_row(row, 0),
        )?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn update_publication(&self, publication: &Value) -> Result<()> {
        let publication_id = string_field(publication, "publication_id");
        let guild_id = string_field(publication, "guild_id");
        let channel_id = string_field(publication, "voice_channel_id");
        if publication_id.is_empty() || guild_id.is_empty() || channel_id.is_empty() {
            return Ok(());
        }
        let now_ms = instant_ms_dt(utc_now());
        let created_ms =
            instant_ms_str(Some(&string_field(publication, "created_at"))).unwrap_or(now_ms);
        let db = self.connect()?;
        self.ensure_room(&db, &guild_id, &channel_id, "", "", "")?;
        db.execute(
            r#"
            INSERT INTO publications(publication_id, guild_id, voice_channel_id, window_id, state, created_at_ms, updated_at_ms, payload_json)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            ON CONFLICT(publication_id) DO UPDATE SET
              window_id = excluded.window_id,
              state = excluded.state,
              updated_at_ms = excluded.updated_at_ms,
              payload_json = excluded.payload_json
            "#,
            params![
                publication_id,
                guild_id,
                channel_id,
                string_field(publication, "window_id"),
                string_field(publication, "state"),
                created_ms,
                now_ms,
                json_dumps(publication)?
            ],
        )?;
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

    pub fn create_authoritative_span(
        &self,
        guild_id: &str,
        voice_channel_id: &str,
        window_id: &str,
        publication_id: &str,
        provider: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        text_artifact_path: &Path,
        speaker_alignment_path: &Path,
        capture_run_ids: Vec<String>,
        voice_bot_ids: Vec<String>,
    ) -> Result<Value> {
        let span_id = new_id("span");
        let span = serde_json::json!({
            "span_id": span_id,
            "authoritative_span_id": span_id,
            "kind": "refined_transcript",
            "provider": provider,
            "window_id": window_id,
            "publication_id": publication_id,
            "guild_id": guild_id,
            "voice_channel_id": voice_channel_id,
            "start_time": isoformat_z(Some(start)),
            "end_time": isoformat_z(Some(end)),
            "text_artifact_path": text_artifact_path.display().to_string(),
            "speaker_alignment_path": speaker_alignment_path.display().to_string(),
            "capture_run_ids": capture_run_ids,
            "voice_bot_ids": voice_bot_ids,
            "quality": "refined",
            "created_at": isoformat_z(None)
        });
        let created_ms = instant_ms_str(Some(&string_field(&span, "created_at"))).unwrap_or(0);
        let db = self.connect()?;
        self.ensure_room(&db, guild_id, voice_channel_id, "", "", "")?;
        db.execute(
            r#"
            INSERT INTO authoritative_spans(span_id, guild_id, voice_channel_id, window_id, publication_id, start_ms, end_ms, created_at_ms, payload_json)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
            "#,
            params![
                span_id,
                guild_id,
                voice_channel_id,
                window_id,
                publication_id,
                instant_ms_dt(start),
                instant_ms_dt(end),
                created_ms,
                json_dumps(&span)?
            ],
        )?;
        self.append_event(
            guild_id,
            voice_channel_id,
            serde_json::json!({
                "event_kind": "refinement_completed",
                "kind": "refinement_completed",
                "span_id": span_id,
                "window_id": window_id,
                "publication_id": publication_id,
                "provider": provider,
                "start_time": isoformat_z(Some(start)),
                "end_time": isoformat_z(Some(end))
            }),
        )?;
        Ok(span)
    }

    pub fn list_spans(
        &self,
        guild_id: &str,
        voice_channel_id: &str,
        start: Option<DateTime<Utc>>,
        end: Option<DateTime<Utc>>,
    ) -> Result<Vec<Value>> {
        let mut conditions = vec![
            "guild_id = ?".to_string(),
            "voice_channel_id = ?".to_string(),
        ];
        let mut params_values: Vec<Box<dyn ToSql>> = vec![
            Box::new(guild_id.to_string()),
            Box::new(voice_channel_id.to_string()),
        ];
        if let Some(start) = start {
            conditions.push("COALESCE(end_ms, start_ms) > ?".to_string());
            params_values.push(Box::new(instant_ms_dt(start)));
        }
        if let Some(end) = end {
            conditions.push("start_ms < ?".to_string());
            params_values.push(Box::new(instant_ms_dt(end)));
        }
        let sql = format!(
            "SELECT payload_json FROM authoritative_spans WHERE {} ORDER BY start_ms, span_id",
            conditions.join(" AND ")
        );
        let db = self.connect()?;
        let mut statement = db.prepare(&sql)?;
        let rows = statement.query_map(
            params_from_iter(params_values.iter().map(|value| &**value)),
            |row| payload_from_row(row, 0),
        )?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn event_covered_by_span(&self, event: &Value, spans: &[Value]) -> bool {
        let Some(started) = event_start(event) else {
            return false;
        };
        let ended = event_end(event).unwrap_or(started);
        spans.iter().any(|span| {
            let span_start = parse_instant(&string_field(span, "start_time"));
            let span_end = parse_instant(&string_field(span, "end_time"));
            overlaps(Some(started), Some(ended), span_start, span_end)
        })
    }
}

impl TimelineStore {
    pub fn export_mixed_audio(
        &self,
        guild_id: &str,
        voice_channel_id: &str,
        window_id: &str,
        job_id: &str,
    ) -> Result<Value> {
        let window = self.get_window(window_id)?;
        let start = parse_instant(&string_field(&window, "start_time"))
            .with_context(|| format!("window {window_id} has invalid start time"))?;
        let end = parse_instant(&string_field(&window, "end_time"))
            .with_context(|| format!("window {window_id} has invalid end time"))?;
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
        let sample_rate = 48_000u32;
        let total_frames = (((end - start).num_milliseconds() as f64 / 1000.0) * sample_rate as f64)
            .max(1.0) as usize;
        let mut mix = vec![0i32; total_frames];
        let mut local_segments = Vec::new();
        for event in events {
            let source = PathBuf::from(first_value_string(
                &event,
                &["source_audio_path", "sourceAudioPath"],
            ));
            if !source.is_file() {
                continue;
            }
            let Some(seg_start) = event_start(&event) else {
                continue;
            };
            let seg_end = event_end(&event).unwrap_or(seg_start);
            let clipped_start = start.max(seg_start);
            let clipped_end = end.min(seg_end);
            if clipped_start >= clipped_end {
                continue;
            }
            let offset_frames = (((clipped_start - start).num_milliseconds() as f64 / 1000.0)
                * sample_rate as f64)
                .max(0.0) as usize;
            let skip_frames = (((clipped_start - seg_start).num_milliseconds() as f64 / 1000.0)
                * sample_rate as f64)
                .max(0.0) as usize;
            let take_frames = (((clipped_end - clipped_start).num_milliseconds() as f64 / 1000.0)
                * sample_rate as f64)
                .max(0.0) as usize;
            let mut mono = read_wav_mono(&source, sample_rate)?;
            if skip_frames > 0 {
                mono = mono.into_iter().skip(skip_frames).collect();
            }
            if take_frames > 0 {
                mono.truncate(take_frames);
            }
            for (index, sample) in mono.into_iter().enumerate() {
                let target = offset_frames + index;
                if target >= mix.len() {
                    break;
                }
                mix[target] += sample as i32;
            }
            local_segments.push(serde_json::json!({
                "speaker_user_id": first_value_string(&event, &["speaker_user_id", "speakerId"]),
                "speaker_label": event_speaker(&event),
                "start_offset": round3((clipped_start - start).num_milliseconds() as f64 / 1000.0),
                "end_offset": round3((clipped_end - start).num_milliseconds() as f64 / 1000.0),
                "source_event_ids": [first_value_string(&event, &["event_id", "eventId"])]
            }));
        }
        let output_dir = self
            .channel_dir(guild_id, voice_channel_id)
            .join("jobs")
            .join(job_id);
        fs::create_dir_all(&output_dir)?;
        let mixed_path = output_dir.join("mixed.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&mixed_path, spec)?;
        for sample in mix {
            writer.write_sample(sample.clamp(-32768, 32767) as i16)?;
        }
        writer.finalize()?;
        let sidecar = serde_json::json!({
            "window_id": window_id,
            "mixed_audio_path": mixed_path.display().to_string(),
            "window_start_time": isoformat_z(Some(start)),
            "window_end_time": isoformat_z(Some(end)),
            "local_speaker_segments": local_segments,
            "audio_checksum": sha256_file(&mixed_path)?
        });
        write_json_file(&output_dir.join("mixed.sidecar.json"), &sidecar)?;
        Ok(sidecar)
    }
}
