use serde_json::{Value, json};

use crate::Result;
use crate::config;
use crate::runtime::core::execution::JobDecision;
use crate::runtime::timeline::{
    event_text, isoformat_z, new_id, parse_instant, resolve_time_reference, utc_now,
};
use crate::runtime::{
    AgentSessionRecord, AgentSessionRecordState, AgentSessionResumePayload, AgentSessionRouteKind,
    AgentSessionStartOutput, AgentSessionStartPayload, AgentSessionSunsetPayload, CommandRequest,
    DiscordForumThreadCreatePayload, Job, JobKind, JobOutput, JobState, Runtime, TextTarget,
    TextTargetKind, dm_route_key, voice_route_key,
};

const DISCORD_THREAD_NAME_LIMIT: usize = 100;

impl Runtime {
    pub(crate) async fn agent_session_start_or_task_job(
        &self,
        guild_id: &str,
        voice_channel_id: &str,
        requested_by_user_id: &str,
        command: CommandRequest,
    ) -> Result<Job> {
        self.retire_due_agent_sessions().await?;
        let route_key = voice_route_key(guild_id, voice_channel_id);
        if let Some(record) = self
            .timeline_store
            .active_agent_session_for_route(&route_key)
            .await?
            .filter(|record| !record.discord_thread_id.trim().is_empty())
        {
            return Ok(Job::agent_task_for_session(
                record.agent_session_id,
                record.guild_id,
                record.voice_channel_id,
                requested_by_user_id.to_string(),
                command,
            ));
        }
        let control = self.timeline_store.control_config().await?;
        let parent_channel_id = control.agent_threads_channel_id.trim();
        if parent_channel_id.is_empty() {
            anyhow::bail!("agentThreadsChannelId is not configured");
        }
        let record = if let Some(record) = self
            .timeline_store
            .starting_agent_session_for_route(&route_key)
            .await?
        {
            record
        } else {
            let created_at = utc_now();
            let max_active_until =
                created_at + chrono::Duration::seconds(agent_session_max_active_seconds());
            let mut record = AgentSessionRecord::new_voice_starting(
                new_id("ags"),
                guild_id.to_string(),
                voice_channel_id.to_string(),
                parent_channel_id.to_string(),
                isoformat_z(Some(created_at)),
                isoformat_z(Some(max_active_until)),
            );
            record.voice_capture_session_id = self
                .active_session_for_channel(guild_id, voice_channel_id)
                .await?
                .map(|session| session.session_id)
                .unwrap_or_default();
            self.timeline_store
                .create_agent_session_record(record)
                .await?
        };
        Ok(Job::agent_session_start(
            guild_id.to_string(),
            voice_channel_id.to_string(),
            requested_by_user_id.to_string(),
            AgentSessionStartPayload {
                agent_session_id: record.agent_session_id,
                guild_id: guild_id.to_string(),
                voice_channel_id: voice_channel_id.to_string(),
                discord_parent_channel_id: parent_channel_id.to_string(),
                requested_by_user_id: requested_by_user_id.to_string(),
                command,
            },
        ))
    }

    pub(crate) async fn ensure_dm_agent_session(
        &self,
        user_id: &str,
    ) -> Result<AgentSessionRecord> {
        self.retire_due_agent_sessions().await?;
        let route_key = dm_route_key(user_id);
        if let Some(record) = self
            .timeline_store
            .active_agent_session_for_route(&route_key)
            .await?
        {
            return Ok(record);
        }
        let created_at = utc_now();
        let max_active_until =
            created_at + chrono::Duration::seconds(agent_session_max_active_seconds());
        let record = AgentSessionRecord::new_dm(
            new_id("ags"),
            user_id.to_string(),
            isoformat_z(Some(created_at)),
            isoformat_z(Some(max_active_until)),
        );
        self.timeline_store
            .create_agent_session_record(record)
            .await
    }

    pub(crate) async fn touch_agent_session(&self, agent_session_id: &str) -> Result<()> {
        let mut record = self
            .timeline_store
            .get_agent_session_record(agent_session_id)
            .await?;
        let now = utc_now();
        record.last_activity_at = isoformat_z(Some(now));
        if record.state == AgentSessionRecordState::Starting {
            record.state = AgentSessionRecordState::Active;
        }
        self.timeline_store
            .update_agent_session_record(&record)
            .await
    }

