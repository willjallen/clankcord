use serde_json::json;

use crate::Result;
use crate::runtime::core::execution::JobDecision;
use crate::runtime::timeline::{isoformat_z, parse_instant, utc_now};
use crate::runtime::{
    AgentSessionRecord, CommandRequest, DiscordTextMessagePayload, Job, JobOutput, Runtime,
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
            return Ok(JobDecision::Complete(JobOutput::from_boundary_json(
                &json!({
                    "kind": "discord_text_message",
                    "status": "ignored_expired_agent_thread",
                    "agent_session_id": session.agent_session_id,
                }),
            )?));
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

    let event = runtime
        .timeline_store
        .append_event(
            &session.guild_id,
            &session.voice_channel_id,
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
    runtime
        .touch_agent_session(&session.agent_session_id)
        .await?;

    let mut command = CommandRequest::agent_task(
        session.guild_id.clone(),
        session.voice_channel_id.clone(),
        payload.author_user_id.clone(),
        payload.content.clone(),
    );
    command.requested_by_speaker_label = text_author_label(payload);
    let event_id = event
        .get("event_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_string();
    let mut arguments = command.arguments.to_json();
    if let Some(object) = arguments.as_object_mut() {
        object.insert(
            "source_event_ids".to_string(),
            serde_json::Value::Array(vec![serde_json::Value::String(event_id)]),
        );
    }
    command.arguments = crate::runtime::CommandArguments::from_json(Some(&arguments))?;

    let agent_job = Job::agent_task_for_session(
        session.agent_session_id,
        session.guild_id,
        session.voice_channel_id,
        payload.author_user_id.clone(),
        command,
    );
    Ok(JobDecision::WaitFor(vec![agent_job]))
}

fn agent_session_is_current(session: &AgentSessionRecord) -> bool {
    session.state.is_selectable()
        && parse_instant(&session.expires_at)
            .map(|expires_at| expires_at > utc_now())
            .unwrap_or(false)
}

fn text_author_label(payload: &DiscordTextMessagePayload) -> String {
    crate::runtime::util::first_non_empty([
        payload.author_display_name.clone(),
        payload.author_username.clone(),
        payload.author_user_id.clone(),
    ])
}
