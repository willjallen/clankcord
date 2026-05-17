use super::*;
use serde_json::json;

fn voice_state_transition_events(
    previous: Option<&Value>,
    current: &Value,
) -> Vec<(String, Value)> {
    let previous_channel_id = previous
        .map(|state| {
            first_value_string(state, &["voice_channel_id", "voiceChannelId", "channelId"])
        })
        .unwrap_or_default();
    let current_channel_id = first_value_string(
        current,
        &["voice_channel_id", "voiceChannelId", "channelId"],
    );
    let mut events = Vec::new();

    if previous_channel_id != current_channel_id {
        if !previous_channel_id.is_empty() {
            events.push((
                previous_channel_id.clone(),
                voice_transition_event("participant_left", previous, current),
            ));
        }
        if !current_channel_id.is_empty() {
            events.push((
                current_channel_id.clone(),
                voice_transition_event("participant_joined", previous, current),
            ));
        }
        if !previous_channel_id.is_empty() && !current_channel_id.is_empty() {
            events.push((
                current_channel_id.clone(),
                voice_transition_event("participant_moved", previous, current),
            ));
        }
        return events;
    }

    if previous.is_some() && !current_channel_id.is_empty() {
        for (event_kind, field, previous_value, current_value) in
            voice_flag_changes(previous, current)
        {
            if previous_value == current_value {
                continue;
            }
            let mut event = voice_transition_event(event_kind, previous, current);
            if let Value::Object(object) = &mut event {
                object.insert("field".to_string(), json!(field));
                object.insert("previous".to_string(), json!(previous_value));
                object.insert("current".to_string(), json!(current_value));
            }
            events.push((current_channel_id.clone(), event));
        }
    }

    events
}

fn voice_transition_event(event_kind: &str, previous: Option<&Value>, current: &Value) -> Value {
    let empty_previous = Value::Null;
    let previous = previous.unwrap_or(&empty_previous);
    let user_id = non_empty(
        first_value_string(current, &["user_id", "userId", "speaker_user_id"]),
        first_value_string(previous, &["user_id", "userId", "speaker_user_id"]),
    );
    let display_name = voice_display_name(current, previous, &user_id);
    let previous_channel_id = first_value_string(
        previous,
        &["voice_channel_id", "voiceChannelId", "channelId"],
    );
    let current_channel_id = first_value_string(
        current,
        &["voice_channel_id", "voiceChannelId", "channelId"],
    );
    json!({
        "event_kind": event_kind,
        "kind": event_kind,
        "created_at": string_field(current, "updated_at"),
        "updated_at": string_field(current, "updated_at"),
        "user_id": user_id,
        "userId": user_id,
        "speaker_user_id": user_id,
        "speaker_label": display_name,
        "display_name": display_name,
        "member_display_name": display_name,
        "username": first_value_string(current, &["username"]),
        "global_name": first_value_string(current, &["global_name", "globalName"]),
        "nick": first_value_string(current, &["nick"]),
        "voice_channel_id": current_channel_id,
        "previous_voice_channel_id": previous_channel_id,
        "current_voice_channel_id": current_channel_id,
        "from_voice_channel_id": previous_channel_id,
        "to_voice_channel_id": current_channel_id,
        "muted": voice_muted(current),
        "deafened": voice_deafened(current),
        "self_mute": voice_state_bool(current, "self_mute"),
        "server_mute": voice_state_bool(current, "mute"),
        "self_deaf": voice_state_bool(current, "self_deaf"),
        "server_deaf": voice_state_bool(current, "deaf"),
        "streaming": voice_state_bool(current, "self_stream"),
        "video": voice_state_bool(current, "self_video"),
        "suppress": voice_state_bool(current, "suppress"),
        "text": voice_event_text(event_kind, &display_name, &previous_channel_id, &current_channel_id),
    })
}

fn attach_event_room_snapshot(
    event: &mut Value,
    previous: Option<&Value>,
    current: &Value,
    after_occupants: &[Value],
) -> Result<()> {
    let event_kind = first_value_string(event, &["event_kind", "kind"]);
    let before_occupants = match event_kind.as_str() {
        "participant_joined" | "participant_moved" => {
            occupants_without_user(after_occupants, &voice_state_user_id(current))
        }
        "participant_left" => occupants_with_state(
            after_occupants,
            previous.context("participant_left event is missing previous voice state")?,
        ),
        "participant_mute_changed"
        | "participant_deafen_changed"
        | "participant_stream_changed"
        | "participant_video_changed"
        | "participant_suppress_changed" => occupants_with_state(
            after_occupants,
            previous.context("voice flag change event is missing previous voice state")?,
        ),
        _ => after_occupants.to_vec(),
    };
    let snapshot = json!({
        "before": room_snapshot(before_occupants),
        "after": room_snapshot(after_occupants.to_vec()),
    });
    if let Some(object) = event.as_object_mut() {
        object.insert("event_room".to_string(), snapshot);
    }
    Ok(())
}