    pub(crate) async fn set_agent_session_codex_session(
        &self,
        agent_session_id: &str,
        codex_session_id: String,
    ) -> Result<AgentSessionRecord> {
        let mut record = self
            .timeline_store
            .get_agent_session_record(agent_session_id)
            .await?;
        let now = utc_now();
        record.codex_session_id = codex_session_id;
        record.last_activity_at = isoformat_z(Some(now));
        record.state = AgentSessionRecordState::Active;
        self.timeline_store
            .update_agent_session_record(&record)
            .await?;
        Ok(record)
    }

    pub(crate) async fn prepare_agent_session_start_job(
        &mut self,
        job: &Job,
        payload: &AgentSessionStartPayload,
    ) -> Result<JobDecision> {
        let children = self.timeline_store.list_child_jobs(&job.id).await?;
        if children.iter().any(|child| !child.state.is_terminal()) {
            return Ok(JobDecision::Wait);
        }
        if let Some(failed) = children
            .iter()
            .find(|child| child.state != JobState::Complete && child.kind != JobKind::AgentTask)
        {
            self.mark_agent_session_failed(&payload.agent_session_id)
                .await?;
            return Ok(JobDecision::fail(format!(
                "agent session start dependency {} ended as {}: {}",
                failed.id, failed.state, failed.metadata.error
            )));
        }

        let mut record = self
            .timeline_store
            .get_agent_session_record(&payload.agent_session_id)
            .await?;
        if record.discord_thread_id.trim().is_empty() {
            if let Some(thread_job) = children
                .iter()
                .find(|child| child.kind == JobKind::DiscordForumThreadCreate)
            {
                let Some(JobOutput::DiscordForumThreadCreate(output)) =
                    thread_job.metadata.output.clone()
                else {
                    self.mark_agent_session_failed(&payload.agent_session_id)
                        .await?;
                    return Ok(JobDecision::fail(format!(
                        "agent session thread job {} completed without thread output",
                        thread_job.id
                    )));
                };
                record.discord_thread_id = output.thread_id.clone();
                record.discord_parent_channel_id = output.parent_channel_id;
                record.text_target = TextTarget {
                    kind: TextTargetKind::Channel,
                    channel_id: output.thread_id,
                    user_id: String::new(),
                };
                record.state = AgentSessionRecordState::Active;
                record.last_activity_at = isoformat_z(None);
                self.timeline_store
                    .update_agent_session_record(&record)
                    .await?;
                self.timeline_store
                    .append_event(
                        &record.guild_id,
                        &record.voice_channel_id,
                        json!({
                            "event_kind": "agent_session_created",
                            "kind": "agent_session_created",
                            "agent_session": record.to_json(),
                            "requested_by_user_id": payload.requested_by_user_id,
                        }),
                    )
                    .await?;
            } else {
                return Ok(JobDecision::WaitFor(vec![
                    Job::discord_forum_thread_create(
                        payload.guild_id.clone(),
                        payload.voice_channel_id.clone(),
                        payload.requested_by_user_id.clone(),
                        DiscordForumThreadCreatePayload {
                            parent_channel_id: payload.discord_parent_channel_id.clone(),
                            name: agent_thread_name(
                                &payload.voice_channel_id,
                                &payload.agent_session_id,
                            ),
                            content: agent_thread_content(
                                &payload.guild_id,
                                &payload.voice_channel_id,
                                &payload.requested_by_user_id,
                                &payload.agent_session_id,
                            ),
                            auto_archive_minutes: agent_thread_auto_archive_minutes(),
                            source_job_id: job.id.clone(),
                        },
                    ),
                ]));
            }
        }

        if let Some(agent_task) = children
            .iter()
            .find(|child| child.kind == JobKind::AgentTask)
        {
            if agent_task.state == JobState::Complete {
                return Ok(JobDecision::Complete(JobOutput::AgentSessionStart(
                    AgentSessionStartOutput {
                        agent_session_id: payload.agent_session_id.clone(),
                        status: "complete".to_string(),
                        agent_task_job_id: agent_task.id.clone(),
                    },
                )));
            }
            return Ok(JobDecision::fail(format!(
                "agent session task {} ended as {}: {}",
                agent_task.id, agent_task.state, agent_task.metadata.error
            )));
        }

        Ok(JobDecision::WaitFor(vec![Job::agent_task_for_session(
            payload.agent_session_id.clone(),
            payload.guild_id.clone(),
            payload.voice_channel_id.clone(),
            payload.requested_by_user_id.clone(),
            payload.command.clone(),
        )]))
    }

