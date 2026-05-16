use std::str::FromStr;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::Result;
use crate::adapters::codex::CodexAdapter;
use crate::runtime::agents::AgentRole;
use crate::runtime::jobs::{TextTarget, TextTargetKind};
use crate::runtime::timeline::isoformat_z;

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
    Expired,
    Failed,
}

impl AgentSessionRecordState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Active => "active",
            Self::Expired => "expired",
            Self::Failed => "failed",
        }
    }

    pub fn is_selectable(self) -> bool {
        matches!(self, Self::Starting | Self::Active)
    }
}

impl FromStr for AgentSessionRecordState {
    type Err = anyhow::Error;

    fn from_str(raw: &str) -> Result<Self> {
        match raw.trim() {
            "starting" => Ok(Self::Starting),
            "active" => Ok(Self::Active),
            "expired" => Ok(Self::Expired),
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
    pub voice_channel_id: String,
    pub dm_user_id: String,
    pub discord_thread_id: String,
    pub discord_parent_channel_id: String,
    pub text_target: TextTarget,
    pub state: AgentSessionRecordState,
    pub created_at: String,
    pub last_activity_at: String,
    pub expires_at: String,
}

impl AgentSessionRecord {
    pub fn new_voice(
        agent_session_id: impl Into<String>,
        guild_id: impl Into<String>,
        voice_channel_id: impl Into<String>,
        discord_parent_channel_id: impl Into<String>,
        discord_thread_id: impl Into<String>,
        created_at: impl Into<String>,
        expires_at: impl Into<String>,
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
            voice_channel_id,
            dm_user_id: String::new(),
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
            expires_at: expires_at.into(),
        }
    }

    pub fn new_voice_starting(
        agent_session_id: impl Into<String>,
        guild_id: impl Into<String>,
        voice_channel_id: impl Into<String>,
        discord_parent_channel_id: impl Into<String>,
        created_at: impl Into<String>,
        expires_at: impl Into<String>,
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
            voice_channel_id,
            dm_user_id: String::new(),
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
            expires_at: expires_at.into(),
        }
    }

    pub fn new_dm(
        agent_session_id: impl Into<String>,
        user_id: impl Into<String>,
        created_at: impl Into<String>,
        expires_at: impl Into<String>,
    ) -> Self {
        let agent_session_id = agent_session_id.into();
        let user_id = user_id.into();
        let created_at = created_at.into();
        Self {
            agent_session_id,
            codex_session_id: String::new(),
            route_kind: AgentSessionRouteKind::Dm,
            route_key: dm_route_key(&user_id),
            guild_id: "dm".to_string(),
            voice_channel_id: user_id.clone(),
            dm_user_id: user_id.clone(),
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
            expires_at: expires_at.into(),
        }
    }

    pub fn invocation_key(&self) -> String {
        AgentRuntime::agent_session_key(&self.agent_session_id)
    }

    pub fn thread_route_key(&self) -> String {
        thread_route_key(&self.guild_id, &self.discord_thread_id)
    }

    pub fn to_json(&self) -> Value {
        json!({
            "agent_session_id": self.agent_session_id,
            "codex_session_id": self.codex_session_id,
            "route_kind": self.route_kind.as_str(),
            "route_key": self.route_key,
            "guild_id": self.guild_id,
            "voice_channel_id": self.voice_channel_id,
            "dm_user_id": self.dm_user_id,
            "discord_thread_id": self.discord_thread_id,
            "discord_parent_channel_id": self.discord_parent_channel_id,
            "text_target": self.text_target.to_json(),
            "state": self.state.as_str(),
            "created_at": self.created_at,
            "last_activity_at": self.last_activity_at,
            "expires_at": self.expires_at,
        })
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
    pub voice_channel_id: String,
    pub session_id: String,
    pub active_job_id: String,
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
        voice_channel_id: &str,
        job_id: &str,
        prior_session_id: impl Into<String>,
    ) -> Self {
        let now = isoformat_z(None);
        Self {
            key: key.to_string(),
            role: role.as_str().to_string(),
            guild_id: guild_id.to_string(),
            voice_channel_id: voice_channel_id.to_string(),
            session_id: prior_session_id.into(),
            active_job_id: job_id.to_string(),
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
            "voice_channel_id": self.voice_channel_id,
            "session_id": self.session_id,
            "active_job_id": self.active_job_id,
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
