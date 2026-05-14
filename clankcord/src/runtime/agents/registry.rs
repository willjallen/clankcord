use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use crate::adapters::codex::CodexAdapter;
use crate::runtime::agents::AgentRole;
use crate::runtime::timeline::isoformat_z;

#[derive(Debug, Clone)]
pub struct AgentRuntime {
    registry: Arc<Mutex<AgentRegistry>>,
    codex: CodexAdapter,
}

impl Default for AgentRuntime {
    fn default() -> Self {
        Self {
            registry: Arc::new(Mutex::new(AgentRegistry::default())),
            codex: CodexAdapter::default(),
        }
    }
}

impl AgentRuntime {
    pub(crate) fn begin_invocation(
        &self,
        role: AgentRole,
        key: &str,
        guild_id: &str,
        voice_channel_id: &str,
        job_id: &str,
    ) -> AgentSession {
        self.with_registry(|registry| {
            registry.begin_invocation(key.to_string(), role, guild_id, voice_channel_id, job_id)
        })
    }

    pub(crate) fn complete_invocation(
        &self,
        key: &str,
        session_id: impl Into<String>,
    ) -> AgentSession {
        self.with_registry(|registry| registry.complete_invocation(key, session_id.into()))
    }

    pub(crate) fn fail_invocation(&self, key: &str, error: impl Into<String>) -> AgentSession {
        self.with_registry(|registry| registry.fail_invocation(key, error.into()))
    }

    pub(crate) fn codex(&self) -> &CodexAdapter {
        &self.codex
    }

    pub fn session_snapshot(&self, key: &str) -> Option<AgentSession> {
        self.with_registry(|registry| registry.sessions.get(key).cloned())
    }

    pub fn sessions_snapshot(&self) -> Vec<AgentSession> {
        self.with_registry(|registry| registry.sessions.values().cloned().collect())
    }

    pub fn task_session_key(guild_id: &str, voice_channel_id: &str) -> String {
        format!(
            "task:{}:{}",
            normalize_key_part(guild_id),
            normalize_key_part(voice_channel_id)
        )
    }

    fn with_registry<T>(&self, f: impl FnOnce(&mut AgentRegistry) -> T) -> T {
        let mut registry = self
            .registry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        f(&mut registry)
    }
}

#[derive(Debug, Clone, Default)]
struct AgentRegistry {
    sessions: BTreeMap<String, AgentSession>,
}

impl AgentRegistry {
    fn begin_invocation(
        &mut self,
        key: String,
        role: AgentRole,
        guild_id: &str,
        voice_channel_id: &str,
        job_id: &str,
    ) -> AgentSession {
        let now = isoformat_z(None);
        let session = self
            .sessions
            .entry(key.clone())
            .or_insert_with(|| AgentSession {
                key: key.clone(),
                role: role.as_str().to_string(),
                guild_id: guild_id.to_string(),
                voice_channel_id: voice_channel_id.to_string(),
                session_id: String::new(),
                active_job_id: String::new(),
                status: AgentSessionStatus::Idle,
                invocation_count: 0,
                created_at: now.clone(),
                last_used_at: String::new(),
                last_error: String::new(),
            });
        session.active_job_id = job_id.to_string();
        session.status = AgentSessionStatus::Running;
        session.invocation_count += 1;
        session.last_used_at = now;
        session.last_error.clear();
        session.clone()
    }

    fn complete_invocation(&mut self, key: &str, session_id: String) -> AgentSession {
        let session = self.sessions.entry(key.to_string()).or_default();
        session.key = key.to_string();
        if !session_id.trim().is_empty() {
            session.session_id = session_id;
        }
        session.status = AgentSessionStatus::Idle;
        session.active_job_id.clear();
        session.last_used_at = isoformat_z(None);
        session.clone()
    }

    fn fail_invocation(&mut self, key: &str, error: String) -> AgentSession {
        let session = self.sessions.entry(key.to_string()).or_default();
        session.key = key.to_string();
        session.status = AgentSessionStatus::Failed;
        session.active_job_id.clear();
        session.last_error = error;
        session.last_used_at = isoformat_z(None);
        session.clone()
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
    fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Running => "running",
            Self::Failed => "failed",
        }
    }
}

fn normalize_key_part(value: &str) -> String {
    let normalized = value
        .trim()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    if normalized.is_empty() {
        "unknown".to_string()
    } else {
        normalized
    }
}