    pub(crate) async fn prepare_agent_session_sunset_job(
        &self,
        payload: &AgentSessionSunsetPayload,
    ) -> Result<JobDecision> {
        if payload.reason.trim().is_empty() {
            anyhow::bail!("agent session sunset requires a reason");
        }
        let record = self
            .retire_agent_session(
                &payload.agent_session_id,
                payload.reason.trim(),
                &payload.requested_by_user_id,
            )
            .await?;
        Ok(JobDecision::Complete(JobOutput::from_boundary_json(
            &json!({
                "kind": "agent_session_sunset",
                "agent_session": record.to_json(),
            }),
        )?))
    }

    pub(crate) async fn prepare_agent_session_resume_job(
        &self,
        job: &Job,
        payload: &AgentSessionResumePayload,
    ) -> Result<JobDecision> {
        let source = self
            .timeline_store
            .get_agent_session_record(&payload.source_agent_session_id)
            .await?;
        if source.state != AgentSessionRecordState::Retired {
            anyhow::bail!(
                "agent session {} is {}; resume requires retired",
                source.agent_session_id,
                source.state.as_str()
            );
        }

        let route_kind = payload.route_kind.trim();
        let route_key = match route_kind {
            "voice" => voice_route_key(&payload.guild_id, &payload.voice_channel_id),
            "dm" => dm_route_key(&payload.dm_user_id),
            value => anyhow::bail!("unsupported agent session resume route kind: {value}"),
        };
        if let Some(active) = self
            .timeline_store
            .active_agent_session_for_route(&route_key)
            .await?
            && active.agent_session_id != payload.new_agent_session_id
        {
            anyhow::bail!(
                "route {} already has active agent session {}",
                route_key,
                active.agent_session_id
            );
        }

        let children = self.timeline_store.list_child_jobs(&job.id).await?;
        if children.iter().any(|child| !child.state.is_terminal()) {
            return Ok(JobDecision::Wait);
        }
        if let Some(failed) = children
            .iter()
            .find(|child| child.state != JobState::Complete && child.kind != JobKind::AgentTask)
        {
            return Ok(JobDecision::fail(format!(
                "agent session resume dependency {} ended as {}: {}",
                failed.id, failed.state, failed.metadata.error
            )));
        }

        let mut record = if let Some(record) = self
            .timeline_store
            .maybe_agent_session_record(&payload.new_agent_session_id)
            .await?
        {
            record
        } else {
            let created_at = utc_now();
            let max_active_until =
                created_at + chrono::Duration::seconds(agent_session_max_active_seconds());
            let mut record = match route_kind {
                "voice" => {
                    if payload.guild_id.trim().is_empty()
                        || payload.voice_channel_id.trim().is_empty()
                    {
                        anyhow::bail!("voice resume requires guild_id and voice_channel_id");
                    }
                    let control = self.timeline_store.control_config().await?;
                    let parent_channel_id = control.agent_threads_channel_id.trim();
                    if parent_channel_id.is_empty() {
                        anyhow::bail!("agentThreadsChannelId is not configured");
                    }
                    let mut record = AgentSessionRecord::new_voice_starting(
                        payload.new_agent_session_id.clone(),
                        payload.guild_id.clone(),
                        payload.voice_channel_id.clone(),
                        parent_channel_id.to_string(),
                        isoformat_z(Some(created_at)),
                        isoformat_z(Some(max_active_until)),
                    );
                    record.voice_capture_session_id = self
                        .active_session_for_channel(&payload.guild_id, &payload.voice_channel_id)
                        .await?
                        .map(|session| session.session_id)
                        .unwrap_or_default();
                    record
                }
                "dm" => {
                    if payload.dm_user_id.trim().is_empty() {
                        anyhow::bail!("DM resume requires dm_user_id");
                    }
                    AgentSessionRecord::new_dm(
                        payload.new_agent_session_id.clone(),
                        payload.dm_user_id.clone(),
                        isoformat_z(Some(created_at)),
                        isoformat_z(Some(max_active_until)),
                    )
                }
                _ => unreachable!(),
            };
            record.codex_session_id = source.codex_session_id.clone();
            record.resumed_from_agent_session_id = source.agent_session_id.clone();
            let record = self
                .timeline_store
                .create_agent_session_record(record)
                .await?;
            self.timeline_store
                .append_event(
                    &record.guild_id,
                    &record.voice_channel_id,
                    json!({
                        "event_kind": "agent_session_resumed",
                        "kind": "agent_session_resumed",
                        "agent_session": record.to_json(),
                        "resumed_from_agent_session_id": source.agent_session_id,
                        "requested_by_user_id": payload.requested_by_user_id,
                    }),
                )
                .await?;
            record
        };

        if record.route_kind == AgentSessionRouteKind::Voice && record.discord_thread_id.is_empty()
        {
            if let Some(thread_job) = children
                .iter()
                .find(|child| child.kind == JobKind::DiscordForumThreadCreate)
            {
                let Some(JobOutput::DiscordForumThreadCreate(output)) =
                    thread_job.metadata.output.clone()
                else {
                    return Ok(JobDecision::fail(format!(
                        "agent session resume thread job {} completed without thread output",
                        thread_job.id
                    )));
                };
                record.discord_thread_id = output.thread_id.clone();
                record.discord_parent_channel_id = output.parent_channel_id;
                record.text_target = TextTarget {
                    kind: TextTargetKind::Channel,
                    channel_id: output.thread_id,
                    user_id: String::new(),
                };
                record.state = AgentSessionRecordState::Active;
                record.last_activity_at = isoformat_z(None);
                self.timeline_store
                    .update_agent_session_record(&record)
                    .await?;
            } else {
                return Ok(JobDecision::WaitFor(vec![
                    Job::discord_forum_thread_create(
                        record.guild_id.clone(),
                        record.voice_channel_id.clone(),
                        payload.requested_by_user_id.clone(),
                        DiscordForumThreadCreatePayload {
                            parent_channel_id: record.discord_parent_channel_id.clone(),
                            name: agent_thread_name(
                                &record.voice_channel_id,
                                &record.agent_session_id,
                            ),
                            content: agent_thread_content(
                                &record.guild_id,
                                &record.voice_channel_id,
                                &payload.requested_by_user_id,
                                &record.agent_session_id,
                            ),
                            auto_archive_minutes: agent_thread_auto_archive_minutes(),
                            source_job_id: job.id.clone(),
                        },
                    ),
                ]));
            }
        }

        if !payload.message.trim().is_empty() {
            if let Some(agent_task) = children
                .iter()
                .find(|child| child.kind == JobKind::AgentTask)
            {
                if agent_task.state == JobState::Complete {
                    return Ok(JobDecision::Complete(JobOutput::from_boundary_json(
                        &json!({
                            "kind": "agent_session_resume",
                            "agent_session": record.to_json(),
                            "agent_task_job_id": agent_task.id,
                        }),
                    )?));
                }
                return Ok(JobDecision::fail(format!(
                    "agent session resume task {} ended as {}: {}",
                    agent_task.id, agent_task.state, agent_task.metadata.error
                )));
            }
            return Ok(JobDecision::WaitFor(vec![Job::agent_task_for_session(
                record.agent_session_id.clone(),
                record.guild_id.clone(),
                record.voice_channel_id.clone(),
                payload.requested_by_user_id.clone(),
                CommandRequest::agent_task(
                    record.guild_id.clone(),
                    record.voice_channel_id.clone(),
                    payload.requested_by_user_id.clone(),
                    payload.message.clone(),
                ),
            )]));
        }

        Ok(JobDecision::Complete(JobOutput::from_boundary_json(
            &json!({
                "kind": "agent_session_resume",
                "agent_session": record.to_json(),
            }),
        )?))
    }

