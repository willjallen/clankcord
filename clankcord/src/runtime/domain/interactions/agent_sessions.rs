use serde_json::json;

use crate::Result;
use crate::runtime::core::execution::JobDecision;
use crate::runtime::timeline::{isoformat_z, new_id, utc_now};
use crate::runtime::{
    AgentSessionRecord, AgentSessionRecordState, AgentSessionStartOutput, AgentSessionStartPayload,
    CommandRequest, DiscordForumThreadCreatePayload, Job, JobKind, JobOutput, JobState, Runtime,
    TextTarget, TextTargetKind, dm_route_key, voice_route_key,
};

const DEFAULT_AGENT_SESSION_EXPIRY_SECONDS: i64 = 4 * 60 * 60;
const DISCORD_THREAD_NAME_LIMIT: usize = 100;

impl Runtime {
    pub(crate) async fn agent_session_start_or_task_job(
        &self,
        guild_id: &str,
        voice_channel_id: &str,
        requested_by_user_id: &str,
        command: CommandRequest,
    ) -> Result<Job> {
        self.timeline_store.expire_due_agent_sessions().await?;
        let route_key = voice_route_key(guild_id, voice_channel_id);
        if let Some(record) = self
            .timeline_store
            .active_agent_session_for_route(&route_key)
            .await?
            .filter(|record| {
                record.state == AgentSessionRecordState::Active
                    && !record.discord_thread_id.trim().is_empty()
            })
        {
            return Ok(Job::agent_task_for_session(
                record.agent_session_id,
                record.guild_id,
                record.voice_channel_id,
                requested_by_user_id.to_string(),
                command,
            ));
        }
        let parent_channel_id = self.control_config.agent_threads_channel_id.trim();
        if parent_channel_id.is_empty() {
            anyhow::bail!("agentThreadsChannelId is not configured");
        }
        let record = if let Some(record) = self
            .timeline_store
            .active_agent_session_for_route(&route_key)
            .await?
            .filter(|record| record.state == AgentSessionRecordState::Starting)
        {
            record
        } else {
            let created_at = utc_now();
            let expires_at = created_at + chrono::Duration::seconds(agent_session_expiry_seconds());
            let record = AgentSessionRecord::new_voice_starting(
                new_id("ags"),
                guild_id.to_string(),
                voice_channel_id.to_string(),
                parent_channel_id.to_string(),
                isoformat_z(Some(created_at)),
                isoformat_z(Some(expires_at)),
            );
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
        self.timeline_store.expire_due_agent_sessions().await?;
        let route_key = dm_route_key(user_id);
        if let Some(record) = self
            .timeline_store
            .active_agent_session_for_route(&route_key)
            .await?
        {
            return Ok(record);
        }
        let created_at = utc_now();
        let expires_at = created_at + chrono::Duration::seconds(agent_session_expiry_seconds());
        let record = AgentSessionRecord::new_dm(
            new_id("ags"),
            user_id.to_string(),
            isoformat_z(Some(created_at)),
            isoformat_z(Some(expires_at)),
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
        record.expires_at = isoformat_z(Some(
            now + chrono::Duration::seconds(agent_session_expiry_seconds()),
        ));
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
        record.expires_at = isoformat_z(Some(
            now + chrono::Duration::seconds(agent_session_expiry_seconds()),
        ));
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

fn agent_session_expiry_seconds() -> i64 {
    std::env::var("CLANKCORD_AGENT_SESSION_EXPIRY_SECONDS")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(DEFAULT_AGENT_SESSION_EXPIRY_SECONDS)
        .clamp(60, 7 * 24 * 60 * 60)
}

fn agent_thread_auto_archive_minutes() -> i64 {
    std::env::var("CLANKCORD_AGENT_THREAD_AUTO_ARCHIVE_MINUTES")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(1440)
        .clamp(60, 10080)
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
