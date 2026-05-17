use std::collections::BTreeMap;
use std::path::PathBuf;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value, json};

use crate::Result;
use crate::runtime::rooms::RoomConfig;
use crate::runtime::timeline::parse_duration;
use crate::runtime::util::{first_non_empty, string_array, string_field};

use super::JobKind;
use super::util::{insert_non_empty, truthy};

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
pub enum CommandAction {
    DispatchNow,
    WaitForMore,
    Ignore,
    CancelJob,
    AmendJob,
    ReplaceJob,
}

impl CommandAction {
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

impl Default for CommandAction {
    fn default() -> Self {
        Self::DispatchNow
    }
}

impl FromStr for CommandAction {
    type Err = anyhow::Error;

    fn from_str(raw: &str) -> Result<Self> {
        match raw.trim() {
            "" | "dispatch_now" => Ok(Self::DispatchNow),
            "wait_for_more" => Ok(Self::WaitForMore),
            "ignore" => Ok(Self::Ignore),
            "cancel_job" => Ok(Self::CancelJob),
            "amend_job" => Ok(Self::AmendJob),
            "replace_job" => Ok(Self::ReplaceJob),
            value => anyhow::bail!("unknown command action: {value}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CommandKind {
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
    SetVoiceMute,
    PlayVoiceCue,
}

impl CommandKind {
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
            Self::SetVoiceMute => "set_voice_mute",
            Self::PlayVoiceCue => "play_voice_cue",
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
            Self::SetVoiceMute => "set_voice_mute",
            Self::PlayVoiceCue => "play_voice_cue",
        }
    }
}

impl Default for CommandKind {
    fn default() -> Self {
        Self::AgentTask
    }
}

impl FromStr for CommandKind {
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
            "set_voice_mute" => Ok(Self::SetVoiceMute),
            "play_voice_cue" => Ok(Self::PlayVoiceCue),
            value => anyhow::bail!("unknown command kind: {value}"),
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
    pub cue: String,
    pub refine: Option<bool>,
    pub duration_seconds: Option<i64>,
    pub muted: Option<bool>,
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
            cue: string_field(object, "cue"),
            refine: object.get("refine").and_then(Value::as_bool),
            duration_seconds: i64_field(object, &["duration_seconds", "durationSeconds"]),
            muted: bool_field(object, &["muted"]),
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
        insert_non_empty(&mut map, "cue", &self.cue);
        if let Some(refine) = self.refine {
            map.insert("refine".to_string(), Value::Bool(refine));
        }
        if let Some(duration_seconds) = self.duration_seconds {
            map.insert(
                "duration_seconds".to_string(),
                Value::Number(Number::from(duration_seconds)),
            );
        }
        if let Some(muted) = self.muted {
            map.insert("muted".to_string(), Value::Bool(muted));
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
pub struct CommandRequest {
    pub action: CommandAction,
    pub command_kind: CommandKind,
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

impl CommandRequest {
    pub fn agent_task(
        guild_id: impl Into<String>,
        voice_channel_id: impl Into<String>,
        requested_by_user_id: impl Into<String>,
        request: impl Into<String>,
    ) -> Self {
        Self::new_internal(
            CommandKind::AgentTask,
            guild_id,
            voice_channel_id,
            requested_by_user_id,
            CommandArguments {
                request: request.into(),
                ..CommandArguments::default()
            },
        )
    }

    pub fn start_live_transcript(
        guild_id: impl Into<String>,
        voice_channel_id: impl Into<String>,
        requested_by_user_id: impl Into<String>,
        title: impl Into<String>,
    ) -> Self {
        Self::new_internal(
            CommandKind::StartLiveTranscript,
            guild_id,
            voice_channel_id,
            requested_by_user_id,
            CommandArguments {
                request: title.into(),
                publish: "discord".to_string(),
                ..CommandArguments::default()
            },
        )
    }

    fn new_internal(
        command_kind: CommandKind,
        guild_id: impl Into<String>,
        voice_channel_id: impl Into<String>,
        requested_by_user_id: impl Into<String>,
        arguments: CommandArguments,
    ) -> Self {
        Self {
            action: CommandAction::DispatchNow,
            command_kind,
            guild_id: guild_id.into(),
            voice_channel_id: voice_channel_id.into(),
            requested_by_user_id: requested_by_user_id.into(),
            requested_by_speaker_label: String::new(),
            target_room_id: String::new(),
            target_voice_channel_id: String::new(),
            acknowledgement_text: String::new(),
            requires_confirmation: false,
            approved_by_user_id: String::new(),
            target_job_id: String::new(),
            target_job_ids: Vec::new(),
            arguments,
            opaque: BinaryPayload::empty(),
        }
    }

    pub fn from_json(value: &Value) -> Result<Self> {
        if !value.is_object() {
            anyhow::bail!("command must be a JSON object at the boundary");
        }
        let command_kind = CommandKind::from_str(&string_field(value, "command_kind"))?;
        Ok(Self {
            action: CommandAction::from_str(&string_field(value, "action"))?,
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
pub struct WakeProbePayload {
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
    pub probe_start_time: DateTime<Utc>,
    pub probe_end_time: DateTime<Utc>,
    pub probe_index: i64,
    pub duration_ms: i64,
    pub source_audio_path: PathBuf,
    pub audio_checksum: String,
    pub audio_bytes: u64,
    pub audio_format: String,
    pub sample_rate_hz: u32,
    pub channels: u16,
    pub sample_width_bits: u16,
    pub post_processing: String,
    pub stream_id: String,
    pub reset_stream: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WakeActivationPayload {
    pub activation_id: String,
    pub guild_id: String,
    pub voice_channel_id: String,
    pub voice_channel_name: String,
    pub speaker_user_id: String,
    pub speaker_label: String,
    pub wake_event_id: String,
    pub wake_started_at: String,
    pub wake_ended_at: String,
    pub latest_wake_event_id: String,
    pub latest_wake_at: String,
    pub lookback_seconds: i64,
    pub min_post_seconds: i64,
    pub speaker_idle_seconds: i64,
    pub stt_flush_grace_seconds: i64,
    pub max_window_seconds: i64,
    pub additive_preempt_seconds: i64,
    pub independent_after_seconds: i64,
    pub amended_wake_event_ids: Vec<String>,
    pub replacement_of_job_ids: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TextDeliveryKind {
    Message,
    Question,
}

impl TextDeliveryKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Message => "message",
            Self::Question => "question",
        }
    }
}

impl FromStr for TextDeliveryKind {
    type Err = anyhow::Error;

    fn from_str(raw: &str) -> Result<Self> {
        match raw.trim() {
            "message" => Ok(Self::Message),
            "question" => Ok(Self::Question),
            value => anyhow::bail!("unknown text delivery kind: {value}"),
        }
    }
}

impl Default for TextDeliveryKind {
    fn default() -> Self {
        Self::Message
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TextTargetKind {
    AgentSession,
    AgentChat,
    Channel,
    Dm,
}

impl TextTargetKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AgentSession => "session",
            Self::AgentChat => "agent_chat",
            Self::Channel => "channel",
            Self::Dm => "dm",
        }
    }
}

impl FromStr for TextTargetKind {
    type Err = anyhow::Error;

    fn from_str(raw: &str) -> Result<Self> {
        match raw.trim() {
            "session" => Ok(Self::AgentSession),
            "agent_chat" => Ok(Self::AgentChat),
            "channel" => Ok(Self::Channel),
            "dm" => Ok(Self::Dm),
            value if value.starts_with("dm:") => Ok(Self::Dm),
            value if value.starts_with("channel:") => Ok(Self::Channel),
            value => anyhow::bail!("unknown text target: {value}"),
        }
    }
}

impl Default for TextTargetKind {
    fn default() -> Self {
        Self::AgentSession
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TextTarget {
    pub kind: TextTargetKind,
    pub channel_id: String,
    pub user_id: String,
}

impl TextTarget {
    pub fn from_json(value: Option<&Value>) -> Result<Self> {
        let Some(value) = value else {
            return Ok(Self::default());
        };
        if let Some(raw) = value.as_str() {
            return Self::from_string(raw);
        }
        let raw_kind = string_field(value, "kind");
        let mut sink = Self {
            kind: TextTargetKind::from_str(&raw_kind)?,
            channel_id: string_field(value, "channel_id"),
            user_id: string_field(value, "user_id"),
        };
        if sink.channel_id.is_empty() {
            sink.channel_id = string_field(value, "channelId");
        }
        if sink.user_id.is_empty() {
            sink.user_id = string_field(value, "userId");
        }
        Ok(sink)
    }

    pub fn from_string(raw: &str) -> Result<Self> {
        let raw = raw.trim();
        let mut sink = Self {
            kind: TextTargetKind::from_str(raw)?,
            ..Self::default()
        };
        if let Some(channel_id) = raw.strip_prefix("channel:") {
            sink.channel_id = channel_id.trim().to_string();
        } else if let Some(user_id) = raw.strip_prefix("dm:") {
            sink.user_id = user_id.trim().to_string();
        }
        Ok(sink)
    }

    pub fn to_json(&self) -> Value {
        let mut map = Map::new();
        map.insert(
            "kind".to_string(),
            Value::String(self.kind.as_str().to_string()),
        );
        insert_non_empty(&mut map, "channel_id", &self.channel_id);
        insert_non_empty(&mut map, "user_id", &self.user_id);
        Value::Object(map)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextDeliveryPayload {
    pub intent: TextDeliveryKind,
    pub target: TextTarget,
    pub content: String,
    pub source_job_id: String,
    pub requested_by_user_id: String,
    pub expects_reply: bool,
    opaque: BinaryPayload,
}

impl TextDeliveryPayload {
    pub fn new(
        intent: TextDeliveryKind,
        target: TextTarget,
        content: impl Into<String>,
        source_job_id: impl Into<String>,
        requested_by_user_id: impl Into<String>,
        expects_reply: bool,
    ) -> Self {
        Self {
            intent,
            target,
            content: content.into(),
            source_job_id: source_job_id.into(),
            requested_by_user_id: requested_by_user_id.into(),
            expects_reply,
            opaque: BinaryPayload::empty(),
        }
    }

    pub fn from_json(value: &Value) -> Result<Self> {
        if !value.is_object() {
            anyhow::bail!("text delivery must be a JSON object at the boundary");
        }
        let intent = TextDeliveryKind::from_str(&string_field(value, "intent"))?;
        Ok(Self {
            intent,
            target: TextTarget::from_json(value.get("target"))?,
            content: string_field(value, "content"),
            source_job_id: string_field(value, "source_job_id"),
            requested_by_user_id: string_field(value, "requested_by_user_id"),
            expects_reply: truthy(
                value.get("expects_reply"),
                intent == TextDeliveryKind::Question,
            ),
            opaque: BinaryPayload::from_json(value)?,
        })
    }

    pub fn to_json(&self) -> Value {
        let mut map = object_from_payload(&self.opaque);
        map.insert(
            "intent".to_string(),
            Value::String(self.intent.as_str().to_string()),
        );
        map.insert("target".to_string(), self.target.to_json());
        insert_non_empty(&mut map, "content", &self.content);
        insert_non_empty(&mut map, "source_job_id", &self.source_job_id);
        insert_non_empty(&mut map, "requested_by_user_id", &self.requested_by_user_id);
        if self.expects_reply {
            map.insert("expects_reply".to_string(), Value::Bool(true));
        }
        Value::Object(map)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscordTextSendPayload {
    pub intent: TextDeliveryKind,
    pub target: TextTarget,
    pub content: String,
    pub source_job_id: String,
    pub requested_by_user_id: String,
    pub allowed_mentions: BinaryPayload,
    pub components: BinaryPayload,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscordForumThreadCreatePayload {
    pub parent_channel_id: String,
    pub name: String,
    pub content: String,
    pub auto_archive_minutes: i64,
    pub source_job_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentTaskPayload {
    pub agent_session_id: String,
    pub command: CommandRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSessionStartPayload {
    pub agent_session_id: String,
    pub guild_id: String,
    pub voice_channel_id: String,
    pub discord_parent_channel_id: String,
    pub requested_by_user_id: String,
    pub command: CommandRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscordTextMessagePayload {
    pub guild_id: String,
    pub channel_id: String,
    pub message_id: String,
    pub author_user_id: String,
    pub author_username: String,
    pub author_display_name: String,
    pub content: String,
    pub created_at: String,
    pub referenced_message_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscordSlashCommandPayload {
    pub interaction_id: String,
    pub interaction_token: String,
    pub application_id: String,
    pub guild_id: String,
    pub channel_id: String,
    pub voice_channel_id: String,
    pub user_id: String,
    pub username: String,
    pub command_name: String,
    pub options: BinaryPayload,
    pub created_at: String,
    pub response_visibility: String,
}

impl DiscordSlashCommandPayload {
    pub fn options_json(&self) -> Value {
        self.options.to_json()
    }

    pub fn timeline_channel_id(&self) -> &str {
        if matches!(
            self.command_name.as_str(),
            "join" | "leave" | "wake" | "deafen" | "undeafen"
        ) && !self.voice_channel_id.trim().is_empty()
        {
            &self.voice_channel_id
        } else {
            &self.channel_id
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefineTranscriptPayload {
    pub window_id: String,
    pub publication_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptPublicationPayload {
    pub publication_id: String,
    pub live: bool,
    pub refined_queued: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfirmationRequiredPayload {
    pub command: CommandRequest,
    pub confirmation: ConfirmationContext,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandPayload {
    pub command: CommandRequest,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscordVoiceJoinPayload {
    pub room: RoomConfig,
    pub bot_id: String,
    pub capture_run_id: String,
    pub assignment_id: String,
    pub started_at: DateTime<Utc>,
    pub session_dir: PathBuf,
    pub requested_by_user_id: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscordVoiceLeavePayload {
    pub session_id: String,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiscordVoicePlaybackCue {
    Join,
    Leave,
    Wake,
    Ack,
    Preempt,
    Deafen,
    Undeafen,
}

impl DiscordVoicePlaybackCue {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Join => "join",
            Self::Leave => "leave",
            Self::Wake => "wake",
            Self::Ack => "ack",
            Self::Preempt => "preempt",
            Self::Deafen => "deafen",
            Self::Undeafen => "undeafen",
        }
    }

    pub fn asset_file_name(self) -> &'static str {
        match self {
            Self::Join => "clanky-join.wav",
            Self::Leave => "clanky-leave.wav",
            Self::Wake => "clanky-wake.wav",
            Self::Ack => "clanky-ack.wav",
            Self::Preempt => "clanky-preempt.wav",
            Self::Deafen | Self::Undeafen => "clanky-deafen.wav",
        }
    }
}

impl FromStr for DiscordVoicePlaybackCue {
    type Err = anyhow::Error;

    fn from_str(raw: &str) -> Result<Self> {
        match raw.trim() {
            "join" => Ok(Self::Join),
            "leave" => Ok(Self::Leave),
            "wake" => Ok(Self::Wake),
            "ack" => Ok(Self::Ack),
            "preempt" => Ok(Self::Preempt),
            "deafen" => Ok(Self::Deafen),
            "undeafen" => Ok(Self::Undeafen),
            value => anyhow::bail!("unknown voice cue: {value}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscordVoicePlaybackPayload {
    pub session_id: String,
    pub cue: DiscordVoicePlaybackCue,
    pub source_job_id: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscordVoiceMutePayload {
    pub session_id: String,
    pub muted: bool,
    pub source_job_id: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscordVoiceDeafenPayload {
    pub session_id: String,
    pub deafened: bool,
    pub source_job_id: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscordVoicePlayAudioPayload {
    pub session_id: String,
    pub cue: DiscordVoicePlaybackCue,
    pub source_job_id: String,
    pub reason: String,
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
pub struct RuntimeMaintenancePayload {
    pub interval_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiceStatusSyncPayload {
    pub source_job_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscordVoiceStatusSnapshotPayload {
    pub source_job_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutomationEvaluationPayload {
    pub source_job_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StaleWakeProbeSweepPayload {
    pub source_job_id: String,
    pub max_age_seconds: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StaleRunningJobSweepPayload {
    pub source_job_id: String,
    pub timeout_minutes: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EphemeralJobGcPayload {
    pub source_job_id: String,
    pub batch_limit: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum JobPayload {
    AudioSegment(AudioSegmentPayload),
    WakeActivation(WakeActivationPayload),
    AgentTask(AgentTaskPayload),
    DiscordTextMessage(DiscordTextMessagePayload),
    DiscordSlashCommand(DiscordSlashCommandPayload),
    TextDelivery(TextDeliveryPayload),
    DiscordTextSend(DiscordTextSendPayload),
    DiscordForumThreadCreate(DiscordForumThreadCreatePayload),
    AgentSessionStart(AgentSessionStartPayload),
    TranscriptPublication(TranscriptPublicationPayload),
    RefineTranscript(RefineTranscriptPayload),
    ConfirmationRequired(ConfirmationRequiredPayload),
    Command(CommandPayload),
    RoomAgentPlacement(RoomAgentPlacementPayload),
    DiscordVoiceJoin(DiscordVoiceJoinPayload),
    DiscordVoiceLeave(DiscordVoiceLeavePayload),
    DiscordVoicePlayback(DiscordVoicePlaybackPayload),
    DiscordVoiceMute(DiscordVoiceMutePayload),
    DiscordVoicePlayAudio(DiscordVoicePlayAudioPayload),
    RuntimeControl(RuntimeControlPayload),
    WakeProbe(WakeProbePayload),
    RuntimeMaintenance(RuntimeMaintenancePayload),
    VoiceStatusSync(VoiceStatusSyncPayload),
    DiscordVoiceStatusSnapshot(DiscordVoiceStatusSnapshotPayload),
    AutomationEvaluation(AutomationEvaluationPayload),
    StaleWakeProbeSweep(StaleWakeProbeSweepPayload),
    StaleRunningJobSweep(StaleRunningJobSweepPayload),
    EphemeralJobGc(EphemeralJobGcPayload),
    DiscordVoiceDeafen(DiscordVoiceDeafenPayload),
}

impl JobPayload {
    pub fn kind(&self) -> JobKind {
        match self {
            Self::AudioSegment(_) => JobKind::AudioSegment,
            Self::WakeActivation(_) => JobKind::WakeActivation,
            Self::AgentTask(_) => JobKind::AgentTask,
            Self::DiscordTextMessage(_) => JobKind::DiscordTextMessage,
            Self::DiscordSlashCommand(_) => JobKind::DiscordSlashCommand,
            Self::TextDelivery(_) => JobKind::TextDelivery,
            Self::DiscordTextSend(_) => JobKind::DiscordTextSend,
            Self::DiscordForumThreadCreate(_) => JobKind::DiscordForumThreadCreate,
            Self::AgentSessionStart(_) => JobKind::AgentSessionStart,
            Self::TranscriptPublication(_) => JobKind::TranscriptPublication,
            Self::RefineTranscript(_) => JobKind::RefineTranscript,
            Self::ConfirmationRequired(_) => JobKind::ConfirmationRequired,
            Self::Command(_) => JobKind::Command,
            Self::RoomAgentPlacement(_) => JobKind::RoomAgentPlacement,
            Self::DiscordVoiceJoin(_) => JobKind::DiscordVoiceJoin,
            Self::DiscordVoiceLeave(_) => JobKind::DiscordVoiceLeave,
            Self::DiscordVoicePlayback(_) => JobKind::DiscordVoicePlayback,
            Self::DiscordVoiceMute(_) => JobKind::DiscordVoiceMute,
            Self::DiscordVoicePlayAudio(_) => JobKind::DiscordVoicePlayAudio,
            Self::RuntimeControl(_) => JobKind::RuntimeControl,
            Self::WakeProbe(_) => JobKind::WakeProbe,
            Self::RuntimeMaintenance(_) => JobKind::RuntimeMaintenance,
            Self::VoiceStatusSync(_) => JobKind::VoiceStatusSync,
            Self::DiscordVoiceStatusSnapshot(_) => JobKind::DiscordVoiceStatusSnapshot,
            Self::AutomationEvaluation(_) => JobKind::AutomationEvaluation,
            Self::StaleWakeProbeSweep(_) => JobKind::StaleWakeProbeSweep,
            Self::StaleRunningJobSweep(_) => JobKind::StaleRunningJobSweep,
            Self::EphemeralJobGc(_) => JobKind::EphemeralJobGc,
            Self::DiscordVoiceDeafen(_) => JobKind::DiscordVoiceDeafen,
        }
    }

    pub fn command(&self) -> Option<&CommandRequest> {
        match self {
            Self::AudioSegment(_) => None,
            Self::WakeProbe(_) => None,
            Self::RuntimeMaintenance(_) => None,
            Self::VoiceStatusSync(_) => None,
            Self::DiscordVoiceStatusSnapshot(_) => None,
            Self::AutomationEvaluation(_) => None,
            Self::StaleWakeProbeSweep(_) => None,
            Self::StaleRunningJobSweep(_) => None,
            Self::EphemeralJobGc(_) => None,
            Self::DiscordVoiceDeafen(_) => None,
            Self::WakeActivation(_) => None,
            Self::AgentTask(payload) => Some(&payload.command),
            Self::DiscordTextMessage(_) => None,
            Self::DiscordSlashCommand(_) => None,
            Self::TextDelivery(_) => None,
            Self::DiscordTextSend(_) => None,
            Self::DiscordForumThreadCreate(_) => None,
            Self::AgentSessionStart(payload) => Some(&payload.command),
            Self::TranscriptPublication(_) => None,
            Self::ConfirmationRequired(payload) => Some(&payload.command),
            Self::Command(payload) => Some(&payload.command),
            Self::RoomAgentPlacement(_) => None,
            Self::DiscordVoiceJoin(_) => None,
            Self::DiscordVoiceLeave(_) => None,
            Self::DiscordVoicePlayback(_) => None,
            Self::DiscordVoiceMute(_) => None,
            Self::DiscordVoicePlayAudio(_) => None,
            Self::RuntimeControl(_) => None,
            Self::RefineTranscript(_) => None,
        }
    }

    pub fn command_mut(&mut self) -> Option<&mut CommandRequest> {
        match self {
            Self::AudioSegment(_) => None,
            Self::WakeProbe(_) => None,
            Self::RuntimeMaintenance(_) => None,
            Self::VoiceStatusSync(_) => None,
            Self::DiscordVoiceStatusSnapshot(_) => None,
            Self::AutomationEvaluation(_) => None,
            Self::StaleWakeProbeSweep(_) => None,
            Self::StaleRunningJobSweep(_) => None,
            Self::EphemeralJobGc(_) => None,
            Self::DiscordVoiceDeafen(_) => None,
            Self::WakeActivation(_) => None,
            Self::AgentTask(payload) => Some(&mut payload.command),
            Self::DiscordTextMessage(_) => None,
            Self::DiscordSlashCommand(_) => None,
            Self::TextDelivery(_) => None,
            Self::DiscordTextSend(_) => None,
            Self::DiscordForumThreadCreate(_) => None,
            Self::AgentSessionStart(payload) => Some(&mut payload.command),
            Self::TranscriptPublication(_) => None,
            Self::ConfirmationRequired(payload) => Some(&mut payload.command),
            Self::Command(payload) => Some(&mut payload.command),
            Self::RoomAgentPlacement(_) => None,
            Self::DiscordVoiceJoin(_) => None,
            Self::DiscordVoiceLeave(_) => None,
            Self::DiscordVoicePlayback(_) => None,
            Self::DiscordVoiceMute(_) => None,
            Self::DiscordVoicePlayAudio(_) => None,
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
            Self::WakeActivation(payload) => json!({
                "activation_id": payload.activation_id,
                "guild_id": payload.guild_id,
                "voice_channel_id": payload.voice_channel_id,
                "voice_channel_name": payload.voice_channel_name,
                "speaker_user_id": payload.speaker_user_id,
                "speaker_label": payload.speaker_label,
                "wake_event_id": payload.wake_event_id,
                "wake_started_at": payload.wake_started_at,
                "wake_ended_at": payload.wake_ended_at,
                "latest_wake_event_id": payload.latest_wake_event_id,
                "latest_wake_at": payload.latest_wake_at,
                "lookback_seconds": payload.lookback_seconds,
                "min_post_seconds": payload.min_post_seconds,
                "speaker_idle_seconds": payload.speaker_idle_seconds,
                "stt_flush_grace_seconds": payload.stt_flush_grace_seconds,
                "max_window_seconds": payload.max_window_seconds,
                "additive_preempt_seconds": payload.additive_preempt_seconds,
                "independent_after_seconds": payload.independent_after_seconds,
                "amended_wake_event_ids": payload.amended_wake_event_ids,
                "replacement_of_job_ids": payload.replacement_of_job_ids,
            }),
            Self::AgentTask(payload) => json!({
                "agent_session_id": payload.agent_session_id,
                "command": payload.command.to_json(),
            }),
            Self::DiscordTextMessage(payload) => json!({
                "guild_id": payload.guild_id,
                "channel_id": payload.channel_id,
                "message_id": payload.message_id,
                "author_user_id": payload.author_user_id,
                "author_username": payload.author_username,
                "author_display_name": payload.author_display_name,
                "content": payload.content,
                "created_at": payload.created_at,
                "referenced_message_id": payload.referenced_message_id,
            }),
            Self::DiscordSlashCommand(payload) => json!({
                "interaction_id": payload.interaction_id,
                "interaction_token": payload.interaction_token,
                "application_id": payload.application_id,
                "guild_id": payload.guild_id,
                "channel_id": payload.channel_id,
                "voice_channel_id": payload.voice_channel_id,
                "user_id": payload.user_id,
                "username": payload.username,
                "command_name": payload.command_name,
                "options": payload.options.to_json(),
                "created_at": payload.created_at,
                "response_visibility": payload.response_visibility,
            }),
            Self::TextDelivery(payload) => payload.to_json(),
            Self::DiscordTextSend(payload) => json!({
                "intent": payload.intent.as_str(),
                "target": payload.target.to_json(),
                "content": payload.content,
                "source_job_id": payload.source_job_id,
                "requested_by_user_id": payload.requested_by_user_id,
                "allowed_mentions": payload.allowed_mentions.to_json(),
                "components": payload.components.to_json(),
            }),
            Self::DiscordForumThreadCreate(payload) => json!({
                "parent_channel_id": payload.parent_channel_id,
                "name": payload.name,
                "content": payload.content,
                "auto_archive_minutes": payload.auto_archive_minutes,
                "source_job_id": payload.source_job_id,
            }),
            Self::AgentSessionStart(payload) => json!({
                "agent_session_id": payload.agent_session_id,
                "guild_id": payload.guild_id,
                "voice_channel_id": payload.voice_channel_id,
                "discord_parent_channel_id": payload.discord_parent_channel_id,
                "requested_by_user_id": payload.requested_by_user_id,
                "command": payload.command.to_json(),
            }),
            Self::TranscriptPublication(payload) => json!({
                "publication_id": payload.publication_id,
                "live": payload.live,
                "refined_queued": payload.refined_queued,
            }),
            Self::RefineTranscript(payload) => json!({
                "window_id": payload.window_id,
                "publication_id": payload.publication_id,
            }),
            Self::ConfirmationRequired(payload) => json!({
                "command": payload.command.to_json(),
                "confirmation": payload.confirmation.to_json(),
            }),
            Self::Command(payload) => json!({"command": payload.command.to_json()}),
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
            Self::DiscordVoiceJoin(payload) => json!({
                "room": payload.room.to_json(),
                "bot_id": payload.bot_id,
                "capture_run_id": payload.capture_run_id,
                "assignment_id": payload.assignment_id,
                "started_at": payload.started_at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                "session_dir": payload.session_dir.display().to_string(),
                "requested_by_user_id": payload.requested_by_user_id,
                "reason": payload.reason,
            }),
            Self::DiscordVoiceLeave(payload) => json!({
                "session_id": payload.session_id,
                "reason": payload.reason,
            }),
            Self::DiscordVoicePlayback(payload) => json!({
                "session_id": payload.session_id,
                "cue": payload.cue.as_str(),
                "source_job_id": payload.source_job_id,
                "reason": payload.reason,
            }),
            Self::DiscordVoiceMute(payload) => json!({
                "session_id": payload.session_id,
                "muted": payload.muted,
                "source_job_id": payload.source_job_id,
                "reason": payload.reason,
            }),
            Self::DiscordVoiceDeafen(payload) => json!({
                "session_id": payload.session_id,
                "deafened": payload.deafened,
                "source_job_id": payload.source_job_id,
                "reason": payload.reason,
            }),
            Self::DiscordVoicePlayAudio(payload) => json!({
                "session_id": payload.session_id,
                "cue": payload.cue.as_str(),
                "source_job_id": payload.source_job_id,
                "reason": payload.reason,
            }),
            Self::RuntimeControl(payload) => json!({
                "action": payload.action.as_str(),
                "target_job_id": payload.target_job_id,
                "actor_user_id": payload.actor_user_id,
            }),
            Self::RuntimeMaintenance(payload) => json!({
                "interval_ms": payload.interval_ms,
            }),
            Self::VoiceStatusSync(payload) => json!({
                "source_job_id": payload.source_job_id,
            }),
            Self::DiscordVoiceStatusSnapshot(payload) => json!({
                "source_job_id": payload.source_job_id,
            }),
            Self::AutomationEvaluation(payload) => json!({
                "source_job_id": payload.source_job_id,
            }),
            Self::StaleWakeProbeSweep(payload) => json!({
                "source_job_id": payload.source_job_id,
                "max_age_seconds": payload.max_age_seconds,
            }),
            Self::StaleRunningJobSweep(payload) => json!({
                "source_job_id": payload.source_job_id,
                "timeout_minutes": payload.timeout_minutes,
            }),
            Self::EphemeralJobGc(payload) => json!({
                "source_job_id": payload.source_job_id,
                "batch_limit": payload.batch_limit,
            }),
            Self::WakeProbe(payload) => json!({
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
                "probe_start_time": payload.probe_start_time.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                "probe_end_time": payload.probe_end_time.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                "probe_index": payload.probe_index,
                "duration_ms": payload.duration_ms,
                "source_audio_path": payload.source_audio_path.display().to_string(),
                "audio_checksum": payload.audio_checksum,
                "audio_bytes": payload.audio_bytes,
                "audio_format": payload.audio_format,
                "sample_rate_hz": payload.sample_rate_hz,
                "channels": payload.channels,
                "sample_width_bits": payload.sample_width_bits,
                "post_processing": payload.post_processing,
                "stream_id": payload.stream_id,
                "reset_stream": payload.reset_stream,
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
