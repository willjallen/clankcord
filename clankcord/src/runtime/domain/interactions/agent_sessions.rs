use serde_json::json;

use crate::Result;
use crate::adapters::discord::api::{create_forum_thread, string_field};
use crate::runtime::timeline::{isoformat_z, new_id, utc_now};
use crate::runtime::{
    AgentSessionRecord, AgentSessionRecordState, Runtime, dm_route_key, voice_route_key,
};

const DEFAULT_AGENT_SESSION_EXPIRY_SECONDS: i64 = 4 * 60 * 60;
const DISCORD_THREAD_NAME_LIMIT: usize = 100;

impl Runtime {
    pub(crate) async fn ensure_voice_agent_session(
        &self,
        guild_id: &str,
        voice_channel_id: &str,
        requested_by_user_id: &str,
    ) -> Result<AgentSessionRecord> {
        self.timeline_store.expire_due_agent_sessions().await?;
        let route_key = voice_route_key(guild_id, voice_channel_id);
        if let Some(record) = self
            .timeline_store
            .active_agent_session_for_route(&route_key)
            .await?
        {
            return Ok(record);
        }

        let parent_channel_id = self.control_config.agent_threads_channel_id.trim();
        if parent_channel_id.is_empty() {
            anyhow::bail!("agentThreadsChannelId is not configured");
        }
        let created_at = utc_now();
        let expires_at = created_at + chrono::Duration::seconds(agent_session_expiry_seconds());
        let agent_session_id = new_id("ags");
        let thread = self.create_voice_agent_thread(
            parent_channel_id,
            guild_id,
            voice_channel_id,
            requested_by_user_id,
            &agent_session_id,
        )?;
        let thread_id = string_field(&thread, "id");
        if thread_id.trim().is_empty() {
            anyhow::bail!("Discord did not return an agent session thread id");
        }
        let record = AgentSessionRecord::new_voice(
            agent_session_id,
            guild_id.to_string(),
            voice_channel_id.to_string(),
            parent_channel_id.to_string(),
            thread_id,
            isoformat_z(Some(created_at)),
            isoformat_z(Some(expires_at)),
        );
        let record = self
            .timeline_store
            .create_agent_session_record(record)
            .await?;
        self.timeline_store
            .append_event(
                guild_id,
                voice_channel_id,
                json!({
                    "event_kind": "agent_session_created",
                    "kind": "agent_session_created",
                    "agent_session": record.to_json(),
                    "requested_by_user_id": requested_by_user_id,
                }),
            )
            .await?;
        Ok(record)
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

    fn create_voice_agent_thread(
        &self,
        parent_channel_id: &str,
        guild_id: &str,
        voice_channel_id: &str,
        requested_by_user_id: &str,
        agent_session_id: &str,
    ) -> Result<serde_json::Value> {
        let name = trim_thread_name(&format!("agent {voice_channel_id} {agent_session_id}"));
        let content = format!(
            "# Agent Session\n\n- Voice channel: `{voice_channel_id}`\n- Guild: `{guild_id}`\n- Requested by: <@{requested_by_user_id}>\n- Session: `{agent_session_id}`"
        );
        create_forum_thread(
            parent_channel_id,
            &name,
            &content,
            agent_thread_auto_archive_minutes(),
        )
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