    pub(crate) async fn prepare_agent_session_retirement_job(&self) -> Result<JobDecision> {
        let retired = self.retire_due_agent_sessions().await?;
        Ok(JobDecision::Complete(JobOutput::from_boundary_json(
            &json!({
                "kind": "agent_session_retirement",
                "retired_sessions": retired.into_iter().map(|record| record.to_json()).collect::<Vec<_>>(),
            }),
        )?))
    }

    pub(crate) async fn retire_due_agent_sessions(&self) -> Result<Vec<AgentSessionRecord>> {
        let retired = self.timeline_store.retire_due_agent_sessions().await?;
        for record in &retired {
            self.timeline_store
                .append_event(
                    &record.guild_id,
                    &record.voice_channel_id,
                    json!({
                        "event_kind": "agent_session_retired",
                        "kind": "agent_session_retired",
                        "agent_session": record.to_json(),
                        "agent_session_id": record.agent_session_id,
                        "retirement_reason": record.retirement_reason,
                    }),
                )
                .await?;
        }
        Ok(retired)
    }

    pub(crate) async fn retire_agent_session(
        &self,
        agent_session_id: &str,
        reason: &str,
        retired_by_user_id: &str,
    ) -> Result<AgentSessionRecord> {
        let mut record = self
            .timeline_store
            .get_agent_session_record(agent_session_id)
            .await?;
        if record.state != AgentSessionRecordState::Retired {
            record.state = AgentSessionRecordState::Retired;
            record.retired_at = isoformat_z(None);
            record.retirement_reason = reason.to_string();
            record.retired_by_user_id = retired_by_user_id.to_string();
            self.timeline_store
                .update_agent_session_record(&record)
                .await?;
            self.timeline_store
                .append_event(
                    &record.guild_id,
                    &record.voice_channel_id,
                    json!({
                        "event_kind": "agent_session_retired",
                        "kind": "agent_session_retired",
                        "agent_session": record.to_json(),
                        "agent_session_id": record.agent_session_id,
                        "retirement_reason": record.retirement_reason,
                        "retired_by_user_id": record.retired_by_user_id,
                    }),
                )
                .await?;
        }
        Ok(record)
    }