fn room_snapshot(occupants: Vec<Value>) -> Value {
    let participants = room_participant_map(&occupants);
    json!({
        "liveOccupants": occupants,
        "participants": participants,
    })
}

fn room_participant_map(occupants: &[Value]) -> BTreeMap<String, Value> {
    occupants
        .iter()
        .filter_map(|occupant| {
            let user_id = voice_state_user_id(occupant);
            (!user_id.is_empty()).then(|| {
                (
                    user_id.clone(),
                    json!({
                        "present": true,
                        "user_id": user_id,
                        "display_name": first_value_string(occupant, &["display_name", "member_display_name", "global_name", "globalName", "username"]),
                        "username": first_value_string(occupant, &["username"]),
                    }),
                )
            })
        })
        .collect()
}

fn occupants_without_user(occupants: &[Value], user_id: &str) -> Vec<Value> {
    sorted_occupants(
        occupants
            .iter()
            .filter(|occupant| voice_state_user_id(occupant) != user_id)
            .cloned()
            .collect(),
    )
}

fn occupants_with_state(occupants: &[Value], state: &Value) -> Vec<Value> {
    let user_id = voice_state_user_id(state);
    let mut with_state = occupants_without_user(occupants, &user_id);
    with_state.push(state.clone());
    sorted_occupants(with_state)
}

fn sorted_occupants(mut occupants: Vec<Value>) -> Vec<Value> {
    occupants.sort_by(|left, right| {
        occupant_sort_key(left)
            .cmp(&occupant_sort_key(right))
            .then_with(|| voice_state_user_id(left).cmp(&voice_state_user_id(right)))
    });
    occupants
}

fn occupant_sort_key(occupant: &Value) -> String {
    non_empty(
        first_value_string(
            occupant,
            &[
                "display_name",
                "member_display_name",
                "global_name",
                "globalName",
                "username",
            ],
        ),
        voice_state_user_id(occupant),
    )
}

fn voice_state_user_id(state: &Value) -> String {
    first_value_string(state, &["user_id", "userId", "speaker_user_id"])
}

fn voice_flag_changes(
    previous: Option<&Value>,
    current: &Value,
) -> Vec<(&'static str, &'static str, bool, bool)> {
    let empty_previous = Value::Null;
    let previous = previous.unwrap_or(&empty_previous);
    vec![
        (
            "participant_mute_changed",
            "muted",
            voice_muted(previous),
            voice_muted(current),
        ),
        (
            "participant_deafen_changed",
            "deafened",
            voice_deafened(previous),
            voice_deafened(current),
        ),
        (
            "participant_stream_changed",
            "streaming",
            voice_state_bool(previous, "self_stream"),
            voice_state_bool(current, "self_stream"),
        ),
        (
            "participant_video_changed",
            "video",
            voice_state_bool(previous, "self_video"),
            voice_state_bool(current, "self_video"),
        ),
        (
            "participant_suppress_changed",
            "suppress",
            voice_state_bool(previous, "suppress"),
            voice_state_bool(current, "suppress"),
        ),
    ]
}

fn voice_display_name(current: &Value, previous: &Value, user_id: &str) -> String {
    non_empty(
        first_value_string(
            current,
            &[
                "member_display_name",
                "display_name",
                "displayName",
                "global_name",
                "username",
            ],
        ),
        non_empty(
            first_value_string(
                previous,
                &[
                    "member_display_name",
                    "display_name",
                    "displayName",
                    "global_name",
                    "username",
                ],
            ),
            user_id.to_string(),
        ),
    )
}

fn voice_muted(state: &Value) -> bool {
    voice_state_bool(state, "mute") || voice_state_bool(state, "self_mute")
}

fn voice_deafened(state: &Value) -> bool {
    voice_state_bool(state, "deaf") || voice_state_bool(state, "self_deaf")
}

