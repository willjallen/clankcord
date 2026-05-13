use std::collections::BTreeMap;
use std::path::PathBuf;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value, json};

use crate::Result;
use crate::runtime::timeline::parse_duration;

use super::JobKind;
use super::util::{first_non_empty, insert_non_empty, string_array, string_field, truthy};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum OpaqueValue {
    Null,
    Bool(bool),
    I64(i64),
    F64(f64),
    String(String),
    Array(Vec<OpaqueValue>),
    Object(BTreeMap<String, OpaqueValue>),
}

impl OpaqueValue {
    pub fn from_json(value: &Value) -> Self {
        match value {
            Value::Null => Self::Null,
            Value::Bool(value) => Self::Bool(*value),
            Value::Number(value) => value
                .as_i64()
                .map(Self::I64)
                .or_else(|| value.as_f64().map(Self::F64))
                .unwrap_or(Self::Null),
            Value::String(value) => Self::String(value.clone()),
            Value::Array(values) => Self::Array(values.iter().map(Self::from_json).collect()),
            Value::Object(values) => Self::Object(
                values
                    .iter()
                    .map(|(key, value)| (key.clone(), Self::from_json(value)))
                    .collect(),
            ),
        }
    }

    pub fn to_json(&self) -> Value {
        match self {
            Self::Null => Value::Null,
            Self::Bool(value) => Value::Bool(*value),
            Self::I64(value) => Value::Number(Number::from(*value)),
            Self::F64(value) => Number::from_f64(*value)
                .map(Value::Number)
                .unwrap_or(Value::Null),
            Self::String(value) => Value::String(value.clone()),
            Self::Array(values) => Value::Array(values.iter().map(Self::to_json).collect()),
            Self::Object(values) => Value::Object(
                values
                    .iter()
                    .map(|(key, value)| (key.clone(), value.to_json()))
                    .collect(),
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct BinaryPayload {
    bytes: Vec<u8>,
}

impl BinaryPayload {
    pub fn empty() -> Self {
        Self { bytes: Vec::new() }
    }

    pub fn from_json(value: &Value) -> Result<Self> {
        Ok(Self {
            bytes: bincode::serialize(&OpaqueValue::from_json(value))?,
        })
    }

    pub fn to_json(&self) -> Value {
        if self.bytes.is_empty() {
            return json!({});
        }
        bincode::deserialize::<OpaqueValue>(&self.bytes)
            .map(|value| value.to_json())
            .unwrap_or_else(|_| json!({}))
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RouterAction {
    DispatchNow,
    WaitForMore,
    Ignore,
    CancelJob,
    AmendJob,
    ReplaceJob,
}

impl RouterAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DispatchNow => "dispatch_now",
            Self::WaitForMore => "wait_for_more",
            Self::Ignore => "ignore",
            Self::CancelJob => "cancel_job",
            Self::AmendJob => "amend_job",
            Self::ReplaceJob => "replace_job",
        }
    }
}

impl Default for RouterAction {
    fn default() -> Self {
        Self::DispatchNow
    }
}

impl FromStr for RouterAction {
    type Err = anyhow::Error;

    fn from_str(raw: &str) -> Result<Self> {
        match raw.trim() {
            "" | "dispatch_now" => Ok(Self::DispatchNow),
            "wait_for_more" => Ok(Self::WaitForMore),
            "ignore" => Ok(Self::Ignore),
            "cancel_job" => Ok(Self::CancelJob),
            "amend_job" => Ok(Self::AmendJob),
            "replace_job" => Ok(Self::ReplaceJob),
            value => anyhow::bail!("unknown router action: {value}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RouterCommandKind {
    AgentTask,
    StartLiveTranscript,
    StartDraftTranscript,
    MaterializeTranscript,
    MakePermanent,
    PauseListening,
    DeafenListening,
    ResumeListening,
    ForgetWindow,
    LeaveRoom,
    JoinRoom,
}

impl RouterCommandKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AgentTask => "agent_task",
            Self::StartLiveTranscript => "start_live_transcript",
            Self::StartDraftTranscript => "start_draft_transcript",
            Self::MaterializeTranscript => "materialize_transcript",
            Self::MakePermanent => "make_permanent",
            Self::PauseListening => "pause_listening",
            Self::DeafenListening => "deafen_listening",
            Self::ResumeListening => "resume_listening",
            Self::ForgetWindow => "forget_window",
            Self::LeaveRoom => "leave_room",
            Self::JoinRoom => "join_room",
        }
    }

    pub fn job_kind(self) -> &'static str {
        match self {
            Self::AgentTask => "agent_task",
            Self::StartLiveTranscript
            | Self::StartDraftTranscript
            | Self::MaterializeTranscript => "materialize_transcript",
            Self::MakePermanent => "make_permanent",
            Self::PauseListening => "pause_listening",
            Self::DeafenListening => "deafen_listening",
            Self::ResumeListening => "resume_listening",
            Self::ForgetWindow => "forget_window",
            Self::LeaveRoom => "leave_room",
            Self::JoinRoom => "join_room",
        }
    }
}

impl Default for RouterCommandKind {
    fn default() -> Self {
        Self::AgentTask
    }
}

impl FromStr for RouterCommandKind {
    type Err = anyhow::Error;

    fn from_str(raw: &str) -> Result<Self> {
        match raw.trim() {
            "" | "agent_task" => Ok(Self::AgentTask),
            "start_live_transcript" => Ok(Self::StartLiveTranscript),
            "start_draft_transcript" => Ok(Self::StartDraftTranscript),
            "materialize_transcript" => Ok(Self::MaterializeTranscript),
            "make_permanent" => Ok(Self::MakePermanent),
            "pause_listening" => Ok(Self::PauseListening),
            "deafen_listening" => Ok(Self::DeafenListening),
            "resume_listening" => Ok(Self::ResumeListening),
            "forget_window" => Ok(Self::ForgetWindow),
            "leave_room" => Ok(Self::LeaveRoom),
            "join_room" => Ok(Self::JoinRoom),
            value => anyhow::bail!("unknown router command kind: {value}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CommandArguments {
    pub query: String,
    pub question: String,
    pub request: String,
    pub instruction_text: String,
    pub relative_start: String,
    pub window_id: String,
    pub from: String,
    pub to: String,
    pub room: String,
    pub channel: String,
    pub target_room: String,
    pub target_channel: String,
    pub publish: String,
    pub refine: Option<bool>,
    pub duration_seconds: Option<i64>,
    pub unpublished_only: Option<bool>,
    opaque: BinaryPayload,
}

impl CommandArguments {
    pub fn from_json(value: Option<&Value>) -> Result<Self> {
        let object = value
            .filter(|value| value.is_object())
            .unwrap_or(&Value::Null);
        Ok(Self {
            query: string_field(object, "query"),
            question: string_field(object, "question"),
            request: string_field(object, "request"),
            instruction_text: string_field(object, "instruction_text"),
            relative_start: string_field(object, "relative_start"),
            window_id: first_non_empty([
                string_field(object, "window_id"),
                string_field(object, "windowId"),
            ]),
            from: string_field(object, "from"),
            to: string_field(object, "to"),
            room: string_field(object, "room"),
            channel: string_field(object, "channel"),
            target_room: string_field(object, "target_room"),
            target_channel: string_field(object, "target_channel"),
            publish: string_field(object, "publish"),
            refine: object.get("refine").and_then(Value::as_bool),
            duration_seconds: i64_field(object, &["duration_seconds", "durationSeconds"]),
            unpublished_only: bool_field(object, &["unpublished_only", "unpublishedOnly"]),
            opaque: BinaryPayload::from_json(object)?,
        })
    }

    pub fn to_json(&self) -> Value {
        let mut map = object_from_payload(&self.opaque);
        insert_non_empty(&mut map, "query", &self.query);
        insert_non_empty(&mut map, "question", &self.question);
        insert_non_empty(&mut map, "request", &self.request);
        insert_non_empty(&mut map, "instruction_text", &self.instruction_text);
        insert_non_empty(&mut map, "relative_start", &self.relative_start);
        insert_non_empty(&mut map, "window_id", &self.window_id);
        insert_non_empty(&mut map, "from", &self.from);
        insert_non_empty(&mut map, "to", &self.to);
        insert_non_empty(&mut map, "room", &self.room);
        insert_non_empty(&mut map, "channel", &self.channel);
        insert_non_empty(&mut map, "target_room", &self.target_room);
        insert_non_empty(&mut map, "target_channel", &self.target_channel);
        insert_non_empty(&mut map, "publish", &self.publish);
        if let Some(refine) = self.refine {
            map.insert("refine".to_string(), Value::Bool(refine));
        }
        if let Some(duration_seconds) = self.duration_seconds {
            map.insert(
                "duration_seconds".to_string(),
                Value::Number(Number::from(duration_seconds)),
            );
        }
        if let Some(unpublished_only) = self.unpublished_only {
            map.insert(
                "unpublished_only".to_string(),
                Value::Bool(unpublished_only),
            );
        }
        Value::Object(map)
    }

    pub fn request_text(&self) -> String {
        first_non_empty([
            self.query.clone(),
            self.question.clone(),
            self.request.clone(),
            self.instruction_text.clone(),
        ])
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouterCommand {
    pub action: RouterAction,
    pub command_kind: RouterCommandKind,
    pub guild_id: String,
    pub voice_channel_id: String,
    pub requested_by_user_id: String,
    pub requested_by_speaker_label: String,
    pub target_room_id: String,
    pub target_voice_channel_id: String,
    pub acknowledgement_text: String,
    pub requires_confirmation: bool,
    pub approved_by_user_id: String,
    pub target_job_id: String,
    pub target_job_ids: Vec<String>,
    pub arguments: CommandArguments,
    opaque: BinaryPayload,
}

impl RouterCommand {
    pub fn from_json(value: &Value) -> Result<Self> {
        if !value.is_object() {
            anyhow::bail!("router command must be a JSON object at the boundary");
        }
        let command_kind = RouterCommandKind::from_str(&string_field(value, "command_kind"))?;
        Ok(Self {
            action: RouterAction::from_str(&string_field(value, "action"))?,
            command_kind,
            guild_id: string_field(value, "guild_id"),
            voice_channel_id: string_field(value, "voice_channel_id"),
            requested_by_user_id: string_field(value, "requested_by_user_id"),
            requested_by_speaker_label: string_field(value, "requested_by_speaker_label"),
            target_room_id: first_non_empty([
                string_field(value, "target_room_id"),
                string_field(value, "targetRoomId"),
            ]),
            target_voice_channel_id: first_non_empty([
                string_field(value, "target_voice_channel_id"),
                string_field(value, "targetVoiceChannelId"),
            ]),
            acknowledgement_text: string_field(value, "acknowledgement_text"),
            requires_confirmation: truthy(value.get("requires_confirmation"), false),
            approved_by_user_id: string_field(value, "approved_by_user_id"),
            target_job_id: string_field(value, "target_job_id"),
            target_job_ids: string_array(value, "target_job_ids"),
            arguments: CommandArguments::from_json(value.get("arguments"))?,
            opaque: BinaryPayload::from_json(value)?,
        })
    }

    pub fn to_json(&self) -> Value {
        let mut map = object_from_payload(&self.opaque);
        map.insert(
            "action".to_string(),
            Value::String(self.action.as_str().to_string()),
        );
        map.insert(
            "command_kind".to_string(),
            Value::String(self.command_kind.as_str().to_string()),
        );
        insert_non_empty(&mut map, "guild_id", &self.guild_id);
        insert_non_empty(&mut map, "voice_channel_id", &self.voice_channel_id);
        insert_non_empty(&mut map, "requested_by_user_id", &self.requested_by_user_id);
        insert_non_empty(
            &mut map,
            "requested_by_speaker_label",
            &self.requested_by_speaker_label,
        );
        insert_non_empty(&mut map, "target_room_id", &self.target_room_id);
        insert_non_empty(
            &mut map,
            "target_voice_channel_id",
            &self.target_voice_channel_id,
        );
        insert_non_empty(&mut map, "acknowledgement_text", &self.acknowledgement_text);
        insert_non_empty(&mut map, "approved_by_user_id", &self.approved_by_user_id);
        insert_non_empty(&mut map, "target_job_id", &self.target_job_id);
        if !self.target_job_ids.is_empty() {
            map.insert(
                "target_job_ids".to_string(),
                Value::Array(
                    self.target_job_ids
                        .iter()
                        .map(|value| Value::String(value.clone()))
                        .collect(),
                ),
            );
        }
        map.insert(
            "requires_confirmation".to_string(),
            Value::Bool(self.requires_confirmation),
        );
        map.insert("arguments".to_string(), self.arguments.to_json());
        Value::Object(map)
    }

    pub fn clear_confirmation_requirement(&mut self, approved_by_user_id: impl Into<String>) {
        self.requires_confirmation = false;
        self.approved_by_user_id = approved_by_user_id.into();
    }

    pub fn target_room_identifier(&self, default_channel_id: &str) -> String {
        first_non_empty([
            self.target_room_id.clone(),
            self.target_voice_channel_id.clone(),
            self.arguments.room.clone(),
            self.arguments.channel.clone(),
            self.arguments.target_room.clone(),
            self.arguments.target_channel.clone(),
            default_channel_id.to_string(),
        ])
    }

    pub fn window_times(&self, now: Option<DateTime<Utc>>) -> (DateTime<Utc>, DateTime<Utc>) {
        let current = now.unwrap_or_else(Utc::now);
        let relative_start = if self.arguments.relative_start.trim().is_empty() {
            "-10m"
        } else {
            self.arguments.relative_start.trim()
        };
        let delta =
            parse_duration(relative_start).unwrap_or_else(|| chrono::Duration::minutes(-10));
        (current + delta, current)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfirmationContext {
    pub sensitive: bool,
    pub delivery: String,
    pub target_window_start: String,
    pub target_window_end: String,
    pub target_window_duration_seconds: i64,
    pub source_preview: Vec<String>,
    pub created_at: String,
}

impl ConfirmationContext {
    pub fn to_json(&self) -> Value {
        json!({
            "sensitive": self.sensitive,
            "delivery": self.delivery,
            "target_window": {
                "start": self.target_window_start,
                "end": self.target_window_end,
                "duration_seconds": self.target_window_duration_seconds,
            },
            "source_preview": self.source_preview,
            "created_at": self.created_at,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioSegmentPayload {
    pub guild_id: String,
    pub guild_slug: String,
    pub voice_channel_id: String,
    pub voice_channel_name: String,
    pub voice_channel_slug: String,
    pub capture_run_id: String,
    pub voice_bot_id: String,
    pub voice_bot_discord_user_id: String,
    pub speaker_user_id: String,
    pub speaker_label: String,
    pub speaker_username: String,
    pub segment_start_time: DateTime<Utc>,
    pub segment_end_time: DateTime<Utc>,
    pub segment_index: i64,
    pub duration_ms: i64,
    pub source_audio_path: PathBuf,
    pub audio_checksum: String,
    pub audio_bytes: u64,
    pub audio_format: String,
    pub sample_rate_hz: u32,
    pub channels: u16,
    pub sample_width_bits: u16,
    pub post_processing: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentTaskPayload {
    pub command: RouterCommand,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefineTranscriptPayload {
    pub window_id: String,
    pub publication_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfirmationRequiredPayload {
    pub command: RouterCommand,
    pub confirmation: ConfirmationContext,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouterCommandPayload {
    pub command: RouterCommand,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RoomAgentPlacementAction {
    Join,
    Leave,
}

impl RoomAgentPlacementAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Join => "join",
            Self::Leave => "leave",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomAgentPlacementPayload {
    pub action: RoomAgentPlacementAction,
    pub room_id: String,
    pub reason: String,
    pub decision_key: String,
    pub cooldown_seconds: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuntimeControlAction {
    RetryJob,
    ApproveConfirmation,
    CancelConfirmation,
}

impl RuntimeControlAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RetryJob => "retry_job",
            Self::ApproveConfirmation => "approve_confirmation",
            Self::CancelConfirmation => "cancel_confirmation",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeControlPayload {
    pub action: RuntimeControlAction,
    pub target_job_id: String,
    pub actor_user_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum JobPayload {
    AudioSegment(AudioSegmentPayload),
    AgentTask(AgentTaskPayload),
    RefineTranscript(RefineTranscriptPayload),
    ConfirmationRequired(ConfirmationRequiredPayload),
    RouterCommand(RouterCommandPayload),
    RoomAgentPlacement(RoomAgentPlacementPayload),
    RuntimeControl(RuntimeControlPayload),
}

impl JobPayload {
    pub fn kind(&self) -> JobKind {
        match self {
            Self::AudioSegment(_) => JobKind::AudioSegment,
            Self::AgentTask(_) => JobKind::AgentTask,
            Self::RefineTranscript(_) => JobKind::RefineTranscript,
            Self::ConfirmationRequired(_) => JobKind::ConfirmationRequired,
            Self::RouterCommand(_) => JobKind::RouterCommand,
            Self::RoomAgentPlacement(_) => JobKind::RoomAgentPlacement,
            Self::RuntimeControl(_) => JobKind::RuntimeControl,
        }
    }

    pub fn command(&self) -> Option<&RouterCommand> {
        match self {
            Self::AudioSegment(_) => None,
            Self::AgentTask(payload) => Some(&payload.command),
            Self::ConfirmationRequired(payload) => Some(&payload.command),
            Self::RouterCommand(payload) => Some(&payload.command),
            Self::RoomAgentPlacement(_) => None,
            Self::RuntimeControl(_) => None,
            Self::RefineTranscript(_) => None,
        }
    }

    pub fn command_mut(&mut self) -> Option<&mut RouterCommand> {
        match self {
            Self::AudioSegment(_) => None,
            Self::AgentTask(payload) => Some(&mut payload.command),
            Self::ConfirmationRequired(payload) => Some(&mut payload.command),
            Self::RouterCommand(payload) => Some(&mut payload.command),
            Self::RoomAgentPlacement(_) => None,
            Self::RuntimeControl(_) => None,
            Self::RefineTranscript(_) => None,
        }
    }

    pub fn command_kind(&self) -> String {
        self.command()
            .map(|command| command.command_kind.as_str().to_string())
            .unwrap_or_default()
    }

    pub fn to_json(&self) -> Value {
        match self {
            Self::AudioSegment(payload) => json!({
                "guild_id": payload.guild_id,
                "guild_slug": payload.guild_slug,
                "voice_channel_id": payload.voice_channel_id,
                "voice_channel_name": payload.voice_channel_name,
                "voice_channel_slug": payload.voice_channel_slug,
                "capture_run_id": payload.capture_run_id,
                "voice_bot_id": payload.voice_bot_id,
                "voice_bot_discord_user_id": payload.voice_bot_discord_user_id,
                "speaker_user_id": payload.speaker_user_id,
                "speaker_label": payload.speaker_label,
                "speaker_username": payload.speaker_username,
                "segment_start_time": payload.segment_start_time.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                "segment_end_time": payload.segment_end_time.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                "segment_index": payload.segment_index,
                "duration_ms": payload.duration_ms,
                "source_audio_path": payload.source_audio_path.display().to_string(),
                "audio_checksum": payload.audio_checksum,
                "audio_bytes": payload.audio_bytes,
                "audio_format": payload.audio_format,
                "sample_rate_hz": payload.sample_rate_hz,
                "channels": payload.channels,
                "sample_width_bits": payload.sample_width_bits,
                "post_processing": payload.post_processing,
            }),
            Self::AgentTask(payload) => json!({"command": payload.command.to_json()}),
            Self::RefineTranscript(payload) => json!({
                "window_id": payload.window_id,
                "publication_id": payload.publication_id,
            }),
            Self::ConfirmationRequired(payload) => json!({
                "command": payload.command.to_json(),
                "confirmation": payload.confirmation.to_json(),
            }),
            Self::RouterCommand(payload) => json!({"command": payload.command.to_json()}),
            Self::RoomAgentPlacement(payload) => {
                let mut object = Map::new();
                object.insert(
                    "action".to_string(),
                    Value::String(payload.action.as_str().to_string()),
                );
                insert_non_empty(&mut object, "room_id", &payload.room_id);
                insert_non_empty(&mut object, "reason", &payload.reason);
                insert_non_empty(&mut object, "decision_key", &payload.decision_key);
                if let Some(cooldown_seconds) = payload.cooldown_seconds {
                    object.insert(
                        "cooldown_seconds".to_string(),
                        Value::Number(Number::from(cooldown_seconds)),
                    );
                }
                Value::Object(object)
            }
            Self::RuntimeControl(payload) => json!({
                "action": payload.action.as_str(),
                "target_job_id": payload.target_job_id,
                "actor_user_id": payload.actor_user_id,
            }),
        }
    }
}

fn object_from_payload(payload: &BinaryPayload) -> Map<String, Value> {
    payload.to_json().as_object().cloned().unwrap_or_default()
}

fn i64_field(value: &Value, keys: &[&str]) -> Option<i64> {
    keys.iter().find_map(|key| match value.get(*key) {
        Some(Value::Number(number)) => number.as_i64(),
        Some(Value::String(text)) => text.trim().parse::<i64>().ok(),
        _ => None,
    })
}

fn bool_field(value: &Value, keys: &[&str]) -> Option<bool> {
    keys.iter().find_map(|key| match value.get(*key) {
        Some(Value::Bool(value)) => Some(*value),
        Some(Value::Number(value)) => Some(value.as_i64().unwrap_or(0) != 0),
        Some(Value::String(text)) => match text.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Some(true),
            "0" | "false" | "no" | "off" => Some(false),
            _ => None,
        },
        _ => None,
    })
}
