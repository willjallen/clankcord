use serde_json::json;

use crate::Result;
use crate::runtime::core::execution::JobDecision;
use crate::runtime::timeline::{isoformat_z, parse_instant, utc_now};
use crate::runtime::{
    AgentSessionRecord, AgentSessionRouteKind, CommandRequest, DiscordTextMessagePayload, Job,
    JobOutput, Runtime, RuntimeScope,
};

pub(crate) async fn prepare(
    runtime: &mut Runtime,
    _job: &Job,
    payload: &DiscordTextMessagePayload,
) -> Result<JobDecision> {
    if payload.content.trim().is_empty() {
        return Ok(JobDecision::Complete(JobOutput::from_boundary_json(
            &json!({"kind": "discord_text_message", "status": "ignored_empty"}),
        )?));
    }

    let session = if payload.guild_id.trim().is_empty() {
        runtime
            .ensure_dm_agent_session(&payload.author_user_id)
            .await?
    } else if let Some(session) = runtime
        .timeline_store
        .agent_session_for_thread(&payload.channel_id)
        .await?
    {
        if !agent_session_is_current(&session) {
            return resume_agent_session_from_thread_message(runtime, payload, session).await;
        }
        session
    } else if payload.channel_id
        == runtime
            .timeline_store
            .control_config()
            .await?
            .bots_channel_id
    {
        return Ok(JobDecision::Complete(JobOutput::from_boundary_json(
            &json!({
                "kind": "discord_text_message",
                "status": "ignored_agent_chat_top_level",
                "reason": "top-level agent-chat messages are not tied to an agent session",
            }),
        )?));
    } else {
        return Ok(JobDecision::Complete(JobOutput::from_boundary_json(
            &json!({"kind": "discord_text_message", "status": "ignored_unmanaged_channel"}),
        )?));
    };

    let event_id = append_thread_message_event(runtime, &session, payload).await?;
    runtime
        .touch_agent_session(&session.agent_session_id)
        .await?;

    let agent_job = agent_task_for_thread_message(session, payload, event_id)?;
    Ok(JobDecision::WaitFor(vec![agent_job]))
}

async fn resume_agent_session_from_thread_message(
    runtime: &mut Runtime,
    payload: &DiscordTextMessagePayload,
    session: AgentSessionRecord,
) -> Result<JobDecision> {
    if session.route_kind.as_str() != "voice" {
        anyhow::bail!(
            "Discord thread {} is attached to unsupported {} agent session {}",
            payload.channel_id,
            session.route_kind.as_str(),
            session.agent_session_id
        );
    }

    append_thread_message_event(runtime, &session, payload).await?;
    Ok(JobDecision::WaitFor(vec![Job::agent_session_resume(
        session.agent_session_id,
        "voice",
        session.guild_id,
        session.scope_id,
        payload.author_user_id.clone(),
        payload.content.clone(),
    )]))
}

async fn append_thread_message_event(
    runtime: &mut Runtime,
    session: &AgentSessionRecord,
    payload: &DiscordTextMessagePayload,
) -> Result<String> {
    let event = runtime
        .timeline_store
        .append_scope_event(
            &agent_session_scope(session),
            json!({
                "event_kind": "discord_text_message",
                "kind": "discord_text_message",
                "agent_session_id": session.agent_session_id,
                "discord_channel_id": payload.channel_id,
                "discord_message_id": payload.message_id,
                "referenced_message_id": payload.referenced_message_id,
                "speaker_user_id": payload.author_user_id,
                "speaker_label": text_author_label(payload),
                "text": payload.content,
                "timestamp": if payload.created_at.trim().is_empty() {
                    isoformat_z(None)
                } else {
                    payload.created_at.clone()
                },
            }),
        )
        .await?;
    Ok(event
        .get("event_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_string())
}

fn agent_task_for_thread_message(
    session: AgentSessionRecord,
    payload: &DiscordTextMessagePayload,
    event_id: String,
) -> Result<Job> {
    let mut command = CommandRequest::agent_task(
        session.guild_id.clone(),
        session.scope_id.clone(),
        payload.author_user_id.clone(),
        payload.content.clone(),
    );
    command.requested_by_speaker_label = text_author_label(payload);
    let mut arguments = command.arguments.to_json();
    if let Some(object) = arguments.as_object_mut() {
        object.insert(
            "source_event_ids".to_string(),
            serde_json::Value::Array(vec![serde_json::Value::String(event_id)]),
        );
    }
    command.arguments = crate::runtime::CommandArguments::from_json(Some(&arguments))?;

    Ok(Job::agent_task_for_session(
        session.agent_session_id.clone(),
        agent_session_scope(&session),
        payload.author_user_id.clone(),
        command,
    ))
}

fn agent_session_is_current(session: &AgentSessionRecord) -> bool {
    session.state.is_selectable()
        && parse_instant(&session.max_active_until)
            .map(|max_active_until| max_active_until > utc_now())
            .unwrap_or(false)
}

fn agent_session_scope(session: &AgentSessionRecord) -> RuntimeScope {
    if session.route_kind == AgentSessionRouteKind::Dm {
        RuntimeScope::dm(session.dm_user_id.clone())
    } else {
        RuntimeScope::voice_channel(session.guild_id.clone(), session.scope_id.clone())
    }
}

fn text_author_label(payload: &DiscordTextMessagePayload) -> String {
    crate::runtime::util::first_non_empty([
        payload.author_display_name.clone(),
        payload.author_username.clone(),
        payload.author_user_id.clone(),
    ])
}
