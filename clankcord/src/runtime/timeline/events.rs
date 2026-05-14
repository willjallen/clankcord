use super::*;

impl TimelineStore {
    pub fn ensure_room(
        &self,
        db: &Connection,
        guild_id: &str,
        voice_channel_id: &str,
        guild_slug: &str,
        voice_channel_name: &str,
        voice_channel_slug: &str,
    ) -> Result<()> {
        if guild_id.is_empty() || voice_channel_id.is_empty() {
            return Ok(());
        }
        let now_ms = instant_ms_dt(utc_now());
        db.execute(
            r#"
            INSERT INTO voice_rooms(guild_id, voice_channel_id, guild_slug, voice_channel_name, voice_channel_slug, updated_at_ms)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            ON CONFLICT(guild_id, voice_channel_id) DO UPDATE SET
              guild_slug = COALESCE(NULLIF(excluded.guild_slug, ''), voice_rooms.guild_slug),
              voice_channel_name = COALESCE(NULLIF(excluded.voice_channel_name, ''), voice_rooms.voice_channel_name),
              voice_channel_slug = COALESCE(NULLIF(excluded.voice_channel_slug, ''), voice_rooms.voice_channel_slug),
              updated_at_ms = excluded.updated_at_ms
            "#,
            params![guild_id, voice_channel_id, guild_slug, voice_channel_name, voice_channel_slug, now_ms],
        )?;
        fs::create_dir_all(self.channel_dir(guild_id, voice_channel_id))?;
        Ok(())
    }