    pub async fn agent_session_current(
        &self,
        guild_id: &str,
        voice_channel_id: &str,
    ) -> Result<Value> {
        let route_key = voice_route_key(guild_id, voice_channel_id);
        let session = self
            .timeline_store
            .active_agent_session_for_route(&route_key)
            .await?
            .map(|record| record.to_json())
            .unwrap_or(Value::Null);
        Ok(json!({
            "route_key": route_key,
            "agent_session": session,
        }))
    }

    pub async fn agent_session_list(
        &self,
        guild_id: &str,
        voice_channel_id: &str,
        state: &str,
        limit: usize,
    ) -> Result<Value> {
        let sessions = self
            .timeline_store
            .list_agent_session_records(guild_id, voice_channel_id, state, limit)
            .await?
            .into_iter()
            .map(|record| record.to_json())
            .collect::<Vec<_>>();
        Ok(json!({
            "count": sessions.len(),
            "agent_sessions": sessions,
        }))
    }

    pub async fn agent_session_get(&self, agent_session_id: &str) -> Result<Value> {
        let record = self
            .timeline_store
            .get_agent_session_record(agent_session_id)
            .await?;
        Ok(json!({
            "agent_session": record.to_json(),
        }))
    }

    pub async fn agent_session_search(
        &self,
        guild_id: &str,
        voice_channel_id: &str,
        state: &str,
        query: &str,
        since: &str,
        limit: usize,
    ) -> Result<Value> {
        let now = utc_now();
        let since_at = resolve_time_reference(since, Some(now));
        let needle = query.trim().to_lowercase();
        let records = self
            .timeline_store
            .list_agent_session_records(guild_id, voice_channel_id, state, 500)
            .await?;
        let mut hits = Vec::new();
        for record in records {
            if let Some(since_at) = since_at
                && parse_instant(&record.created_at)
                    .map(|created_at| created_at < since_at)
                    .unwrap_or(false)
            {
                continue;
            }
            let document = self.agent_session_search_document(&record).await?;
            let haystack = document.to_lowercase();
            if !needle.is_empty() && !haystack.contains(&needle) {
                continue;
            }
            hits.push(json!({
                "agent_session_id": record.agent_session_id,
                "state": record.state.as_str(),
                "route_key": record.route_key,
                "created_at": record.created_at,
                "retired_at": record.retired_at,
                "retirement_reason": record.retirement_reason,
                "resumed_from_agent_session_id": record.resumed_from_agent_session_id,
                "discord_thread_id": record.discord_thread_id,
                "latest_activity": record.last_activity_at,
                "matched_fields": agent_session_matched_fields(&record, &document, &needle),
                "snippet": agent_session_search_snippet(&document, &needle),
                "resume_command": agent_session_resume_command(&record),
            }));
            if hits.len() >= limit.max(1).min(100) {
                break;
            }
        }
        Ok(json!({
            "count": hits.len(),
            "hits": hits,
        }))
    }

