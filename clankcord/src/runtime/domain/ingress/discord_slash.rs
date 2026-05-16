use serde_json::{Value, json};

use crate::Result;
use crate::runtime::core::execution::JobDecision;
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
            &payload.channel_id,
            json!({
                "event_kind": "discord_slash_command",
                "kind": "discord_slash_command",
                "job_id": job.id,
                "interaction_id": payload.interaction_id,
                "discord_channel_id": payload.channel_id,
                "speaker_user_id": payload.user_id,
                "speaker_label": payload.username,
                "command_name": payload.command_name,
                "options": payload.options_json(),
                "timestamp": payload.created_at,
            }),
        )
        .await?;

    match payload.command_name.as_str() {
        "join" => Ok(JobDecision::WaitFor(vec![command_child(
            payload,
            CommandKind::JoinRoom,
        )?])),
        "leave" => Ok(JobDecision::WaitFor(vec![command_child(
            payload,
            CommandKind::LeaveRoom,
        )?])),
        "feedback" => Ok(JobDecision::Complete(JobOutput::from_boundary_json(
            &json!({
                "kind": "feedback",
                "status": "recorded",
                "message": slash_option_string(payload, &["message", "text", "feedback"]),
            }),
        )?)),
        command => Ok(JobDecision::Complete(JobOutput::from_boundary_json(
            &json!({
                "kind": "discord_slash_command",
                "status": "ignored_unknown_command",
                "command": command,
            }),
        )?)),
    }
}

fn command_child(payload: &DiscordSlashCommandPayload, command_kind: CommandKind) -> Result<Job> {
    let target = slash_option_string(payload, &["room", "channel", "voice_channel", "target"]);
    let command = CommandRequest::from_json(&json!({
        "action": "dispatch_now",
        "command_kind": command_kind.as_str(),
        "guild_id": payload.guild_id,
        "voice_channel_id": target,
        "requested_by_user_id": payload.user_id,
        "requested_by_speaker_label": payload.username,
        "target_voice_channel_id": target,
        "arguments": {
            "channel": target,
            "target_channel": target,
        },
    }))?;
    Ok(Job::command_request(
        command.guild_id.clone(),
        command.voice_channel_id.clone(),
        command.requested_by_user_id.clone(),
        command,
    ))
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
