use serde_json::{Value, json};

use crate::adapters::codex::CodexAdapter;
use crate::runtime::agents::AgentRole;
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