    async fn agent_session_search_document(&self, record: &AgentSessionRecord) -> Result<String> {
        let mut parts = vec![serde_json::to_string(&record.to_json())?];
        let start = parse_instant(&record.created_at);
        let end =
            parse_instant(&record.retired_at).or_else(|| parse_instant(&record.max_active_until));
        for event in self
            .timeline_store
            .load_events(
                &record.guild_id,
                &record.voice_channel_id,
                start,
                end,
                None,
                None,
                false,
            )
            .await?
        {
            let text = event_text(&event);
            if !text.trim().is_empty() {
                parts.push(text);
            }
        }
        for job in self
            .timeline_store
            .list_jobs_by_scope_kind(
                &record.guild_id,
                &record.voice_channel_id,
                JobKind::AgentTask,
            )
            .await?
        {
            if let crate::runtime::JobPayload::AgentTask(payload) = &job.payload
                && payload.agent_session_id == record.agent_session_id
            {
                parts.push(serde_json::to_string(&job.to_value())?);
            }
        }
        Ok(parts.join("\n"))
    }

    async fn mark_agent_session_failed(&self, agent_session_id: &str) -> Result<()> {
        let mut record = self
            .timeline_store
            .get_agent_session_record(agent_session_id)
            .await?;
        record.state = AgentSessionRecordState::Failed;
        self.timeline_store
            .update_agent_session_record(&record)
            .await
    }
}

fn agent_session_max_active_seconds() -> i64 {
    config::agent_session_max_active_seconds()
}

fn agent_thread_auto_archive_minutes() -> i64 {
    config::agent_thread_auto_archive_minutes()
}

fn trim_thread_name(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.chars().count() <= DISCORD_THREAD_NAME_LIMIT {
        return trimmed.to_string();
    }
    trimmed.chars().take(DISCORD_THREAD_NAME_LIMIT).collect()
}

fn agent_thread_name(voice_channel_id: &str, agent_session_id: &str) -> String {
    trim_thread_name(&format!("agent {voice_channel_id} {agent_session_id}"))
}

fn agent_thread_content(
    guild_id: &str,
    voice_channel_id: &str,
    requested_by_user_id: &str,
    agent_session_id: &str,
) -> String {
    format!(
        "# Agent Session\n\n- Voice channel: `{voice_channel_id}`\n- Guild: `{guild_id}`\n- Requested by: <@{requested_by_user_id}>\n- Session: `{agent_session_id}`"
    )
}

fn agent_session_resume_command(record: &AgentSessionRecord) -> String {
    if record.route_kind == AgentSessionRouteKind::Dm {
        format!(
            "clankcord agent-sessions resume {} --dm-user {}",
            record.agent_session_id, record.dm_user_id
        )
    } else {
        format!(
            "clankcord agent-sessions resume {} --guild {} --channel {}",
            record.agent_session_id, record.guild_id, record.voice_channel_id
        )
    }
}

fn agent_session_matched_fields(
    record: &AgentSessionRecord,
    document: &str,
    needle: &str,
) -> Vec<&'static str> {
    if needle.is_empty() {
        return vec!["session"];
    }
    let mut fields = Vec::new();
    let session_blob = record.to_json().to_string().to_lowercase();
    if session_blob.contains(needle) {
        fields.push("session");
    }
    if document.to_lowercase().contains(needle) {
        fields.push("timeline");
    }
    fields
}

fn agent_session_search_snippet(document: &str, needle: &str) -> String {
    let compact = document.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.is_empty() {
        return String::new();
    }
    if needle.is_empty() {
        return compact.chars().take(240).collect();
    }
    let lower = compact.to_lowercase();
    let Some(start) = lower.find(needle) else {
        return compact.chars().take(240).collect();
    };
    let prefix_start = start.saturating_sub(80);
    compact
        .chars()
        .skip(prefix_start)
        .take(240)
        .collect::<String>()
}