    pub fn append_event(
        &self,
        guild_id: &str,
        voice_channel_id: &str,
        event: Value,
    ) -> Result<Value> {
        let mut payload = event.as_object().cloned().unwrap_or_default();
        set_default_string(&mut payload, "event_id", &new_id("evt"));
        let event_id = string_field_map(&payload, "event_id");
        set_default_string(&mut payload, "eventId", &event_id);
        set_default_string(&mut payload, "guild_id", guild_id);
        set_default_string(&mut payload, "guildId", guild_id);
        set_default_string(&mut payload, "voice_channel_id", voice_channel_id);
        set_default_string(&mut payload, "channelId", voice_channel_id);
        set_default_string(&mut payload, "created_at", &isoformat_z(None));
        let created_at = string_field_map(&payload, "created_at");
        set_default_string(&mut payload, "timestamp", &created_at);
        let kind = non_empty(
            string_field_map(&payload, "event_kind"),
            non_empty(string_field_map(&payload, "kind"), "event".to_string()),
        );
        set_default_string(&mut payload, "event_kind", &kind);
        set_default_string(&mut payload, "kind", &kind);
        let payload_value = Value::Object(payload.clone());
        let started_ms =
            event_started_ms(&payload_value).or_else(|| instant_ms_str(Some(&created_at)));
        let ended_ms = event_ended_ms(&payload_value).or(started_ms);
        let created_ms = instant_ms_str(Some(&created_at))
            .or(started_ms)
            .unwrap_or_else(|| instant_ms_dt(utc_now()));
        let text = event_text(&payload_value);
        let speaker = first_string(&payload, &["speaker_user_id", "speakerId", "user_id"]);
        let speaker_label = if !speaker.is_empty() || SPEECH_KINDS.contains(&kind.as_str()) {
            event_speaker(&payload_value)
        } else {
            string_field_map(&payload, "speaker_label")
        };
        let conversation_id = first_string(
            &payload,
            &[
                "conversation_id",
                "conversationId",
                "provisional_conversation_id",
            ],
        );
        let capture_run_id = first_string(&payload, &["capture_run_id", "captureRunId"]);
        let db = self.connect()?;
        self.ensure_room(
            &db,
            guild_id,
            voice_channel_id,
            &first_string(&payload, &["guild_slug", "guildSlug"]),
            &first_string(&payload, &["voice_channel_name", "channelName"]),
            &first_string(&payload, &["voice_channel_slug", "channelSlug"]),
        )?;
        db.execute(
            r#"
            INSERT INTO timeline_events(
              event_id, guild_id, voice_channel_id, event_kind, started_at_ms, ended_at_ms,
              created_at_ms, capture_run_id, conversation_id, speaker_user_id, speaker_label, text, payload_json
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
            "#,
            params![
                event_id,
                guild_id,
                voice_channel_id,
                kind,
                started_ms,
                ended_ms,
                created_ms,
                capture_run_id,
                conversation_id,
                speaker,
                speaker_label,
                text,
                json_dumps(&compact_timeline_payload(&Value::Object(payload.clone()), &kind))?
            ],
        )?;
        if self.fts_enabled && SPEECH_KINDS.contains(&kind.as_str()) && !text.is_empty() {
            let _ = db.execute(
                "INSERT INTO transcript_events_fts(event_id, guild_id, voice_channel_id, speaker_label, text) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![event_id, guild_id, voice_channel_id, speaker_label, text],
            );
        }
        Ok(Value::Object(payload))
    }

    pub fn append_participant_event(
        &self,
        guild_id: &str,
        voice_channel_id: &str,
        event: Value,
    ) -> Result<Value> {
        self.append_event(guild_id, voice_channel_id, event)
    }

    pub fn load_events(
        &self,
        guild_id: &str,
        voice_channel_id: &str,
        start: Option<DateTime<Utc>>,
        end: Option<DateTime<Utc>>,
        kinds: Option<&BTreeSet<String>>,
        capture_run_id: Option<&str>,
        include_forgotten: bool,
    ) -> Result<Vec<Value>> {
        let mut conditions = vec![
            "e.guild_id = ?".to_string(),
            "e.voice_channel_id = ?".to_string(),
        ];
        let mut params_values: Vec<Box<dyn ToSql>> = vec![
            Box::new(guild_id.to_string()),
            Box::new(voice_channel_id.to_string()),
        ];
        if let Some(kinds) = kinds {
            if kinds.is_empty() {
                return Ok(Vec::new());
            }
            conditions.push(format!(
                "e.event_kind IN ({})",
                std::iter::repeat("?")
                    .take(kinds.len())
                    .collect::<Vec<_>>()
                    .join(",")
            ));
            for kind in kinds {
                params_values.push(Box::new(kind.clone()));
            }
        }
        if let Some(capture_run_id) = capture_run_id.filter(|value| !value.is_empty()) {
            conditions.push("e.capture_run_id = ?".to_string());
            params_values.push(Box::new(capture_run_id.to_string()));
        }
        if let Some(start) = start {
            conditions
                .push("COALESCE(e.ended_at_ms, e.started_at_ms, e.created_at_ms) > ?".to_string());
            params_values.push(Box::new(instant_ms_dt(start)));
        }
        if let Some(end) = end {
            conditions.push("COALESCE(e.started_at_ms, e.created_at_ms) < ?".to_string());
            params_values.push(Box::new(instant_ms_dt(end)));
        }
        if !include_forgotten {
            conditions.push("e.forgotten = 0".to_string());
        }
        let sql = format!(
            r#"
            SELECT e.*,
                   r.guild_slug AS room_guild_slug,
                   r.voice_channel_name AS room_voice_channel_name,
                   r.voice_channel_slug AS room_voice_channel_slug
            FROM timeline_events e
            LEFT JOIN voice_rooms r
              ON r.guild_id = e.guild_id AND r.voice_channel_id = e.voice_channel_id
            WHERE {}
            ORDER BY COALESCE(e.started_at_ms, e.created_at_ms), e.sequence, e.event_id
            "#,
            conditions.join(" AND ")
        );
        let db = self.connect()?;
        let mut statement = db.prepare(&sql)?;
        let rows = statement.query_map(
            params_from_iter(params_values.iter().map(|value| &**value)),
            |row| timeline_event_payload(row),
        )?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn get_event(&self, event_id: &str) -> Result<Value> {
        let db = self.connect()?;
        let payload = db.query_row(
            r#"
            SELECT e.*,
                   r.guild_slug AS room_guild_slug,
                   r.voice_channel_name AS room_voice_channel_name,
                   r.voice_channel_slug AS room_voice_channel_slug
            FROM timeline_events e
            LEFT JOIN voice_rooms r
              ON r.guild_id = e.guild_id AND r.voice_channel_id = e.voice_channel_id
            WHERE e.event_id = ?1
            "#,
            params![event_id],
            timeline_event_payload,
        )?;
        Ok(payload)
    }

    pub fn speech_event_for_segment(
        &self,
        guild_id: &str,
        voice_channel_id: &str,
        capture_run_id: &str,
        speaker_user_id: &str,
        segment_index: i64,
    ) -> Result<Option<Value>> {
        let kinds = set(["speech_segment"]);
        Ok(self
            .load_events(
                guild_id,
                voice_channel_id,
                None,
                None,
                Some(&kinds),
                Some(capture_run_id),
                false,
            )?
            .into_iter()
            .find(|event| {
                first_value_string(event, &["speaker_user_id", "speakerId"]) == speaker_user_id
                    && string_field(event, "segment_index")
                        .parse::<i64>()
                        .or_else(|_| string_field(event, "segmentIndex").parse::<i64>())
                        == Ok(segment_index)
            }))
    }

    pub fn speech_stats_for_capture_run(
        &self,
        guild_id: &str,
        voice_channel_id: &str,
        capture_run_id: &str,
    ) -> Result<(i64, Option<DateTime<Utc>>)> {
        let kinds = set(["speech_segment"]);
        let events = self.load_events(
            guild_id,
            voice_channel_id,
            None,
            None,
            Some(&kinds),
            Some(capture_run_id),
            false,
        )?;
        let last = events.iter().filter_map(event_end).max();
        Ok((events.len() as i64, last))
    }

    pub fn append_speech_event(&self, input: SpeechEventInput) -> Result<Value> {
        let event_id = new_id("evt");
        let mut source_path = input.source_audio_path.display().to_string();
        if source_path == "." {
            source_path.clear();
        }
        let (conversation_id, gap_ms) = self.conversation_for_speech(
            &input.guild_id,
            &input.voice_channel_id,
            &event_id,
            input.segment_start_time,
            input.segment_end_time,
            &input.speaker_user_id,
            &input.speaker_label,
            &input.text_draft,
        )?;
        let stt_model = if input.stt_model.is_empty() {
            string_field(&input.stt_metadata, "model")
        } else {
            input.stt_model
        };
        let payload = serde_json::json!({
            "event_id": event_id,
            "eventId": event_id,
            "event_kind": "speech_segment",
            "kind": "speech_segment",
            "capture_run_id": input.capture_run_id,
            "captureRunId": input.capture_run_id,
            "guild_id": input.guild_id,
            "guildId": input.guild_id,
            "guild_slug": input.guild_slug,
            "guildSlug": input.guild_slug,
            "voice_channel_id": input.voice_channel_id,
            "channelId": input.voice_channel_id,
            "voice_channel_name": input.voice_channel_name,
            "channelName": input.voice_channel_name,
            "voice_channel_slug": input.voice_channel_slug,
            "channelSlug": input.voice_channel_slug,
            "voice_bot_id": input.voice_bot_id,
            "botId": input.voice_bot_id,
            "voice_bot_discord_user_id": input.voice_bot_discord_user_id,
            "botUserId": input.voice_bot_discord_user_id,
            "speaker_user_id": input.speaker_user_id,
            "speakerId": input.speaker_user_id,
            "speaker_label": input.speaker_label,
            "speakerLabel": input.speaker_label,
            "speaker_username": input.speaker_username,
            "speakerUsername": input.speaker_username,
            "segment_start_time": isoformat_z(Some(input.segment_start_time)),
            "startedAt": isoformat_z(Some(input.segment_start_time)),
            "segment_end_time": isoformat_z(Some(input.segment_end_time)),
            "endedAt": isoformat_z(Some(input.segment_end_time)),
            "text_draft": input.text_draft,
            "text": input.text_draft,
            "quality": "draft",
            "stt_provider": input.stt_provider,
            "stt_model": stt_model,
            "stt": input.stt_metadata,
            "wake": input.wake_metadata,
            "source_audio_path": source_path,
            "sourceAudioPath": source_path,
            "audio_checksum": input.audio_checksum,
            "audioChecksum": input.audio_checksum,
            "segment_index": input.segment_index,
            "segmentIndex": input.segment_index,
            "duration_ms": input.duration_ms,
            "durationMs": input.duration_ms,
            "gap_since_previous_speech_ms": gap_ms,
            "provisional_conversation_id": conversation_id,
            "conversationId": conversation_id,
            "created_at": isoformat_z(None)
        });
        self.append_event(&input.guild_id, &input.voice_channel_id, payload)
    }
}

impl TimelineStore {
    pub fn create_capture_run(&self, input: CaptureRunInput) -> Result<Value> {
        let started = input.started_at.unwrap_or_else(utc_now);
        let capture_run_id = new_id("cap");
        let assignment_id = new_id("assign");
        let policy = input.retention_policy.unwrap_or_else(|| {
            serde_json::json!({
                "draft_transcript_events": "7d",
                "source_audio": "7d",
                "job_metadata": "30d"
            })
        });
        let run = serde_json::json!({
            "capture_run_id": capture_run_id,
            "captureRunId": capture_run_id,
            "assignment_id": assignment_id,
            "assignmentId": assignment_id,
            "guild_id": input.guild_id,
            "guildId": input.guild_id,
            "guild_slug": input.guild_slug,
            "guildSlug": input.guild_slug,
            "voice_channel_id": input.voice_channel_id,
            "channelId": input.voice_channel_id,
            "voice_channel_name": input.voice_channel_name,
            "channelName": input.voice_channel_name,
            "voice_channel_slug": input.voice_channel_slug,
            "channelSlug": input.voice_channel_slug,
            "voice_bot_id": input.voice_bot_id,
            "botId": input.voice_bot_id,
            "voice_bot_discord_user_id": input.voice_bot_discord_user_id,
            "botUserId": input.voice_bot_discord_user_id,
            "started_at": isoformat_z(Some(started)),
            "startedAt": isoformat_z(Some(started)),
            "ended_at": Value::Null,
            "endedAt": "",
            "state": "active",
            "mode": input.mode,
            "retention_policy": policy,
            "retentionPolicy": policy
        });
        let assignment = serde_json::json!({
            "assignment_id": assignment_id,
            "guild_id": input.guild_id,
            "voice_channel_id": input.voice_channel_id,
            "voice_channel_name": input.voice_channel_name,
            "voice_bot_id": input.voice_bot_id,
            "voice_bot_discord_user_id": input.voice_bot_discord_user_id,
            "capture_run_id": capture_run_id,
            "state": "capturing",
            "mode": input.mode,
            "assigned_at": isoformat_z(Some(started)),
            "released_at": Value::Null,
            "assignment_reason": input.reason
        });
        let now_ms = instant_ms_dt(started);
        let db = self.connect()?;
        self.ensure_room(
            &db,
            &input.guild_id,
            &input.voice_channel_id,
            &input.guild_slug,
            &input.voice_channel_name,
            &input.voice_channel_slug,
        )?;
        db.execute(
            r#"
            INSERT INTO capture_runs(
              capture_run_id, guild_id, voice_channel_id, voice_bot_id, started_at_ms,
              ended_at_ms, state, mode, updated_at_ms, payload_json
            )
            VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6, ?7, ?8, ?9)
            "#,
            params![
                capture_run_id,
                input.guild_id,
                input.voice_channel_id,
                input.voice_bot_id,
                now_ms,
                "active",
                input.mode,
                now_ms,
                json_dumps(&run)?
            ],
        )?;
        db.execute(
            r#"
            INSERT INTO assignments(
              assignment_id, guild_id, voice_channel_id, voice_bot_id, capture_run_id,
              state, assigned_at_ms, released_at_ms, updated_at_ms, payload_json
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, ?8, ?9)
            "#,
            params![
                assignment_id,
                input.guild_id,
                input.voice_channel_id,
                input.voice_bot_id,
                capture_run_id,
                "capturing",
                now_ms,
                now_ms,
                json_dumps(&assignment)?
            ],
        )?;
        self.append_event(
            &input.guild_id,
            &input.voice_channel_id,
            serde_json::json!({
                "event_kind": "voice_bot_assigned",
                "kind": "voice_bot_assigned",
                "assignment_id": assignment_id,
                "capture_run_id": capture_run_id,
                "voice_bot_id": input.voice_bot_id,
                "voice_bot_discord_user_id": input.voice_bot_discord_user_id,
                "voice_channel_name": input.voice_channel_name,
                "assigned_at": isoformat_z(Some(started)),
                "mode": input.mode,
                "assignment_reason": input.reason
            }),
        )?;
        Ok(run)
    }

    pub fn close_capture_run(
        &self,
        guild_id: &str,
        voice_channel_id: &str,
        capture_run_id: &str,
        ended_at: Option<DateTime<Utc>>,
        reason: &str,
        state: &str,
    ) -> Result<Value> {
        if capture_run_id.trim().is_empty() {
            return Ok(serde_json::json!({}));
        }
        let db = self.connect()?;
        let raw: Option<String> = db
            .query_row(
                "SELECT payload_json FROM capture_runs WHERE capture_run_id = ?1",
                params![capture_run_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(raw) = raw else {
            return Ok(serde_json::json!({}));
        };
        let mut run = serde_json::from_str::<Value>(&raw).unwrap_or_else(|_| serde_json::json!({}));
        let ended = ended_at.unwrap_or_else(utc_now);
        update_value_object(
            &mut run,
            [
                ("ended_at", Value::String(isoformat_z(Some(ended)))),
                ("endedAt", Value::String(isoformat_z(Some(ended)))),
                ("state", Value::String(state.to_string())),
                ("release_reason", Value::String(reason.to_string())),
            ],
        );
        let ended_ms = instant_ms_dt(ended);
        db.execute(
            r#"
            UPDATE capture_runs
            SET ended_at_ms = ?1, state = ?2, updated_at_ms = ?3, payload_json = ?4
            WHERE capture_run_id = ?5
            "#,
            params![ended_ms, state, ended_ms, json_dumps(&run)?, capture_run_id],
        )?;
        let assignment_id = first_value_string(&run, &["assignment_id", "assignmentId"]);
        self.release_assignment(&assignment_id, Some(ended), reason)?;
        self.append_event(
            &non_empty(string_field(&run, "guild_id"), guild_id.to_string()),
            &non_empty(
                string_field(&run, "voice_channel_id"),
                voice_channel_id.to_string(),
            ),
            serde_json::json!({
                "event_kind": "voice_bot_released",
                "kind": "voice_bot_released",
                "assignment_id": assignment_id,
                "capture_run_id": capture_run_id,
                "voice_bot_id": first_value_string(&run, &["voice_bot_id", "botId"]),
                "released_at": isoformat_z(Some(ended)),
                "release_reason": reason,
                "state": state,
            }),
        )?;
        Ok(run)
    }

    fn release_assignment(
        &self,
        assignment_id: &str,
        released_at: Option<DateTime<Utc>>,
        reason: &str,
    ) -> Result<()> {
        if assignment_id.trim().is_empty() {
            return Ok(());
        }
        let db = self.connect()?;
        let raw: Option<String> = db
            .query_row(
                "SELECT payload_json FROM assignments WHERE assignment_id = ?1",
                params![assignment_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(raw) = raw else {
            return Ok(());
        };
        let mut assignment =
            serde_json::from_str::<Value>(&raw).unwrap_or_else(|_| serde_json::json!({}));
        let released = released_at.unwrap_or_else(utc_now);
        update_value_object(
            &mut assignment,
            [
                ("released_at", Value::String(isoformat_z(Some(released)))),
                ("state", Value::String("released".to_string())),
                ("release_reason", Value::String(reason.to_string())),
            ],
        );
        let updated_ms = instant_ms_dt(released);
        let assigned_ms = instant_ms_str(Some(&string_field(&assignment, "assigned_at")));
        let released_ms = instant_ms_str(Some(&string_field(&assignment, "released_at")));
        db.execute(
            r#"
            INSERT INTO assignments(
              assignment_id, guild_id, voice_channel_id, voice_bot_id, capture_run_id,
              state, assigned_at_ms, released_at_ms, updated_at_ms, payload_json
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            ON CONFLICT(assignment_id) DO UPDATE SET
              guild_id = excluded.guild_id,
              voice_channel_id = excluded.voice_channel_id,
              voice_bot_id = excluded.voice_bot_id,
              capture_run_id = excluded.capture_run_id,
              state = excluded.state,
              assigned_at_ms = excluded.assigned_at_ms,
              released_at_ms = excluded.released_at_ms,
              updated_at_ms = excluded.updated_at_ms,
              payload_json = excluded.payload_json
            "#,
            params![
                assignment_id,
                string_field(&assignment, "guild_id"),
                string_field(&assignment, "voice_channel_id"),
                string_field(&assignment, "voice_bot_id"),
                string_field(&assignment, "capture_run_id"),
                string_field(&assignment, "state"),
                assigned_ms,
                released_ms,
                updated_ms,
                json_dumps(&assignment)?
            ],
        )?;
        Ok(())
    }
}

impl TimelineStore {
    pub fn conversation_for_speech(
        &self,
        guild_id: &str,
        voice_channel_id: &str,
        event_id: &str,
        started_at: DateTime<Utc>,
        ended_at: DateTime<Utc>,
        speaker_user_id: &str,
        speaker_label: &str,
        text: &str,
    ) -> Result<(String, Option<i64>)> {
        let db = self.connect()?;
        let row: Option<String> = db
            .query_row(
                r#"
                SELECT payload_json FROM conversations
                WHERE guild_id = ?1 AND voice_channel_id = ?2 AND state = 'ephemeral'
                ORDER BY COALESCE(last_speech_at_ms, end_ms, start_ms) DESC
                LIMIT 1
                "#,
                params![guild_id, voice_channel_id],
                |row| row.get(0),
            )
            .optional()?;
        let mut conversation = row
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
            .and_then(|value| value.as_object().cloned())
            .unwrap_or_default();
        let mut active_id = string_field_map(&conversation, "conversation_id");
        let last_speech_at = parse_instant(&non_empty(
            string_field_map(&conversation, "end_time"),
            string_field_map(&conversation, "last_speech_at"),
        ));
        let gap_ms = last_speech_at.map(|last| ((started_at - last).num_milliseconds()).max(0));
        let new_conversation =
            active_id.is_empty() || gap_ms.is_none() || gap_ms.unwrap_or(0) >= 15 * 60 * 1000;
        if new_conversation {
            active_id = new_id("conv");
            conversation = serde_json::json!({
                "conversation_id": active_id,
                "guild_id": guild_id,
                "voice_channel_id": voice_channel_id,
                "event_id_start": event_id,
                "event_id_end": event_id,
                "start_time": isoformat_z(Some(started_at)),
                "end_time": isoformat_z(Some(ended_at)),
                "participants": if speaker_user_id.is_empty() { Value::Array(vec![]) } else { Value::Array(vec![Value::String(speaker_user_id.to_string())]) },
                "participant_labels": if speaker_user_id.is_empty() { Value::Object(Map::new()) } else { serde_json::json!({speaker_user_id: speaker_label}) },
                "title": "",
                "topic_labels": [],
                "summary_draft": "",
                "state": "ephemeral",
                "transcript_quality": "draft"
            })
            .as_object()
            .cloned()
            .unwrap();
            self.store_conversation(&Value::Object(conversation.clone()))?;
            self.append_event(
                guild_id,
                voice_channel_id,
                serde_json::json!({
                    "event_kind": "conversation_started",
                    "kind": "conversation_started",
                    "conversation_id": active_id,
                    "start_time": isoformat_z(Some(started_at)),
                    "reason": if gap_ms.is_none() { "initial_speech" } else { "speech_gap" },
                    "gap_since_previous_speech_ms": gap_ms
                }),
            )?;
        }
        let mut participants = conversation
            .get("participants")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if !speaker_user_id.is_empty()
            && !participants
                .iter()
                .any(|value| value.as_str() == Some(speaker_user_id))
        {
            participants.push(Value::String(speaker_user_id.to_string()));
        }
        let mut labels = conversation
            .remove("participant_labels")
            .and_then(|value| value.as_object().cloned())
            .unwrap_or_default();
        if !speaker_user_id.is_empty() {
            labels.insert(
                speaker_user_id.to_string(),
                Value::String(speaker_label.to_string()),
            );
        }
        conversation.insert(
            "conversation_id".to_string(),
            Value::String(active_id.clone()),
        );
        conversation.insert(
            "event_id_end".to_string(),
            Value::String(event_id.to_string()),
        );
        conversation.insert(
            "end_time".to_string(),
            Value::String(isoformat_z(Some(ended_at))),
        );
        conversation.insert("participants".to_string(), Value::Array(participants));
        conversation.insert("participant_labels".to_string(), Value::Object(labels));
        if string_field_map(&conversation, "title").is_empty() && !text.is_empty() {
            conversation.insert(
                "title".to_string(),
                Value::String(text.chars().take(80).collect()),
            );
        }
        conversation.insert(
            "last_speech_at".to_string(),
            Value::String(isoformat_z(Some(ended_at))),
        );
        self.store_conversation(&Value::Object(conversation))?;
        Ok((active_id, gap_ms))
    }

    pub fn store_conversation(&self, conversation: &Value) -> Result<()> {
        let conversation_id = string_field(conversation, "conversation_id");
        let guild_id = string_field(conversation, "guild_id");
        let channel_id = string_field(conversation, "voice_channel_id");
        if conversation_id.is_empty() || guild_id.is_empty() || channel_id.is_empty() {
            return Ok(());
        }
        let start_ms = instant_ms_str(Some(&string_field(conversation, "start_time")));
        let end_ms = instant_ms_str(Some(&string_field(conversation, "end_time")));
        let last_ms =
            instant_ms_str(Some(&string_field(conversation, "last_speech_at"))).or(end_ms);
        let db = self.connect()?;
        self.ensure_room(&db, &guild_id, &channel_id, "", "", "")?;
        db.execute(
            r#"
            INSERT INTO conversations(conversation_id, guild_id, voice_channel_id, start_ms, end_ms, last_speech_at_ms, state, payload_json)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            ON CONFLICT(conversation_id) DO UPDATE SET
              start_ms = excluded.start_ms,
              end_ms = excluded.end_ms,
              last_speech_at_ms = excluded.last_speech_at_ms,
              state = excluded.state,
              payload_json = excluded.payload_json
            "#,
            params![
                conversation_id,
                guild_id,
                channel_id,
                start_ms,
                end_ms,
                last_ms,
                string_field(conversation, "state"),
                json_dumps(conversation)?
            ],
        )?;
        Ok(())
    }
}

impl TimelineStore {
    pub fn list_conversations(
        &self,
        guild_id: &str,
        voice_channel_id: Option<&str>,
        since: Option<DateTime<Utc>>,
    ) -> Result<Vec<Value>> {
        let mut conditions = vec!["guild_id = ?".to_string()];
        let mut params_values: Vec<Box<dyn ToSql>> = vec![Box::new(guild_id.to_string())];
        if let Some(channel_id) = voice_channel_id.filter(|value| !value.is_empty()) {
            conditions.push("voice_channel_id = ?".to_string());
            params_values.push(Box::new(channel_id.to_string()));
        }
        if let Some(since) = since {
            conditions.push("COALESCE(end_ms, start_ms) >= ?".to_string());
            params_values.push(Box::new(instant_ms_dt(since)));
        }
        let sql = format!(
            "SELECT payload_json FROM conversations WHERE {} ORDER BY COALESCE(start_ms, 0) DESC",
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
}

impl TimelineStore {
    pub fn set_occupancy(&self, snapshot: Value) -> Result<Value> {
        let guild_id = first_value_string(&snapshot, &["guild_id", "guildId"]);
        let channel_id = first_value_string(&snapshot, &["voice_channel_id", "channelId"]);
        if guild_id.is_empty() || channel_id.is_empty() {
            return Ok(snapshot);
        }
        let mut payload = self
            .get_occupancy(&guild_id, &channel_id)?
            .as_object()
            .cloned()
            .unwrap_or_default();
        for (key, value) in snapshot.as_object().cloned().unwrap_or_default() {
            payload.insert(key, value);
        }
        if !payload.contains_key("updated_at") {
            payload.insert("updated_at".to_string(), Value::String(isoformat_z(None)));
        }
        let payload_value = Value::Object(payload);
        let updated_ms =
            instant_ms_str(Some(&string_field(&payload_value, "updated_at"))).unwrap_or(0);
        let db = self.connect()?;
        self.ensure_room(
            &db,
            &guild_id,
            &channel_id,
            "",
            &first_value_string(&payload_value, &["voice_channel_name", "channelName"]),
            "",
        )?;
        db.execute(
            r#"
            INSERT INTO occupancy(guild_id, voice_channel_id, updated_at_ms, payload_json)
            VALUES (?1, ?2, ?3, ?4)
            ON CONFLICT(guild_id, voice_channel_id) DO UPDATE SET
              updated_at_ms = excluded.updated_at_ms,
              payload_json = excluded.payload_json
            "#,
            params![
                guild_id,
                channel_id,
                updated_ms,
                json_dumps(&payload_value)?
            ],
        )?;
        Ok(payload_value)
    }

    pub fn get_occupancy(&self, guild_id: &str, voice_channel_id: &str) -> Result<Value> {
        let db = self.connect()?;
        let row: Option<String> = db
            .query_row(
                "SELECT payload_json FROM occupancy WHERE guild_id = ?1 AND voice_channel_id = ?2",
                params![guild_id, voice_channel_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(row
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
            .unwrap_or_else(|| serde_json::json!({})))
    }
}
