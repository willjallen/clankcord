use super::*;

#[derive(Debug, Clone, Copy)]
struct RetentionPolicy {
    transcript_events: RetentionWindow,
    source_audio: RetentionWindow,
    job_metadata: RetentionWindow,
}

#[derive(Debug, Clone, Copy)]
enum RetentionWindow {
    Forever,
    Duration(chrono::Duration),
}

#[derive(Debug, Clone)]
struct CaptureRunRetention {
    capture_run_id: String,
    guild_id: String,
    voice_channel_id: String,
    started_at_ms: i64,
    ended_at_ms: Option<i64>,
    session_dir: PathBuf,
    policy: RetentionPolicy,
}

#[derive(Debug, Clone)]
struct SourceAudioCandidate {
    guild_id: String,
    voice_channel_id: String,
}

#[derive(Debug, Clone)]
enum FiniteTranscriptRetentionScope {
    None,
    All,
    CaptureRuns(Vec<String>),
}

#[derive(Debug, Clone)]
enum FiniteJobMetadataRetentionScope {
    None,
    All,
    VoiceScopes(Vec<(String, String)>),
}

impl RetentionPolicy {
    fn default() -> Self {
        Self {
            transcript_events: RetentionWindow::Forever,
            source_audio: RetentionWindow::Duration(chrono::Duration::days(7)),
            job_metadata: RetentionWindow::Forever,
        }
    }

    fn from_value(value: &Value) -> Result<Self> {
        Ok(Self {
            transcript_events: retention_window_field(value, "transcript_events")?
                .unwrap_or(RetentionWindow::Forever),
            source_audio: retention_window_field(value, "source_audio")?
                .unwrap_or(RetentionWindow::Duration(chrono::Duration::days(7))),
            job_metadata: retention_window_field(value, "job_metadata")?
                .unwrap_or(RetentionWindow::Forever),
        })
    }
}

impl RetentionWindow {
    fn is_finite(self) -> bool {
        matches!(self, Self::Duration(_))
    }

    fn expires_at_ms(self, basis_ms: i64) -> Option<i64> {
        match self {
            Self::Forever => None,
            Self::Duration(duration) => Some(basis_ms.saturating_add(duration.num_milliseconds())),
        }
    }

    fn expired(self, current_ms: i64, basis_ms: i64) -> bool {
        self.expires_at_ms(basis_ms)
            .is_some_and(|expires_at_ms| expires_at_ms <= current_ms)
    }
}

fn retention_window_field(value: &Value, key: &str) -> Result<Option<RetentionWindow>> {
    let Some(raw) = value.get(key) else {
        return Ok(None);
    };
    let Some(text) = raw.as_str().map(str::trim) else {
        anyhow::bail!("retention policy field {key} must be a string");
    };
    if text.eq_ignore_ascii_case("forever") {
        return Ok(Some(RetentionWindow::Forever));
    }
    let Some(duration) = parse_duration(text) else {
        anyhow::bail!("retention policy field {key} has invalid duration {text}");
    };
    if duration <= chrono::Duration::zero() {
        anyhow::bail!("retention policy field {key} must be positive or forever");
    }
    Ok(Some(RetentionWindow::Duration(duration)))
}

