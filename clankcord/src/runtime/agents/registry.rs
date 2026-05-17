use std::str::FromStr;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::Result;
use crate::adapters::codex::CodexAdapter;
use crate::runtime::agents::AgentRole;
use crate::runtime::jobs::{TextTarget, TextTargetKind};
use crate::runtime::timeline::isoformat_z;

const AGENT_SESSION_PAYLOAD_BLOB_MAGIC: &[u8; 8] = b"CLANKAGS";
const AGENT_SESSION_PAYLOAD_BLOB_VERSION: u16 = 1;
const AGENT_SESSION_PAYLOAD_BLOB_HEADER_LEN: usize =
    AGENT_SESSION_PAYLOAD_BLOB_MAGIC.len() + std::mem::size_of::<u16>();

#[derive(Debug, Clone, Default)]
pub struct AgentRuntime {
    codex: CodexAdapter,
}

impl AgentRuntime {
    pub(crate) fn codex(&self) -> &CodexAdapter {
        &self.codex
    }

    pub fn task_session_key(guild_id: &str, voice_channel_id: &str) -> String {
        format!(
            "task:{}:{}",
            normalize_key_part(guild_id),
            normalize_key_part(voice_channel_id)
        )
    }

    pub fn agent_session_key(agent_session_id: &str) -> String {
        format!("agent:session:{}", normalize_key_part(agent_session_id))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentSessionRouteKind {
    Voice,
    Dm,
    Thread,
}

impl AgentSessionRouteKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Voice => "voice",
            Self::Dm => "dm",
            Self::Thread => "thread",
        }
    }
}

impl FromStr for AgentSessionRouteKind {
    type Err = anyhow::Error;

    fn from_str(raw: &str) -> Result<Self> {
        match raw.trim() {
            "voice" => Ok(Self::Voice),
            "dm" => Ok(Self::Dm),
            "thread" => Ok(Self::Thread),
            value => anyhow::bail!("unknown agent session route kind: {value}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentSessionRecordState {
    Starting,
    Active,
    Retired,
    Failed,
}

impl AgentSessionRecordState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Active => "active",
            Self::Retired => "retired",
            Self::Failed => "failed",
        }
    }

    pub fn is_selectable(self) -> bool {
        matches!(self, Self::Active)
    }
}

impl FromStr for AgentSessionRecordState {
    type Err = anyhow::Error;

