use serde_json::json;

use crate::Result;
use crate::errors::{
    discord_error_channel_id, discord_error_is_unavailable_channel, discord_error_text_channel_id,
    discord_error_text_is_unavailable_channel,
};
use crate::runtime::util::{first_non_empty, preview};
use crate::runtime::{AgentSessionRecord, Runtime, TextTarget, TextTargetKind};

pub(crate) const UNAVAILABLE_SESSION_THREAD_STATUS: &str = "skipped_unavailable_session_thread";

pub(crate) fn discord_error_targets_unavailable_session_thread(
    error: &anyhow::Error,
    target: &TextTarget,
) -> bool {
    target.kind == TextTargetKind::Channel
        && !target.channel_id.trim().is_empty()
        && discord_error_is_unavailable_channel(error)
}

pub(crate) fn discord_error_text_targets_unavailable_session_thread(
    error: &str,
    target: &TextTarget,
) -> bool {
    target.kind == TextTargetKind::Channel
        && !target.channel_id.trim().is_empty()
        && discord_error_text_is_unavailable_channel(error)
}

pub(crate) fn discord_error_unavailable_channel_id(error: &anyhow::Error) -> String {
    discord_error_channel_id(error)
}

pub(crate) fn discord_error_text_unavailable_channel_id(error: &str) -> String {
    discord_error_text_channel_id(error)
}

impl Runtime {
    pub(crate) async fn mark_agent_session_thread_unavailable(
        &self,
        agent_session_id: &str,
        thread_id: &str,
        source_job_id: &str,
        reason: &str,
    ) -> Result<AgentSessionRecord> {
        let mut session = self
            .timeline_store
            .get_agent_session_record(agent_session_id)
            .await?;
        let thread_id = first_non_empty([
            thread_id.to_string(),
            session.discord_thread_id.clone(),
            session.text_target.channel_id.clone(),
        ]);
        if thread_id.trim().is_empty() {
            return Ok(session);
        }

        let mut changed = false;
        if session.discord_thread_id == thread_id {
            session.discord_thread_id.clear();
            changed = true;
        }
        if session.text_target.kind == TextTargetKind::Channel
            && session.text_target.channel_id == thread_id
        {
            session.text_target.channel_id.clear();
            changed = true;
        }
        if !changed {
            return Ok(session);
        }

        self.timeline_store
            .update_agent_session_record(&session)
            .await?;
        let mut event = json!({
            "event_kind": "agent_session_thread_unavailable",
            "kind": "agent_session_thread_unavailable",
            "agent_session_id": session.agent_session_id,
            "discord_thread_id": thread_id,
            "source_job_id": source_job_id,
            "status": UNAVAILABLE_SESSION_THREAD_STATUS,
            "agent_session": session.to_json(),
        });
        let reason = preview(reason, 500);
        if !reason.trim().is_empty() {
            event["reason"] = json!(reason);
        }
        self.timeline_store
            .append_event(&session.guild_id, &session.scope_id, event)
            .await?;
        Ok(session)
    }
}
