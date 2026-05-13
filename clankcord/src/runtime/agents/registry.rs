use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::Result;
use crate::adapters::codex::CodexAdapter;
use crate::runtime::agents::worker::{self, WorkerAgentRequest};
use crate::runtime::jobs::WorkerJobMetadata;
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
    pub(crate) fn dispatch_worker_job(
        &self,
        request: WorkerAgentRequest,
    ) -> Result<WorkerJobMetadata> {
        worker::dispatch_worker_job(self, request)
    }

    pub(crate) fn begin_worker_invocation(
        &self,
        guild_id: &str,
        voice_channel_id: &str,
        job_id: &str,
    ) -> AgentSession {
        self.with_registry(|registry| {
            registry.begin_invocation(
                AgentSessionKey::worker(guild_id, voice_channel_id),
                AgentRole::Worker,
                guild_id,
                voice_channel_id,
                job_id,
            )
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

    pub fn worker_session_key(guild_id: &str, voice_channel_id: &str) -> String {
        AgentSessionKey::worker(guild_id, voice_channel_id).value
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
        key: AgentSessionKey,
        role: AgentRole,
        guild_id: &str,
        voice_channel_id: &str,
        job_id: &str,
    ) -> AgentSession {
        let now = isoformat_z(None);
        let session = self
            .sessions
            .entry(key.value.clone())
            .or_insert_with(|| AgentSession {
                key: key.value.clone(),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AgentSessionStatus {
    #[default]
    Idle,
    Running,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AgentRole {
    Worker,
}

impl AgentRole {
    fn as_str(self) -> &'static str {
        match self {
            Self::Worker => "worker",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AgentSessionKey {
    value: String,
}

impl AgentSessionKey {
    fn worker(guild_id: &str, voice_channel_id: &str) -> Self {
        Self {
            value: format!(
                "worker:{}:{}",
                normalize_key_part(guild_id),
                normalize_key_part(voice_channel_id)
            ),
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
