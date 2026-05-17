use serde_json::{Value, json};

use crate::Result;
use crate::runtime::core::execution::JobDecision;
use crate::runtime::domain::voice_capture::wake_activations;
use crate::runtime::{
    CommandKind, CommandRequest, DiscordSlashCommandPayload, Job, JobOutput, Runtime,
};

pub(crate) async fn prepare(
    runtime: &mut Runtime,
    job: &Job,
    payload: &DiscordSlashCommandPayload,
) -> Result<JobDecision> {
    runtime
        .timeline_store
        .append_event(
            &payload.guild_id,
            payload.timeline_channel_id(),
            json!({
                "event_kind": "discord_slash_command",
                "kind": "discord_slash_command",
                "job_id": job.id,
                "interaction_id": payload.interaction_id,
                "discord_channel_id": payload.channel_id,
                "voice_channel_id": payload.voice_channel_id,
                "speaker_user_id": payload.user_id,
                "speaker_label": payload.username,
                "command_name": payload.command_name,
                "options": payload.options_json(),
                "timestamp": payload.created_at,
            }),
        )
        .await?;

    match payload.command_name.as_str() {
        "join" => queue_command_child(runtime, job, payload, CommandKind::JoinRoom).await,
        "leave" => queue_command_child(runtime, job, payload, CommandKind::LeaveRoom).await,
        "wake" => schedule_manual_wake(runtime, job, payload).await,
        "deafen" => queue_command_child(runtime, job, payload, CommandKind::DeafenListening).await,
        "undeafen" => {
            queue_command_child(runtime, job, payload, CommandKind::ResumeListening).await
        }
        "feedback" => record_feedback(runtime, job, payload).await,
        command => Ok(JobDecision::Complete(JobOutput::from_boundary_json(
            &json!({
                "kind": "discord_slash_command",
                "status": "ignored_unknown_command",
                "command": command,
            }),
        )?)),
    }
}

async fn record_feedback(
    runtime: &mut Runtime,
    job: &Job,
    payload: &DiscordSlashCommandPayload,
) -> Result<JobDecision> {
    let message = slash_option_string(payload, &["message"]);
    runtime
        .timeline_store
        .append_event(
            &payload.guild_id,
            payload.timeline_channel_id(),
            json!({
                "event_kind": "feedback",
                "kind": "feedback",
                "job_id": job.id,
                "interaction_id": payload.interaction_id,
                "discord_channel_id": payload.channel_id,
                "voice_channel_id": payload.voice_channel_id,
                "source": "discord_slash_command",
                "speaker_user_id": payload.user_id,
                "speaker_label": payload.username,
                "speaker_username": payload.username,
                "text": &message,
                "feedback_message": &message,
                "created_at": payload.created_at,
                "timestamp": payload.created_at,
            }),
        )
        .await?;
    Ok(JobDecision::Complete(JobOutput::from_boundary_json(
        &json!({
            "kind": "feedback",
            "status": "recorded",
            "message": message,
        }),
    )?))
}

async fn schedule_manual_wake(
    runtime: &mut Runtime,
    job: &Job,
    payload: &DiscordSlashCommandPayload,
) -> Result<JobDecision> {
    let voice_channel_id = slash_voice_channel_id(payload)?;
    let event = runtime
        .timeline_store
        .append_event(
            &payload.guild_id,
            &voice_channel_id,
            json!({
                "event_kind": "wake_detected",
                "kind": "wake_detected",
                "job_id": job.id,
                "interaction_id": payload.interaction_id,
                "source": "discord_slash_command",
                "manual": true,
                "discord_channel_id": payload.channel_id,
                "voice_channel_id": voice_channel_id,
                "channelId": voice_channel_id,
                "speaker_user_id": payload.user_id,
                "speakerId": payload.user_id,
                "speaker_label": payload.username,
                "speakerLabel": payload.username,
                "speaker_username": payload.username,
                "speakerUsername": payload.username,
                "startedAt": payload.created_at,
                "endedAt": payload.created_at,
                "timestamp": payload.created_at,
                "duration_ms": 0,
                "durationMs": 0,
                "wake": {
                    "wake": true,
                    "source": "discord_slash_command",
                },
                "wake_detected": true,
            }),
        )
        .await?;
    let wake = wake_activations::schedule_from_wake_event(runtime, &event).await?;
    Ok(JobDecision::Complete(JobOutput::from_boundary_json(
        &json!({
            "kind": "manual_wake",
            "wake": wake,
        }),
    )?))
}

async fn queue_command_child(
    runtime: &mut Runtime,
    job: &Job,
    payload: &DiscordSlashCommandPayload,
    command_kind: CommandKind,
) -> Result<JobDecision> {
    runtime
        .create_command_job(command_request(payload, command_kind)?, Some(job))
        .await?;
    Ok(JobDecision::Wait)
}

fn command_request(
    payload: &DiscordSlashCommandPayload,
    command_kind: CommandKind,
) -> Result<CommandRequest> {
    CommandRequest::from_json(&json!({
        "action": "dispatch_now",
        "command_kind": command_kind.as_str(),
        "guild_id": payload.guild_id,
        "voice_channel_id": payload.voice_channel_id,
        "requested_by_user_id": payload.user_id,
        "requested_by_speaker_label": payload.username,
        "target_voice_channel_id": "",
        "arguments": {
            "channel": "",
            "target_channel": "",
        },
    }))
}

fn slash_voice_channel_id(payload: &DiscordSlashCommandPayload) -> Result<String> {
    let voice_channel_id = payload.voice_channel_id.trim();
    if voice_channel_id.is_empty() {
        anyhow::bail!(
            "/{} requires the invoking user to be in a voice channel",
            payload.command_name
        );
    }
    Ok(voice_channel_id.to_string())
}

fn slash_option_string(payload: &DiscordSlashCommandPayload, names: &[&str]) -> String {
    let options = payload.options_json();
    if let Some(object) = options.as_object() {
        for name in names {
            if let Some(value) = object.get(*name).and_then(option_value_string) {
                return value;
            }
        }
    }
    if let Some(values) = options.as_array() {
        for wanted in names {
            for option in values {
                if option.get("name").and_then(Value::as_str) == Some(*wanted)
                    && let Some(value) = option.get("value").and_then(option_value_string)
                {
                    return value;
                }
            }
        }
    }
    String::new()
}

fn option_value_string(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(ToOwned::to_owned)
        .or_else(|| value.as_i64().map(|value| value.to_string()))
        .or_else(|| value.as_u64().map(|value| value.to_string()))
        .or_else(|| {
            value
                .as_object()
                .and_then(|object| object.get("String").or_else(|| object.get("value")))
                .and_then(option_value_string)
        })
}