fn voice_state_bool(state: &Value, key: &str) -> bool {
    state.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn voice_event_text(
    event_kind: &str,
    display_name: &str,
    previous_channel_id: &str,
    current_channel_id: &str,
) -> String {
    match event_kind {
        "participant_joined" => format!("{display_name} joined voice channel {current_channel_id}"),
        "participant_left" => format!("{display_name} left voice channel {previous_channel_id}"),
        "participant_moved" => {
            format!("{display_name} moved from {previous_channel_id} to {current_channel_id}")
        }
        "participant_mute_changed" => format!("{display_name} changed voice mute state"),
        "participant_deafen_changed" => format!("{display_name} changed voice deafen state"),
        "participant_stream_changed" => format!("{display_name} changed stream state"),
        "participant_video_changed" => format!("{display_name} changed video state"),
        "participant_suppress_changed" => format!("{display_name} changed suppress state"),
        _ => event_kind.to_string(),
    }
}

impl TimelineStore {
    pub async fn ensure_room(
        &self,
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
        sqlx::query(
            r#"
            INSERT INTO voice_rooms(guild_id, voice_channel_id, guild_slug, voice_channel_name, voice_channel_slug, updated_at_ms)
            VALUES ($1, $2, $3, $4, $5, $6)
            ON CONFLICT(guild_id, voice_channel_id) DO UPDATE SET
              guild_slug = COALESCE(NULLIF(EXCLUDED.guild_slug, ''), voice_rooms.guild_slug),
              voice_channel_name = COALESCE(NULLIF(EXCLUDED.voice_channel_name, ''), voice_rooms.voice_channel_name),
              voice_channel_slug = COALESCE(NULLIF(EXCLUDED.voice_channel_slug, ''), voice_rooms.voice_channel_slug),
              updated_at_ms = EXCLUDED.updated_at_ms
            "#,
        )
        .bind(guild_id)
        .bind(voice_channel_id)
        .bind(guild_slug)
        .bind(voice_channel_name)
        .bind(voice_channel_slug)
        .bind(now_ms)
        .execute(&self.pool)
        .await?;
        fs::create_dir_all(self.channel_dir(guild_id, voice_channel_id))?;
        Ok(())
    }

    pub async fn append_event(
        &self,
        guild_id: &str,
        voice_channel_id: &str,
        event: Value,
    ) -> Result<Value> {
        let scope = crate::runtime::RuntimeScope::voice_channel(guild_id, voice_channel_id);
        self.append_scope_event(&scope, event).await
    }

    pub async fn append_scope_event(
        &self,
        scope: &crate::runtime::RuntimeScope,
        event: Value,
    ) -> Result<Value> {
        let mut payload = event.as_object().cloned().unwrap_or_default();
        set_default_string(&mut payload, "event_id", &new_id("evt"));
        let event_id = string_field_map(&payload, "event_id");
        set_default_string(&mut payload, "eventId", &event_id);
        set_default_string(&mut payload, "scope_kind", scope.kind.as_str());
        set_default_string(&mut payload, "scope_id", &scope.scope_id);
        set_default_string(&mut payload, "guild_id", &scope.guild_id);
        set_default_string(&mut payload, "guildId", &scope.guild_id);
        if scope.kind == crate::runtime::RuntimeScopeKind::VoiceChannel {
            set_default_string(&mut payload, "voice_channel_id", &scope.scope_id);
            set_default_string(&mut payload, "channelId", &scope.scope_id);
        }
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
        let started_ms = event_started_ms(&payload_value)
            .or_else(|| instant_ms_str(Some(&created_at)))
            .unwrap_or_else(|| instant_ms_dt(utc_now()));
        let ended_ms = event_ended_ms(&payload_value).unwrap_or(started_ms);
        let created_ms = instant_ms_str(Some(&created_at)).unwrap_or(started_ms);
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
        if scope.kind == crate::runtime::RuntimeScopeKind::VoiceChannel {
            self.ensure_room(
                &scope.guild_id,
                &scope.scope_id,
                &first_string(&payload, &["guild_slug", "guildSlug"]),
                &first_string(&payload, &["voice_channel_name", "channelName"]),
                &first_string(&payload, &["voice_channel_slug", "channelSlug"]),
            )
            .await?;
        }
        sqlx::query(
            r#"
            INSERT INTO timeline_events(
              event_id, scope_kind, guild_id, scope_id, event_kind, started_at_ms, ended_at_ms,
              created_at_ms, capture_run_id, conversation_id, speaker_user_id, speaker_label, text, payload_json
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)
            "#,
        )
        .bind(event_id)
        .bind(scope.kind.as_str())
        .bind(&scope.guild_id)
        .bind(&scope.scope_id)
        .bind(kind.clone())
        .bind(started_ms)
        .bind(ended_ms)
        .bind(created_ms)
        .bind(capture_run_id)
        .bind(conversation_id)
        .bind(speaker)
        .bind(speaker_label)
        .bind(text)
        .bind(compact_timeline_payload(&Value::Object(payload.clone()), &kind))
        .execute(&self.pool)
        .await?;
        Ok(Value::Object(payload))
    }

    pub async fn append_participant_event(
        &self,
        guild_id: &str,
        voice_channel_id: &str,
        event: Value,
    ) -> Result<Value> {
        self.append_event(guild_id, voice_channel_id, event).await
    }

    pub async fn record_voice_state_update(
        &self,
        old_state: Option<Value>,
        new_state: Value,
    ) -> Result<Vec<Value>> {
        let guild_id = non_empty(
            first_value_string(&new_state, &["guild_id", "guildId"]),
            old_state
                .as_ref()
                .map(|state| first_value_string(state, &["guild_id", "guildId"]))
                .unwrap_or_default(),
        );
        let user_id = non_empty(
            first_value_string(&new_state, &["user_id", "userId", "speaker_user_id"]),
            old_state
                .as_ref()
                .map(|state| first_value_string(state, &["user_id", "userId", "speaker_user_id"]))
                .unwrap_or_default(),
        );
        if guild_id.is_empty() || user_id.is_empty() {
            return Ok(Vec::new());
        }

        let mut current_object = new_state.as_object().cloned().unwrap_or_default();
        set_default_string(&mut current_object, "guild_id", &guild_id);
        set_default_string(&mut current_object, "guildId", &guild_id);
        set_default_string(&mut current_object, "user_id", &user_id);
        set_default_string(&mut current_object, "userId", &user_id);
        set_default_string(&mut current_object, "speaker_user_id", &user_id);
        set_default_string(&mut current_object, "updated_at", &isoformat_z(None));
        let current = Value::Object(current_object);
        let current_channel_id = first_value_string(
            &current,
            &["voice_channel_id", "voiceChannelId", "channelId"],
        );
        let updated_ms = instant_ms_str(Some(&string_field(&current, "updated_at")))
            .unwrap_or_else(|| instant_ms_dt(utc_now()));

        let previous = {
            let mut transaction = self.pool.begin().await?;
            let previous_row = sqlx::query(
                r#"
                SELECT payload_json
                FROM voice_states
                WHERE guild_id = $1 AND user_id = $2
                FOR UPDATE
                "#,
            )
            .bind(&guild_id)
            .bind(&user_id)
            .fetch_optional(transaction.as_mut())
            .await?;
            let previous = previous_row
                .as_ref()
                .map(|row| json_value(row, "payload_json"))
                .transpose()?
                .or(old_state);
            sqlx::query(
                r#"
                INSERT INTO voice_states(guild_id, user_id, voice_channel_id, updated_at_ms, payload_json)
                VALUES ($1, $2, $3, $4, $5)
                ON CONFLICT(guild_id, user_id) DO UPDATE SET
                  voice_channel_id = EXCLUDED.voice_channel_id,
                  updated_at_ms = EXCLUDED.updated_at_ms,
                  payload_json = EXCLUDED.payload_json
                "#,
            )
            .bind(&guild_id)
            .bind(&user_id)
            .bind(&current_channel_id)
            .bind(updated_ms)
            .bind(&current)
            .execute(transaction.as_mut())
            .await?;
            transaction.commit().await?;
            previous
        };

        let transition_events = voice_state_transition_events(previous.as_ref(), &current);
        let mut after_occupants_by_channel = BTreeMap::new();
        for voice_channel_id in transition_events
            .iter()
            .map(|(voice_channel_id, _)| voice_channel_id.clone())
            .collect::<BTreeSet<_>>()
        {
            after_occupants_by_channel.insert(
                voice_channel_id.clone(),
                self.room_occupants(&guild_id, &voice_channel_id).await?,
            );
        }

        let mut appended = Vec::new();
        for (voice_channel_id, mut event) in transition_events {
            let after_occupants = after_occupants_by_channel
                .get(&voice_channel_id)
                .with_context(|| format!("missing room occupants for {voice_channel_id}"))?;
            attach_event_room_snapshot(&mut event, previous.as_ref(), &current, after_occupants)?;
            appended.push(
                self.append_participant_event(&guild_id, &voice_channel_id, event)
                    .await?,
            );
        }
        Ok(appended)
    }

    pub async fn room_occupants(
        &self,
        guild_id: &str,
        voice_channel_id: &str,
    ) -> Result<Vec<Value>> {
        let rows = sqlx::query(
            r#"
            SELECT payload_json
            FROM voice_states
            WHERE guild_id = $1 AND voice_channel_id = $2
            ORDER BY
              COALESCE(NULLIF(payload_json->>'display_name', ''), NULLIF(payload_json->>'username', ''), user_id),
              user_id
            "#,
        )
        .bind(guild_id)
        .bind(voice_channel_id)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|row| json_value(row, "payload_json"))
            .collect()
    }

    pub async fn voice_occupancy_snapshot(&self) -> Result<Value> {
        let rows = sqlx::query(
            r#"
            SELECT guild_id, voice_channel_id, payload_json
            FROM voice_states
            WHERE voice_channel_id <> ''
            ORDER BY
              guild_id,
              voice_channel_id,
              COALESCE(NULLIF(payload_json->>'display_name', ''), NULLIF(payload_json->>'username', ''), user_id),
              user_id
            "#,
        )
        .fetch_all(&self.pool)
        .await?;
        let mut rooms = BTreeMap::<String, (String, String, Vec<Value>)>::new();
        for row in rows {
            let guild_id: String = row.try_get("guild_id")?;
            let voice_channel_id: String = row.try_get("voice_channel_id")?;
            let occupant = json_value(&row, "payload_json")?;
            rooms
                .entry(format!("{guild_id}:{voice_channel_id}"))
                .or_insert_with(|| (guild_id, voice_channel_id, Vec::new()))
                .2
                .push(occupant);
        }
        let rooms = rooms
            .into_values()
            .map(|(guild_id, voice_channel_id, occupants)| {
                json!({
                    "guild_id": guild_id,
                    "voice_channel_id": voice_channel_id,
                    "occupants": occupants,
                })
            })
            .collect::<Vec<_>>();
        Ok(json!({"rooms": rooms}))
    }

    pub async fn load_events(
        &self,
        guild_id: &str,
        voice_channel_id: &str,
        start: Option<DateTime<Utc>>,
        end: Option<DateTime<Utc>>,
        kinds: Option<&BTreeSet<String>>,
        capture_run_id: Option<&str>,
        include_forgotten: bool,
    ) -> Result<Vec<Value>> {
        self.load_scope_events(
            crate::runtime::RuntimeScopeKind::VoiceChannel,
            guild_id,
            voice_channel_id,
            start,
            end,
            kinds,
            capture_run_id,
            include_forgotten,
        )
        .await
    }

    pub async fn load_scope_events(
        &self,
        scope_kind: crate::runtime::RuntimeScopeKind,
        guild_id: &str,
        scope_id: &str,
        start: Option<DateTime<Utc>>,
        end: Option<DateTime<Utc>>,
        kinds: Option<&BTreeSet<String>>,
        capture_run_id: Option<&str>,
        include_forgotten: bool,
    ) -> Result<Vec<Value>> {
        if kinds.is_some_and(BTreeSet::is_empty) {
            return Ok(Vec::new());
        }
        let mut query = QueryBuilder::<Postgres>::new(
            r#"
            SELECT e.*,
                   r.guild_slug AS room_guild_slug,
                   r.voice_channel_name AS room_voice_channel_name,
                   r.voice_channel_slug AS room_voice_channel_slug
            FROM timeline_events e
            LEFT JOIN voice_rooms r
              ON e.scope_kind = 'voice_channel'
             AND r.guild_id = e.guild_id
             AND r.voice_channel_id = e.scope_id
            WHERE e.guild_id =
            "#,
        );
        query.push_bind(guild_id);
        query
            .push(" AND e.scope_kind = ")
            .push_bind(scope_kind.as_str())
            .push(" AND e.scope_id = ")
            .push_bind(scope_id);
        if let Some(kinds) = kinds {
            query.push(" AND e.event_kind IN (");
            let mut separated = query.separated(", ");
            for kind in kinds {
                separated.push_bind(kind);
            }
            separated.push_unseparated(")");
        }
        if let Some(capture_run_id) = capture_run_id.filter(|value| !value.is_empty()) {
            query
                .push(" AND e.capture_run_id = ")
                .push_bind(capture_run_id);
        }
        if let Some(start) = start {
            query
                .push(" AND e.ended_at_ms > ")
                .push_bind(instant_ms_dt(start));
        }
        if let Some(end) = end {
            query
                .push(" AND e.started_at_ms < ")
                .push_bind(instant_ms_dt(end));
        }
        if !include_forgotten {
            query.push(" AND e.forgotten = FALSE");
        }
        query.push(" ORDER BY e.started_at_ms, e.sequence, e.event_id");
        let rows = query.build().fetch_all(&self.pool).await?;
        rows.iter().map(timeline_event_payload).collect()
    }

    pub async fn get_event(&self, event_id: &str) -> Result<Value> {
        let row = sqlx::query(
            r#"
            SELECT e.*,
                   r.guild_slug AS room_guild_slug,
                   r.voice_channel_name AS room_voice_channel_name,
                   r.voice_channel_slug AS room_voice_channel_slug
            FROM timeline_events e
            LEFT JOIN voice_rooms r
              ON e.scope_kind = 'voice_channel'
             AND r.guild_id = e.guild_id
             AND r.voice_channel_id = e.scope_id
            WHERE e.event_id = $1
            "#,
        )
        .bind(event_id)
        .fetch_one(&self.pool)
        .await?;
        timeline_event_payload(&row)
    }

    pub async fn speech_event_for_segment(
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
            )
            .await?
            .into_iter()
            .find(|event| {
                first_value_string(event, &["speaker_user_id", "speakerId"]) == speaker_user_id
                    && string_field(event, "segment_index")
                        .parse::<i64>()
                        .or_else(|_| string_field(event, "segmentIndex").parse::<i64>())
                        == Ok(segment_index)
            }))
    }

    pub async fn speech_stats_for_capture_run(
        &self,
        guild_id: &str,
        voice_channel_id: &str,
        capture_run_id: &str,
    ) -> Result<(i64, Option<DateTime<Utc>>)> {
        let kinds = set(["speech_segment"]);
        let events = self
            .load_events(
                guild_id,
                voice_channel_id,
                None,
                None,
                Some(&kinds),
                Some(capture_run_id),
                false,
            )
            .await?;
        let last = events.iter().filter_map(event_end).max();
        Ok((events.len() as i64, last))
    }

    pub async fn append_speech_event(&self, input: SpeechEventInput) -> Result<Value> {
        let event_id = new_id("evt");
        let mut source_path = input.source_audio_path.display().to_string();
        if source_path == "." {
            source_path.clear();
        }
        let (conversation_id, gap_ms) = self
            .conversation_for_speech(
                &input.guild_id,
                &input.voice_channel_id,
                &event_id,
                input.segment_start_time,
                input.segment_end_time,
                &input.speaker_user_id,
                &input.speaker_label,
                &input.text_draft,
            )
            .await?;
        let stt_model = if input.stt_model.is_empty() {
            string_field(&input.stt_metadata, "model")
        } else {
            input.stt_model.clone()
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
            .await
    }
}

