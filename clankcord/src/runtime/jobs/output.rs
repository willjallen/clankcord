use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::Result;
use crate::runtime::bots::RuntimeBotStatus;
use crate::runtime::rooms::RoomConfig;
use crate::runtime::sessions::RuntimeSessionStatus;

use super::payload::{
    BinaryPayload, DiscordVoicePlaybackCue, ResponseSink, RoomAgentPlacementAction,
    RuntimeControlAction,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobFailure {
    pub message: String,
}

impl JobFailure {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobCreatedOutput {
    pub kind: String,
    pub job_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeControlOutput {
    pub action: RuntimeControlAction,
    pub target_job_id: String,
    pub target_job_ids: Vec<String>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResponseOutput {
    pub response_kind: String,
    pub sink: ResponseSink,
    pub source_job_id: String,
    pub content: String,
    pub discord_post: Option<super::record::DiscordPostMetadata>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomAgentPlacementOutput {
    pub action: RoomAgentPlacementAction,
    pub status: String,
    pub room: RoomConfig,
    pub bot_id: String,
    pub capture_run_id: String,
    pub requested_by_user_id: String,
    pub reason: String,
    pub session: Option<RuntimeSessionStatus>,
    pub sessions: Vec<RuntimeSessionStatus>,
    pub bots: Vec<RuntimeBotStatus>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscordVoiceJoinOutput {
    pub status: String,
    pub session: Option<RuntimeSessionStatus>,
    pub bot_status: Option<RuntimeBotStatus>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscordVoiceLeaveOutput {
    pub session_id: String,
    pub status: String,
    pub session: Option<RuntimeSessionStatus>,
    pub bot_status: Option<RuntimeBotStatus>,
    pub guild_id: String,
    pub voice_channel_id: String,
    pub capture_run_id: String,
    pub audio_jobs: Vec<super::record::Job>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscordVoicePlaybackOutput {
    pub session_id: String,
    pub cue: DiscordVoicePlaybackCue,
    pub status: String,
    pub guild_id: String,
    pub voice_channel_id: String,
    pub audio_path: String,
    pub duration_ms: i64,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscordVoiceMuteOutput {
    pub session_id: String,
    pub muted: bool,
    pub status: String,
    pub guild_id: String,
    pub voice_channel_id: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscordVoicePlayAudioOutput {
    pub session_id: String,
    pub cue: DiscordVoicePlaybackCue,
    pub status: String,
    pub guild_id: String,
    pub voice_channel_id: String,
    pub audio_path: String,
    pub duration_ms: i64,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum JobOutput {
    Empty,
    JobCreated(JobCreatedOutput),
    RuntimeControl(RuntimeControlOutput),
    Response(ResponseOutput),
    RoomAgentPlacement(RoomAgentPlacementOutput),
    DiscordVoiceJoin(DiscordVoiceJoinOutput),
    DiscordVoiceLeave(DiscordVoiceLeaveOutput),
    DiscordVoicePlayback(DiscordVoicePlaybackOutput),
    DiscordVoiceMute(DiscordVoiceMuteOutput),
    DiscordVoicePlayAudio(DiscordVoicePlayAudioOutput),
    Record(BinaryPayload),
}

impl JobOutput {
    pub fn from_boundary_json(value: &Value) -> Result<Self> {
        Ok(Self::Record(BinaryPayload::from_json(value)?))
    }

    pub fn to_json(&self) -> Value {
        match self {
            Self::Empty => json!({}),
            Self::JobCreated(output) => json!({
                "kind": output.kind,
                "job_ids": output.job_ids,
            }),
            Self::RuntimeControl(output) => json!({
                "kind": "runtime_control",
                "action": output.action.as_str(),
                "target_job_id": output.target_job_id,
                "target_job_ids": output.target_job_ids,
                "message": output.message,
            }),
            Self::Response(output) => {
                let mut object = Map::new();
                object.insert("kind".to_string(), json!("response"));
                object.insert("response_kind".to_string(), json!(output.response_kind));
                object.insert("sink".to_string(), output.sink.to_json());
                object.insert("source_job_id".to_string(), json!(output.source_job_id));
                if !output.content.trim().is_empty() {
                    object.insert("content".to_string(), json!(output.content));
                }
                if let Some(post) = &output.discord_post {
                    object.insert("discord_post".to_string(), post.to_json());
                }
                Value::Object(object)
            }
            Self::RoomAgentPlacement(output) => {
                let mut object = Map::new();
                object.insert("kind".to_string(), json!("room_agent_placement"));
                object.insert("action".to_string(), json!(output.action.as_str()));
                object.insert("status".to_string(), json!(output.status));
                object.insert("room".to_string(), output.room.to_json());
                object.insert("roomId".to_string(), json!(output.room.room_id));
                object.insert("guildId".to_string(), json!(output.room.guild_id));
                object.insert("channelId".to_string(), json!(output.room.channel_id));
                insert_non_empty(&mut object, "botId", &output.bot_id);
                insert_non_empty(&mut object, "captureRunId", &output.capture_run_id);
                insert_non_empty(&mut object, "requestedUserId", &output.requested_by_user_id);
                insert_non_empty(&mut object, "reason", &output.reason);
                insert_non_empty(&mut object, "message", &output.message);
                if let Some(session) = &output.session {
                    object.insert("session".to_string(), session.to_json());
                }
                if !output.sessions.is_empty() {
                    object.insert(
                        "sessions".to_string(),
                        Value::Array(
                            output
                                .sessions
                                .iter()
                                .map(RuntimeSessionStatus::to_json)
                                .collect(),
                        ),
                    );
                }
                if !output.bots.is_empty() {
                    object.insert(
                        "bots".to_string(),
                        Value::Array(output.bots.iter().map(RuntimeBotStatus::to_json).collect()),
                    );
                }
                Value::Object(object)
            }
            Self::DiscordVoiceJoin(output) => {
                let mut object = Map::new();
                object.insert("kind".to_string(), json!("discord_voice_join"));
                object.insert("status".to_string(), json!(output.status));
                insert_non_empty(&mut object, "message", &output.message);
                if let Some(session) = &output.session {
                    object.insert("session".to_string(), session.to_json());
                }
                if let Some(status) = &output.bot_status {
                    object.insert("bot_status".to_string(), status.to_json());
                }
                Value::Object(object)
            }
            Self::DiscordVoiceLeave(output) => {
                let mut object = Map::new();
                object.insert("kind".to_string(), json!("discord_voice_leave"));
                object.insert("session_id".to_string(), json!(output.session_id));
                object.insert("status".to_string(), json!(output.status));
                insert_non_empty(&mut object, "guild_id", &output.guild_id);
                insert_non_empty(&mut object, "voice_channel_id", &output.voice_channel_id);
                insert_non_empty(&mut object, "capture_run_id", &output.capture_run_id);
                if let Some(session) = &output.session {
                    object.insert("session".to_string(), session.to_json());
                }
                if let Some(status) = &output.bot_status {
                    object.insert("bot_status".to_string(), status.to_json());
                }
                if !output.audio_jobs.is_empty() {
                    object.insert(
                        "audio_job_ids".to_string(),
                        Value::Array(
                            output
                                .audio_jobs
                                .iter()
                                .map(|job| Value::String(job.id.clone()))
                                .collect(),
                        ),
                    );
                }
                Value::Object(object)
            }
            Self::DiscordVoicePlayback(output) => {
                let mut object = Map::new();
                object.insert("kind".to_string(), json!("discord_voice_playback"));
                object.insert("session_id".to_string(), json!(output.session_id));
                object.insert("cue".to_string(), json!(output.cue.as_str()));
                object.insert("status".to_string(), json!(output.status));
                insert_non_empty(&mut object, "guild_id", &output.guild_id);
                insert_non_empty(&mut object, "voice_channel_id", &output.voice_channel_id);
                insert_non_empty(&mut object, "audio_path", &output.audio_path);
                object.insert("duration_ms".to_string(), json!(output.duration_ms));
                insert_non_empty(&mut object, "message", &output.message);
                Value::Object(object)
            }
            Self::DiscordVoiceMute(output) => {
                let mut object = Map::new();
                object.insert("kind".to_string(), json!("discord_voice_mute"));
                object.insert("session_id".to_string(), json!(output.session_id));
                object.insert("muted".to_string(), json!(output.muted));
                object.insert("status".to_string(), json!(output.status));
                insert_non_empty(&mut object, "guild_id", &output.guild_id);
                insert_non_empty(&mut object, "voice_channel_id", &output.voice_channel_id);
                insert_non_empty(&mut object, "message", &output.message);
                Value::Object(object)
            }
            Self::DiscordVoicePlayAudio(output) => {
                let mut object = Map::new();
                object.insert("kind".to_string(), json!("discord_voice_play_audio"));
                object.insert("session_id".to_string(), json!(output.session_id));
                object.insert("cue".to_string(), json!(output.cue.as_str()));
                object.insert("status".to_string(), json!(output.status));
                insert_non_empty(&mut object, "guild_id", &output.guild_id);
                insert_non_empty(&mut object, "voice_channel_id", &output.voice_channel_id);
                insert_non_empty(&mut object, "audio_path", &output.audio_path);
                object.insert("duration_ms".to_string(), json!(output.duration_ms));
                insert_non_empty(&mut object, "message", &output.message);
                Value::Object(object)
            }
            Self::Record(payload) => payload.to_json(),
        }
    }
}

fn insert_non_empty(object: &mut Map<String, Value>, key: &str, value: &str) {
    if !value.trim().is_empty() {
        object.insert(key.to_string(), Value::String(value.to_string()));
    }
}