    fn from_str(raw: &str) -> Result<Self> {
        match raw.trim() {
            "starting" => Ok(Self::Starting),
            "active" => Ok(Self::Active),
            "retired" => Ok(Self::Retired),
            "failed" => Ok(Self::Failed),
            value => anyhow::bail!("unknown agent session state: {value}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSessionRecord {
    pub agent_session_id: String,
    pub codex_session_id: String,
    pub route_kind: AgentSessionRouteKind,
    pub route_key: String,
    pub guild_id: String,
    pub scope_id: String,
    pub dm_user_id: String,
    pub voice_capture_session_id: String,
    pub discord_thread_id: String,
    pub discord_parent_channel_id: String,
    pub text_target: TextTarget,
    pub state: AgentSessionRecordState,
    pub created_at: String,
    pub last_activity_at: String,
    pub max_active_until: String,
    pub retired_at: String,
    pub retirement_reason: String,
    pub retired_by_user_id: String,
    pub resumed_from_agent_session_id: String,
}

impl AgentSessionRecord {
    pub fn new_voice(
        agent_session_id: impl Into<String>,
        guild_id: impl Into<String>,
        voice_channel_id: impl Into<String>,
        discord_parent_channel_id: impl Into<String>,
        discord_thread_id: impl Into<String>,
        created_at: impl Into<String>,
        max_active_until: impl Into<String>,
    ) -> Self {
        let agent_session_id = agent_session_id.into();
        let guild_id = guild_id.into();
        let voice_channel_id = voice_channel_id.into();
        let discord_parent_channel_id = discord_parent_channel_id.into();
        let discord_thread_id = discord_thread_id.into();
        let created_at = created_at.into();
        Self {
            agent_session_id,
            codex_session_id: String::new(),
            route_kind: AgentSessionRouteKind::Voice,
            route_key: voice_route_key(&guild_id, &voice_channel_id),
            guild_id,
            scope_id: voice_channel_id,
            dm_user_id: String::new(),
            voice_capture_session_id: String::new(),
            text_target: TextTarget {
                kind: TextTargetKind::Channel,
                channel_id: discord_thread_id.clone(),
                user_id: String::new(),
            },
            discord_thread_id,
            discord_parent_channel_id,
            state: AgentSessionRecordState::Active,
            last_activity_at: created_at.clone(),
            created_at,
            max_active_until: max_active_until.into(),
            retired_at: String::new(),
            retirement_reason: String::new(),
            retired_by_user_id: String::new(),
            resumed_from_agent_session_id: String::new(),
        }
    }

    pub fn new_voice_starting(
        agent_session_id: impl Into<String>,
        guild_id: impl Into<String>,
        voice_channel_id: impl Into<String>,
        discord_parent_channel_id: impl Into<String>,
        created_at: impl Into<String>,
        max_active_until: impl Into<String>,
    ) -> Self {
        let agent_session_id = agent_session_id.into();
        let guild_id = guild_id.into();
        let voice_channel_id = voice_channel_id.into();
        let created_at = created_at.into();
        Self {
            agent_session_id,
            codex_session_id: String::new(),
            route_kind: AgentSessionRouteKind::Voice,
            route_key: voice_route_key(&guild_id, &voice_channel_id),
            guild_id,
            scope_id: voice_channel_id,
            dm_user_id: String::new(),
            voice_capture_session_id: String::new(),
            discord_thread_id: String::new(),
            discord_parent_channel_id: discord_parent_channel_id.into(),
            text_target: TextTarget {
                kind: TextTargetKind::Channel,
                channel_id: String::new(),
                user_id: String::new(),
            },
            state: AgentSessionRecordState::Starting,
            last_activity_at: created_at.clone(),
            created_at,
            max_active_until: max_active_until.into(),
            retired_at: String::new(),
            retirement_reason: String::new(),
            retired_by_user_id: String::new(),
            resumed_from_agent_session_id: String::new(),
        }
    }

    pub fn new_dm(
        agent_session_id: impl Into<String>,
        user_id: impl Into<String>,
        created_at: impl Into<String>,
        max_active_until: impl Into<String>,
    ) -> Self {
        let agent_session_id = agent_session_id.into();
        let user_id = user_id.into();
        let created_at = created_at.into();
        Self {
            agent_session_id,
            codex_session_id: String::new(),
            route_kind: AgentSessionRouteKind::Dm,
            route_key: dm_route_key(&user_id),
            guild_id: String::new(),
            scope_id: user_id.clone(),
            dm_user_id: user_id.clone(),
            voice_capture_session_id: String::new(),
            discord_thread_id: String::new(),
            discord_parent_channel_id: String::new(),
            text_target: TextTarget {
                kind: TextTargetKind::Dm,
                channel_id: String::new(),
                user_id,
            },
            state: AgentSessionRecordState::Active,
            last_activity_at: created_at.clone(),
            created_at,
            max_active_until: max_active_until.into(),
            retired_at: String::new(),
            retirement_reason: String::new(),
            retired_by_user_id: String::new(),
            resumed_from_agent_session_id: String::new(),
        }
    }

    pub fn invocation_key(&self) -> String {
        AgentRuntime::agent_session_key(&self.agent_session_id)
    }

    pub fn thread_route_key(&self) -> String {
        thread_route_key(&self.guild_id, &self.discord_thread_id)
    }

    pub fn to_json(&self) -> Value {
        let mut object = Map::new();
        object.insert(
            "agent_session_id".to_string(),
            Value::String(self.agent_session_id.clone()),
        );
        object.insert(
            "codex_session_id".to_string(),
            Value::String(self.codex_session_id.clone()),
        );
        object.insert(
            "route_kind".to_string(),
            Value::String(self.route_kind.as_str().to_string()),
        );
        object.insert(
            "route_key".to_string(),
            Value::String(self.route_key.clone()),
        );
        if !self.guild_id.trim().is_empty() {
            object.insert("guild_id".to_string(), Value::String(self.guild_id.clone()));
        }
        object.insert("scope_id".to_string(), Value::String(self.scope_id.clone()));
        if self.route_kind == AgentSessionRouteKind::Voice {
            object.insert(
                "voice_channel_id".to_string(),
                Value::String(self.scope_id.clone()),
            );
        }
        object.insert(
            "dm_user_id".to_string(),
            Value::String(self.dm_user_id.clone()),
        );
        object.insert(
            "voice_capture_session_id".to_string(),
            Value::String(self.voice_capture_session_id.clone()),
        );
        object.insert(
            "discord_thread_id".to_string(),
            Value::String(self.discord_thread_id.clone()),
        );
        object.insert(
            "discord_parent_channel_id".to_string(),
            Value::String(self.discord_parent_channel_id.clone()),
        );
        object.insert("text_target".to_string(), self.text_target.to_json());
        object.insert(
            "state".to_string(),
            Value::String(self.state.as_str().to_string()),
        );
        object.insert(
            "created_at".to_string(),
            Value::String(self.created_at.clone()),
        );
        object.insert(
            "last_activity_at".to_string(),
            Value::String(self.last_activity_at.clone()),
        );
        object.insert(
            "max_active_until".to_string(),
            Value::String(self.max_active_until.clone()),
        );
        object.insert(
            "retired_at".to_string(),
            Value::String(self.retired_at.clone()),
        );
        object.insert(
            "retirement_reason".to_string(),
            Value::String(self.retirement_reason.clone()),
        );
        object.insert(
            "retired_by_user_id".to_string(),
            Value::String(self.retired_by_user_id.clone()),
        );
        object.insert(
            "resumed_from_agent_session_id".to_string(),
            Value::String(self.resumed_from_agent_session_id.clone()),
        );
        Value::Object(object)
    }

    pub(crate) fn encode(&self) -> Result<Vec<u8>> {
        let body = bincode::serialize(self)?;
        let mut blob = Vec::with_capacity(AGENT_SESSION_PAYLOAD_BLOB_HEADER_LEN + body.len());
        blob.extend_from_slice(AGENT_SESSION_PAYLOAD_BLOB_MAGIC);
        blob.extend_from_slice(&AGENT_SESSION_PAYLOAD_BLOB_VERSION.to_le_bytes());
        blob.extend_from_slice(&body);
        Ok(blob)
    }

    pub(crate) fn decode(payload: &[u8]) -> Result<Self> {
        if !Self::is_current_payload_blob(payload) {
            anyhow::bail!("agent session payload has invalid blob envelope");
        }
        Ok(bincode::deserialize(
            &payload[AGENT_SESSION_PAYLOAD_BLOB_HEADER_LEN..],
        )?)
    }

    pub(crate) fn is_current_payload_blob(payload: &[u8]) -> bool {
        payload.len() >= AGENT_SESSION_PAYLOAD_BLOB_HEADER_LEN
            && &payload[..AGENT_SESSION_PAYLOAD_BLOB_MAGIC.len()]
                == AGENT_SESSION_PAYLOAD_BLOB_MAGIC
            && u16::from_le_bytes([
                payload[AGENT_SESSION_PAYLOAD_BLOB_MAGIC.len()],
                payload[AGENT_SESSION_PAYLOAD_BLOB_MAGIC.len() + 1],
            ]) == AGENT_SESSION_PAYLOAD_BLOB_VERSION
    }
}

pub fn voice_route_key(guild_id: &str, voice_channel_id: &str) -> String {
    format!(
        "voice:{}:{}",
        normalize_key_part(guild_id),
        normalize_key_part(voice_channel_id)
    )
}

pub fn dm_route_key(user_id: &str) -> String {
    format!("dm:{}", normalize_key_part(user_id))
}

pub fn thread_route_key(guild_id: &str, thread_id: &str) -> String {
    format!(
        "thread:{}:{}",
        normalize_key_part(guild_id),
        normalize_key_part(thread_id)
    )
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AgentSession {
    pub key: String,
    pub role: String,
    pub guild_id: String,
    pub scope_id: String,
    pub session_id: String,
    pub active_job_id: String,
    pub latest_job_id: String,
    pub status: AgentSessionStatus,
    pub invocation_count: u64,
    pub created_at: String,
    pub last_used_at: String,
    pub last_error: String,
}

impl AgentSession {
    pub(crate) fn running(
        role: AgentRole,
        key: &str,
        guild_id: &str,
        scope_id: &str,
        job_id: &str,
        prior_session_id: impl Into<String>,
    ) -> Self {
        let now = isoformat_z(None);
        Self {
            key: key.to_string(),
            role: role.as_str().to_string(),
            guild_id: guild_id.to_string(),
            scope_id: scope_id.to_string(),
            session_id: prior_session_id.into(),
            active_job_id: job_id.to_string(),
            latest_job_id: job_id.to_string(),
            status: AgentSessionStatus::Running,
            invocation_count: 1,
            created_at: now.clone(),
            last_used_at: now,
            last_error: String::new(),
        }
    }

    pub(crate) fn complete(mut self, session_id: String) -> Self {
        if !session_id.trim().is_empty() {
            self.session_id = session_id;
        }
        self.status = AgentSessionStatus::Idle;
        self.active_job_id.clear();
        self.last_used_at = isoformat_z(None);
        self
    }

    pub(crate) fn fail(mut self, error: String) -> Self {
        self.status = AgentSessionStatus::Failed;
        self.active_job_id.clear();
        self.last_error = error;
        self.last_used_at = isoformat_z(None);
        self
    }

    pub fn to_json(&self) -> Value {
        json!({
            "key": self.key,
            "role": self.role,
            "guild_id": self.guild_id,
            "scope_id": self.scope_id,
            "session_id": self.session_id,
            "active_job_id": self.active_job_id,
            "latest_job_id": self.latest_job_id,
            "status": self.status.as_str(),
            "invocation_count": self.invocation_count,
            "created_at": self.created_at,
            "last_used_at": self.last_used_at,
            "last_error": self.last_error,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AgentSessionStatus {
    #[default]
    Idle,
    Running,
    Failed,
}

impl AgentSessionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Running => "running",
            Self::Failed => "failed",
        }
    }
}

fn normalize_key_part(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
        .collect::<String>()
}