impl TimelineStore {
    pub async fn create_capture_run(&self, input: CaptureRunInput) -> Result<Value> {
        let guild_id = input.guild_id.clone();
        let guild_slug = input.guild_slug.clone();
        let voice_channel_id = input.voice_channel_id.clone();
        let voice_channel_name = input.voice_channel_name.clone();
        let voice_channel_slug = input.voice_channel_slug.clone();
        let voice_bot_id = input.voice_bot_id.clone();
        let voice_bot_discord_user_id = input.voice_bot_discord_user_id.clone();
        let mode = input.mode.clone();
        let reason = input.reason.clone();
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
            "guild_id": guild_id,
            "guildId": guild_id,
            "guild_slug": guild_slug,
            "guildSlug": guild_slug,
            "voice_channel_id": voice_channel_id,
            "channelId": voice_channel_id,
            "voice_channel_name": voice_channel_name,
            "channelName": voice_channel_name,
            "voice_channel_slug": voice_channel_slug,
            "channelSlug": voice_channel_slug,
            "voice_bot_id": voice_bot_id,
            "botId": voice_bot_id,
            "voice_bot_discord_user_id": voice_bot_discord_user_id,
            "botUserId": voice_bot_discord_user_id,
            "started_at": isoformat_z(Some(started)),
            "startedAt": isoformat_z(Some(started)),
            "ended_at": Value::Null,
            "endedAt": "",
            "state": "joining",
            "mode": mode,
            "retention_policy": policy,
            "retentionPolicy": policy
        });
        let assignment = serde_json::json!({
            "assignment_id": assignment_id,
            "guild_id": guild_id,
            "voice_channel_id": voice_channel_id,
            "voice_channel_name": voice_channel_name,
            "voice_bot_id": voice_bot_id,
            "voice_bot_discord_user_id": voice_bot_discord_user_id,
            "capture_run_id": capture_run_id,
            "state": "joining",
            "mode": mode,
            "assigned_at": isoformat_z(Some(started)),
            "released_at": Value::Null,
            "assignment_reason": reason
        });
        let now_ms = instant_ms_dt(started);
        self.ensure_room(
            &guild_id,
            &voice_channel_id,
            &guild_slug,
            &voice_channel_name,
            &voice_channel_slug,
        )
        .await?;
        let mut transaction = self.pool.begin().await?;
        sqlx::query(
            r#"
            INSERT INTO capture_runs(
              capture_run_id, guild_id, voice_channel_id, voice_bot_id, started_at_ms,
              ended_at_ms, state, mode, updated_at_ms, payload_json
            )
            VALUES ($1, $2, $3, $4, $5, NULL, $6, $7, $8, $9)
            "#,
        )
        .bind(&capture_run_id)
        .bind(&guild_id)
        .bind(&voice_channel_id)
        .bind(&voice_bot_id)
        .bind(now_ms)
        .bind("joining")
        .bind(&mode)
        .bind(now_ms)
        .bind(&run)
        .execute(transaction.as_mut())
        .await?;
        sqlx::query(
            r#"
            INSERT INTO assignments(
              assignment_id, guild_id, voice_channel_id, voice_bot_id, capture_run_id,
              state, assigned_at_ms, released_at_ms, updated_at_ms, payload_json
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, NULL, $8, $9)
            "#,
        )
        .bind(&assignment_id)
        .bind(&guild_id)
        .bind(&voice_channel_id)
        .bind(&voice_bot_id)
        .bind(&capture_run_id)
        .bind("joining")
        .bind(now_ms)
        .bind(now_ms)
        .bind(&assignment)
        .execute(transaction.as_mut())
        .await?;
        transaction.commit().await?;
        self.append_event(
            &guild_id,
            &voice_channel_id,
            serde_json::json!({
                "event_kind": "voice_bot_assigned",
                "kind": "voice_bot_assigned",
                "assignment_id": assignment_id,
                "capture_run_id": capture_run_id,
                "voice_bot_id": voice_bot_id,
                "voice_bot_discord_user_id": voice_bot_discord_user_id,
                "voice_channel_name": voice_channel_name,
                "assigned_at": isoformat_z(Some(started)),
                "mode": mode,
                "assignment_reason": reason
            }),
        )
        .await?;
        Ok(run)
    }

    pub async fn close_capture_run(
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
        let row = sqlx::query("SELECT payload_json FROM capture_runs WHERE capture_run_id = $1")
            .bind(capture_run_id)
            .fetch_optional(&self.pool)
            .await?;
        let Some(row) = row else {
            return Ok(serde_json::json!({}));
        };
        let mut run = json_value(&row, "payload_json")?;
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
        sqlx::query(
            r#"
            UPDATE capture_runs
            SET ended_at_ms = $1, state = $2, updated_at_ms = $3, payload_json = $4
            WHERE capture_run_id = $5
            "#,
        )
        .bind(ended_ms)
        .bind(state)
        .bind(ended_ms)
        .bind(&run)
        .bind(capture_run_id)
        .execute(&self.pool)
        .await?;
        let assignment_id = first_value_string(&run, &["assignment_id", "assignmentId"]);
        self.release_assignment(&assignment_id, Some(ended), reason, state)
            .await?;
        self.mark_capture_session_ended(capture_run_id, ended)
            .await?;
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
        )
        .await?;
        Ok(run)
    }

    async fn release_assignment(
        &self,
        assignment_id: &str,
        released_at: Option<DateTime<Utc>>,
        reason: &str,
        state: &str,
    ) -> Result<()> {
        if assignment_id.trim().is_empty() {
            return Ok(());
        }
        let row = sqlx::query("SELECT payload_json FROM assignments WHERE assignment_id = $1")
            .bind(assignment_id)
            .fetch_optional(&self.pool)
            .await?;
        let Some(row) = row else {
            return Ok(());
        };
        let mut assignment = json_value(&row, "payload_json")?;
        let released = released_at.unwrap_or_else(utc_now);
        update_value_object(
            &mut assignment,
            [
                ("released_at", Value::String(isoformat_z(Some(released)))),
                ("releasedAt", Value::String(isoformat_z(Some(released)))),
                ("state", Value::String(state.to_string())),
                ("release_reason", Value::String(reason.to_string())),
                ("releaseReason", Value::String(reason.to_string())),
            ],
        );
        let updated_ms = instant_ms_dt(released);
        let assigned_ms = instant_ms_str(Some(&first_value_string(
            &assignment,
            &["assigned_at", "assignedAt"],
        )));
        let released_ms = instant_ms_str(Some(&first_value_string(
            &assignment,
            &["released_at", "releasedAt"],
        )));
        sqlx::query(
            r#"
            INSERT INTO assignments(
              assignment_id, guild_id, voice_channel_id, voice_bot_id, capture_run_id,
              state, assigned_at_ms, released_at_ms, updated_at_ms, payload_json
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
            ON CONFLICT(assignment_id) DO UPDATE SET
              guild_id = EXCLUDED.guild_id,
              voice_channel_id = EXCLUDED.voice_channel_id,
              voice_bot_id = EXCLUDED.voice_bot_id,
              capture_run_id = EXCLUDED.capture_run_id,
              state = EXCLUDED.state,
              assigned_at_ms = EXCLUDED.assigned_at_ms,
              released_at_ms = EXCLUDED.released_at_ms,
              updated_at_ms = EXCLUDED.updated_at_ms,
              payload_json = EXCLUDED.payload_json
            "#,
        )
        .bind(assignment_id)
        .bind(first_value_string(&assignment, &["guild_id", "guildId"]))
        .bind(first_value_string(
            &assignment,
            &["voice_channel_id", "voiceChannelId"],
        ))
        .bind(first_value_string(
            &assignment,
            &["voice_bot_id", "voiceBotId", "botId"],
        ))
        .bind(first_value_string(
            &assignment,
            &["capture_run_id", "captureRunId"],
        ))
        .bind(first_value_string(&assignment, &["state"]))
        .bind(assigned_ms)
        .bind(released_ms)
        .bind(updated_ms)
        .bind(&assignment)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

impl TimelineStore {
    pub async fn conversation_for_speech(
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
        let row = sqlx::query(
            r#"
            SELECT payload_json FROM conversations
            WHERE guild_id = $1
              AND scope_kind = 'voice_channel'
              AND scope_id = $2
              AND state = 'ephemeral'
            ORDER BY COALESCE(last_speech_at_ms, end_ms, start_ms) DESC
            LIMIT 1
            "#,
        )
        .bind(guild_id)
        .bind(voice_channel_id)
        .fetch_optional(&self.pool)
        .await?;
        let mut conversation = row
            .as_ref()
            .and_then(|row| json_value(row, "payload_json").ok())
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
            self.store_conversation(&Value::Object(conversation.clone()))
                .await?;
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
            )
            .await?;
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
        self.store_conversation(&Value::Object(conversation))
            .await?;
        Ok((active_id, gap_ms))
    }

    pub async fn store_conversation(&self, conversation: &Value) -> Result<()> {
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
        self.ensure_room(&guild_id, &channel_id, "", "", "").await?;
        sqlx::query(
            r#"
            INSERT INTO conversations(conversation_id, scope_kind, guild_id, scope_id, start_ms, end_ms, last_speech_at_ms, state, payload_json)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            ON CONFLICT(conversation_id) DO UPDATE SET
              scope_kind = EXCLUDED.scope_kind,
              guild_id = EXCLUDED.guild_id,
              scope_id = EXCLUDED.scope_id,
              start_ms = EXCLUDED.start_ms,
              end_ms = EXCLUDED.end_ms,
              last_speech_at_ms = EXCLUDED.last_speech_at_ms,
              state = EXCLUDED.state,
              payload_json = EXCLUDED.payload_json
            "#,
        )
        .bind(conversation_id)
        .bind("voice_channel")
        .bind(guild_id)
        .bind(channel_id)
        .bind(start_ms)
        .bind(end_ms)
        .bind(last_ms)
        .bind(string_field(conversation, "state"))
        .bind(conversation)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list_conversations(
        &self,
        guild_id: &str,
        voice_channel_id: Option<&str>,
        since: Option<DateTime<Utc>>,
    ) -> Result<Vec<Value>> {
        let mut query = QueryBuilder::<Postgres>::new(
            "SELECT payload_json FROM conversations WHERE guild_id = ",
        );
        query.push_bind(guild_id);
        if let Some(channel_id) = voice_channel_id.filter(|value| !value.is_empty()) {
            query
                .push(" AND scope_kind = 'voice_channel' AND scope_id = ")
                .push_bind(channel_id);
        }
        if let Some(since) = since {
            query
                .push(" AND COALESCE(end_ms, start_ms) >= ")
                .push_bind(instant_ms_dt(since));
        }
        query.push(" ORDER BY COALESCE(start_ms, 0) DESC");
        let rows = query.build().fetch_all(&self.pool).await?;
        rows.iter()
            .map(|row| json_value(row, "payload_json"))
            .collect()
    }
}

impl TimelineStore {
    pub async fn set_occupancy(&self, snapshot: Value) -> Result<Value> {
        let guild_id = first_value_string(&snapshot, &["guild_id", "guildId"]);
        let channel_id = first_value_string(&snapshot, &["voice_channel_id", "channelId"]);
        if guild_id.is_empty() || channel_id.is_empty() {
            return Ok(snapshot);
        }
        let mut payload = self
            .get_occupancy(&guild_id, &channel_id)
            .await?
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
        self.ensure_room(
            &guild_id,
            &channel_id,
            "",
            &first_value_string(&payload_value, &["voice_channel_name", "channelName"]),
            "",
        )
        .await?;
        sqlx::query(
            r#"
            INSERT INTO occupancy(guild_id, voice_channel_id, updated_at_ms, payload_json)
            VALUES ($1, $2, $3, $4)
            ON CONFLICT(guild_id, voice_channel_id) DO UPDATE SET
              updated_at_ms = EXCLUDED.updated_at_ms,
              payload_json = EXCLUDED.payload_json
            "#,
        )
        .bind(guild_id)
        .bind(channel_id)
        .bind(updated_ms)
        .bind(&payload_value)
        .execute(&self.pool)
        .await?;
        Ok(payload_value)
    }

    pub async fn get_occupancy(&self, guild_id: &str, voice_channel_id: &str) -> Result<Value> {
        let row = sqlx::query(
            "SELECT payload_json FROM occupancy WHERE guild_id = $1 AND voice_channel_id = $2",
        )
        .bind(guild_id)
        .bind(voice_channel_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(match row {
            Some(row) => json_value(&row, "payload_json")?,
            None => serde_json::json!({}),
        })
    }
}