impl TimelineStore {
    pub async fn channel_dirs(
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
        let rows = sqlx::query(
            "SELECT DISTINCT voice_channel_id FROM voice_rooms WHERE guild_id = $1 ORDER BY voice_channel_id",
        )
        .bind(guild_id)
        .fetch_all(&self.pool)
        .await?;
        let mut paths = Vec::new();
        for row in rows {
            let channel_id: String = row.try_get("voice_channel_id")?;
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

    pub async fn search(
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
        for channel_dir in self.channel_dirs(guild_id, voice_channel_id).await? {
            let channel_id = Self::channel_id_from_dir(&channel_dir);
            let spans = if prefer_refined {
                self.list_spans(guild_id, &channel_id, since, None).await?
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
            let events = self
                .search_draft_events(guild_id, &channel_id, query, since, limit * 2)
                .await?;
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

    pub async fn search_draft_events(
        &self,
        guild_id: &str,
        voice_channel_id: &str,
        query: &str,
        since: Option<DateTime<Utc>>,
        limit: usize,
    ) -> Result<Vec<Value>> {
        let kinds = set(["speech_segment", "transcript"]);
        let mut events = self
            .load_events(
                guild_id,
                voice_channel_id,
                since,
                None,
                Some(&kinds),
                None,
                false,
            )
            .await?;
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
    pub async fn apply_forget(
        &self,
        guild_id: &str,
        voice_channel_id: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        requested_by_user_id: &str,
        unpublished_only: bool,
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
        self.mark_events_forgotten(&event_ids).await?;
        let event = self
            .append_event(
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
            )
            .await?;
        Ok(serde_json::json!({
            "forgotten_event_count": events.len(),
            "deleted_audio_count": deleted_audio.len(),
            "event": event
        }))
    }

    pub async fn mark_events_forgotten(&self, event_ids: &[String]) -> Result<()> {
        let ids = event_ids
            .iter()
            .filter(|id| !id.is_empty())
            .cloned()
            .collect::<Vec<_>>();
        if ids.is_empty() {
            return Ok(());
        }
        sqlx::query("UPDATE timeline_events SET forgotten = TRUE WHERE event_id = ANY($1)")
            .bind(&ids)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn participant_trace(
        &self,
        guild_id: &str,
        user_id: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        include_speech_snippets: bool,
    ) -> Result<Vec<Value>> {
        let mut trace = Vec::new();
        for channel_dir in self.channel_dirs(guild_id, None).await? {
            let channel_id = Self::channel_id_from_dir(&channel_dir);
            let events = self
                .load_events(
                    guild_id,
                    &channel_id,
                    Some(start),
                    Some(end),
                    None,
                    None,
                    false,
                )
                .await?;
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
    pub async fn retention_sweep(
        &self,
        now: Option<DateTime<Utc>>,
        dry_run: bool,
    ) -> Result<Value> {
        let current = now.unwrap_or_else(utc_now);
        let current_ms = instant_ms_dt(current);
        let contexts = self.load_retention_contexts().await?;
        let default_policy = RetentionPolicy::default();

        let transcript_retirements = match finite_transcript_capture_runs(&contexts, default_policy)
        {
            FiniteTranscriptRetentionScope::None => Vec::new(),
            FiniteTranscriptRetentionScope::All => {
                self.retention_transcript_event_ids(&contexts, default_policy, current_ms, None)
                    .await?
            }
            FiniteTranscriptRetentionScope::CaptureRuns(capture_run_ids) => {
                self.retention_transcript_event_ids(
                    &contexts,
                    default_policy,
                    current_ms,
                    Some(&capture_run_ids),
                )
                .await?
            }
        };
        let transcript_event_candidates = transcript_retirements.len();
        if !transcript_retirements.is_empty() && !dry_run {
            let event_ids = transcript_retirements
                .iter()
                .map(|(_, _, event_id)| event_id.clone())
                .collect::<Vec<_>>();
            self.mark_events_forgotten(&event_ids).await?;
            let mut retired_by_channel: BTreeMap<(String, String), usize> = BTreeMap::new();
            for (guild_id, channel_id, _) in transcript_retirements {
                *retired_by_channel
                    .entry((guild_id, channel_id))
                    .or_default() += 1;
            }
            for ((guild_id, channel_id), channel_retired) in retired_by_channel {
                self.append_event(
                    &guild_id,
                    &channel_id,
                    serde_json::json!({
                        "event_kind": "retention_retired",
                        "kind": "retention_retired",
                        "retired_event_count": channel_retired
                    }),
                )
                .await?;
            }
        }

        let source_audio_candidates = self
            .retention_source_audio_candidates(&contexts, current_ms)
            .await?;
        let source_audio_candidate_count = source_audio_candidates.len();
        let mut deleted_audio = 0;
        if !dry_run {
            let mut retired_by_channel: BTreeMap<(String, String), usize> = BTreeMap::new();
            for (path, candidate) in source_audio_candidates {
                fs::remove_file(&path).with_context(|| {
                    format!("deleting retained source audio {}", path.display())
                })?;
                deleted_audio += 1;
                *retired_by_channel
                    .entry((candidate.guild_id, candidate.voice_channel_id))
                    .or_default() += 1;
            }
            for ((guild_id, channel_id), channel_retired) in retired_by_channel {
                self.append_event(
                    &guild_id,
                    &channel_id,
                    serde_json::json!({
                        "event_kind": "retention_retired",
                        "kind": "retention_retired",
                        "retired_source_audio_count": channel_retired
                    }),
                )
                .await?;
            }
        }

        let job_ids = match finite_job_metadata_scopes(&contexts, default_policy) {
            FiniteJobMetadataRetentionScope::None => Vec::new(),
            FiniteJobMetadataRetentionScope::All => {
                self.retention_terminal_job_ids(&contexts, default_policy, current_ms, None)
                    .await?
            }
            FiniteJobMetadataRetentionScope::VoiceScopes(scope_keys) => {
                self.retention_terminal_job_ids(
                    &contexts,
                    default_policy,
                    current_ms,
                    Some(&scope_keys),
                )
                .await?
            }
        };
        let job_candidates = job_ids.len();
        let deleted_jobs = if dry_run || job_ids.is_empty() {
            0
        } else {
            sqlx::query("DELETE FROM jobs WHERE job_id = ANY($1)")
                .bind(&job_ids)
                .execute(&self.pool)
                .await?
                .rows_affected() as usize
        };

        Ok(serde_json::json!({
            "transcript_event_candidates": transcript_event_candidates,
            "forgotten_events": if dry_run { 0 } else { transcript_event_candidates },
            "source_audio_candidates": source_audio_candidate_count,
            "deleted_audio": deleted_audio,
            "job_candidates": job_candidates,
            "deleted_jobs": deleted_jobs,
            "dry_run": dry_run
        }))
    }

    async fn load_retention_contexts(&self) -> Result<Vec<CaptureRunRetention>> {
        let rows = sqlx::query(
            r#"
            SELECT capture_run_id, guild_id, voice_channel_id, started_at_ms, ended_at_ms, payload_json
            FROM capture_runs
            ORDER BY started_at_ms, capture_run_id
            "#,
        )
        .fetch_all(&self.pool)
        .await?;
        let mut contexts = Vec::new();
        for row in rows {
            let capture_run_id: String = row.try_get("capture_run_id")?;
            let guild_id: String = row.try_get("guild_id")?;
            let voice_channel_id: String = row.try_get("voice_channel_id")?;
            let started_at_ms: Option<i64> = row.try_get("started_at_ms")?;
            let Some(started_at_ms) = started_at_ms else {
                anyhow::bail!("capture run {capture_run_id} has no started_at_ms");
            };
            let payload: Value = row.try_get("payload_json")?;
            let policy = payload
                .get("retention_policy")
                .or_else(|| payload.get("retentionPolicy"))
                .map(RetentionPolicy::from_value)
                .transpose()?
                .unwrap_or_else(RetentionPolicy::default);
            let started_at = ms_to_datetime(started_at_ms).ok_or_else(|| {
                anyhow::anyhow!("capture run {capture_run_id} has invalid started_at_ms")
            })?;
            contexts.push(CaptureRunRetention {
                capture_run_id: capture_run_id.clone(),
                guild_id: guild_id.clone(),
                voice_channel_id: voice_channel_id.clone(),
                started_at_ms,
                ended_at_ms: row.try_get("ended_at_ms")?,
                session_dir: self.capture_run_scratch_dir(
                    &guild_id,
                    &voice_channel_id,
                    started_at,
                    &capture_run_id,
                ),
                policy,
            });
        }
        Ok(contexts)
    }

    async fn retention_transcript_event_ids(
        &self,
        contexts: &[CaptureRunRetention],
        default_policy: RetentionPolicy,
        current_ms: i64,
        capture_run_ids: Option<&[String]>,
    ) -> Result<Vec<(String, String, String)>> {
        let rows = if let Some(capture_run_ids) = capture_run_ids {
            sqlx::query(
                r#"
                SELECT event_id, guild_id, scope_id, capture_run_id, started_at_ms
                FROM timeline_events
                WHERE scope_kind = 'voice_channel'
                  AND event_kind IN ('speech_segment', 'transcript')
                  AND forgotten = FALSE
                  AND capture_run_id = ANY($1)
                ORDER BY guild_id, scope_id, started_at_ms
                "#,
            )
            .bind(capture_run_ids)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query(
                r#"
                SELECT event_id, guild_id, scope_id, capture_run_id, started_at_ms
                FROM timeline_events
                WHERE scope_kind = 'voice_channel'
                  AND event_kind IN ('speech_segment', 'transcript')
                  AND forgotten = FALSE
                ORDER BY guild_id, scope_id, started_at_ms
                "#,
            )
            .fetch_all(&self.pool)
            .await?
        };
        let mut event_ids = Vec::new();
        for row in rows {
            let event_id: String = row.try_get("event_id")?;
            let guild_id: String = row.try_get("guild_id")?;
            let channel_id: String = row.try_get("scope_id")?;
            let capture_run_id: String = row.try_get("capture_run_id")?;
            let started_at_ms: i64 = row.try_get("started_at_ms")?;
            let policy = retention_policy_for_event(contexts, default_policy, &capture_run_id);
            if policy.transcript_events.expired(current_ms, started_at_ms) {
                event_ids.push((guild_id, channel_id, event_id));
            }
        }
        Ok(event_ids)
    }

    async fn retention_source_audio_candidates(
        &self,
        contexts: &[CaptureRunRetention],
        current_ms: i64,
    ) -> Result<BTreeMap<PathBuf, SourceAudioCandidate>> {
        let mut candidates = BTreeMap::new();
        for context in contexts {
            if !context
                .policy
                .source_audio
                .expired(current_ms, context.started_at_ms)
            {
                continue;
            }
            self.collect_source_audio_files(
                context.session_dir.join("segments"),
                context,
                &mut candidates,
            )?;
            self.collect_source_audio_files(
                context.session_dir.join("wake-probes"),
                context,
                &mut candidates,
            )?;
        }
        Ok(candidates)
    }

    fn collect_source_audio_files(
        &self,
        root: PathBuf,
        context: &CaptureRunRetention,
        candidates: &mut BTreeMap<PathBuf, SourceAudioCandidate>,
    ) -> Result<()> {
        if !root.is_dir() {
            return Ok(());
        }
        for entry in fs::read_dir(root)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                self.collect_source_audio_files(path, context, candidates)?;
                continue;
            }
            if path.extension().and_then(|value| value.to_str()) != Some("wav") {
                continue;
            }
            candidates
                .entry(path)
                .or_insert_with(|| SourceAudioCandidate {
                    guild_id: context.guild_id.clone(),
                    voice_channel_id: context.voice_channel_id.clone(),
                });
        }
        Ok(())
    }

    async fn retention_terminal_job_ids(
        &self,
        contexts: &[CaptureRunRetention],
        default_policy: RetentionPolicy,
        current_ms: i64,
        scope_keys: Option<&[(String, String)]>,
    ) -> Result<Vec<String>> {
        let rows = if let Some(scope_keys) = scope_keys {
            let (guild_ids, scope_ids): (Vec<_>, Vec<_>) = scope_keys.iter().cloned().unzip();
            sqlx::query(
                r#"
                SELECT j.job_id, j.scope_kind, j.guild_id, j.scope_id, j.created_at_ms
                FROM jobs j
                JOIN UNNEST($1::text[], $2::text[]) AS finite(guild_id, scope_id)
                  ON j.guild_id = finite.guild_id
                 AND j.scope_id = finite.scope_id
                WHERE j.terminal = TRUE
                  AND j.ephemeral = FALSE
                  AND j.scope_kind = 'voice_channel'
                ORDER BY j.created_at_ms, j.job_id
                "#,
            )
            .bind(&guild_ids)
            .bind(&scope_ids)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query(
                r#"
                SELECT job_id, scope_kind, guild_id, scope_id, created_at_ms
                FROM jobs
                WHERE terminal = TRUE
                  AND ephemeral = FALSE
                ORDER BY created_at_ms, job_id
                "#,
            )
            .fetch_all(&self.pool)
            .await?
        };
        let mut job_ids = Vec::new();
        for row in rows {
            let job_id: String = row.try_get("job_id")?;
            let scope_kind: String = row.try_get("scope_kind")?;
            let guild_id: String = row.try_get("guild_id")?;
            let scope_id: String = row.try_get("scope_id")?;
            let created_at_ms: i64 = row.try_get("created_at_ms")?;
            let policy = if scope_kind == "voice_channel" {
                retention_policy_for_scope_time(
                    contexts,
                    default_policy,
                    &guild_id,
                    &scope_id,
                    created_at_ms,
                )
            } else {
                default_policy
            };
            if policy.job_metadata.expired(current_ms, created_at_ms) {
                job_ids.push(job_id);
            }
        }
        Ok(job_ids)
    }
}

fn retention_policy_for_event(
    contexts: &[CaptureRunRetention],
    default_policy: RetentionPolicy,
    capture_run_id: &str,
) -> RetentionPolicy {
    contexts
        .iter()
        .find(|context| context.capture_run_id == capture_run_id)
        .map(|context| context.policy)
        .unwrap_or(default_policy)
}

fn finite_transcript_capture_runs(
    contexts: &[CaptureRunRetention],
    default_policy: RetentionPolicy,
) -> FiniteTranscriptRetentionScope {
    if default_policy.transcript_events.is_finite() {
        return FiniteTranscriptRetentionScope::All;
    }
    let capture_run_ids = contexts
        .iter()
        .filter(|context| context.policy.transcript_events.is_finite())
        .map(|context| context.capture_run_id.clone())
        .collect::<Vec<_>>();
    if capture_run_ids.is_empty() {
        FiniteTranscriptRetentionScope::None
    } else {
        FiniteTranscriptRetentionScope::CaptureRuns(capture_run_ids)
    }
}

fn finite_job_metadata_scopes(
    contexts: &[CaptureRunRetention],
    default_policy: RetentionPolicy,
) -> FiniteJobMetadataRetentionScope {
    if default_policy.job_metadata.is_finite() {
        return FiniteJobMetadataRetentionScope::All;
    }
    let scope_keys = contexts
        .iter()
        .filter(|context| context.policy.job_metadata.is_finite())
        .map(|context| (context.guild_id.clone(), context.voice_channel_id.clone()))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    if scope_keys.is_empty() {
        FiniteJobMetadataRetentionScope::None
    } else {
        FiniteJobMetadataRetentionScope::VoiceScopes(scope_keys)
    }
}

fn retention_policy_for_scope_time(
    contexts: &[CaptureRunRetention],
    default_policy: RetentionPolicy,
    guild_id: &str,
    scope_id: &str,
    at_ms: i64,
) -> RetentionPolicy {
    contexts
        .iter()
        .filter(|context| {
            context.guild_id == guild_id
                && context.voice_channel_id == scope_id
                && context.started_at_ms <= at_ms
                && context
                    .ended_at_ms
                    .is_none_or(|ended_at_ms| at_ms <= ended_at_ms)
        })
        .max_by_key(|context| context.started_at_ms)
        .map(|context| context.policy)
        .unwrap_or(default_policy)
}
