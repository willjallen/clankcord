use serde_json::json;

use crate::Result;
use crate::domain::Ctx;
use crate::errors::{
    discord_error_channel_id, discord_error_is_unavailable_channel, discord_error_text_channel_id,
    discord_error_text_is_unavailable_channel,
};
use crate::model::agents::{AgentSessionRecord, AgentSessionRecordState};
use crate::model::job::{Job, TextTarget, TextTargetKind};
use crate::util::{first_non_empty, preview};

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

pub(crate) async fn mark_agent_session_thread_unavailable(
    ctx: &Ctx,
    agent_session_id: &str,
    thread_id: &str,
    source_job_id: &str,
    reason: &str,
) -> Result<AgentSessionRecord> {
    let mut session = ctx.store.get_agent_session_record(agent_session_id).await?;
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

    ctx.store.update_agent_session_record(&session).await?;
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
    ctx.store
        .append_scope_event(&session.scope(), event)
        .await?;
    Ok(session)
}

/// Resolves the agent session behind a `source_job_id`, following resume
/// takeovers to the active session on the same route. Shared by every
/// surface whose target kind is AgentSession.
pub(crate) async fn agent_session_for_source_job(
    ctx: &Ctx,
    job: &Job,
    source_job_id: &str,
    surface: &str,
) -> Result<AgentSessionRecord> {
    let source_job_id = source_job_id.trim();
    if source_job_id.is_empty() {
        anyhow::bail!(
            "{surface} job {} uses session target without source job",
            job.id
        );
    }
    let source = ctx.store.get_job(source_job_id).await?;
    let crate::model::job::JobPayload::AgentTask(agent_task) = &source.payload else {
        anyhow::bail!(
            "{surface} job {} uses session target but source job {} is not an agent task",
            job.id,
            source_job_id
        );
    };
    let session = ctx
        .store
        .get_agent_session_record(&agent_task.agent_session_id)
        .await?;
    if session.state == AgentSessionRecordState::Retired
        && session.retirement_reason == "agent_session_resume_route_takeover"
    {
        return ctx
            .store
            .active_agent_session_for_route(&session.route_key)
            .await?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "agent session {} was retired by resume takeover but route {} has no active session",
                    session.agent_session_id,
                    session.route_key
                )
            });
    }
    Ok(session)
}
