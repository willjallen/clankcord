use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use crate::Result;
use crate::adapters::codex::{CodexRunRequest, CodexRunResult};
use crate::runtime::agents::{AgentInfrastructureError, AgentRuntime, AgentSession};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgentRole {
    Task,
}

impl AgentRole {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Task => "task",
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct AgentInvocationRequest {
    pub role: AgentRole,
    pub session_key: String,
    pub job_id: String,
    pub guild_id: String,
    pub voice_channel_id: String,
    pub prompt: String,
    pub cwd: Option<PathBuf>,
    pub model: Option<String>,
    pub timeout: Duration,
    pub env: BTreeMap<String, String>,
    pub result_path: PathBuf,
    pub raw_result_path: PathBuf,
}

#[derive(Debug, Clone)]
pub(crate) struct AgentInvocationResult {
    pub stdout: String,
    pub stderr: String,
    pub returncode: Option<i32>,
    pub success: bool,
    pub timed_out: bool,
    pub session_id: String,
    pub model: String,
    pub final_message: String,
    pub command_display: String,
    pub session: Option<AgentSession>,
}

impl AgentRuntime {
    pub(crate) fn invoke(&self, request: AgentInvocationRequest) -> Result<AgentInvocationResult> {
        if let Some(parent) = request.raw_result_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let started_session = Some(self.begin_invocation(
            request.role,
            &request.session_key,
            &request.guild_id,
            &request.voice_channel_id,
            &request.job_id,
        ));
        let codex_result = match self.codex().run(CodexRunRequest {
            prompt: request.prompt,
            session_id: started_session
                .as_ref()
                .map(|session| session.session_id.clone())
                .filter(|session_id| !session_id.trim().is_empty()),
            cwd: request.cwd,
            model: request.model,
            timeout: request.timeout,
            env: request.env,
            output_last_message_path: request.result_path,
            stdout_path: Some(request.raw_result_path),
        }) {
            Ok(result) => result,
            Err(error) => {
                if let Some(session) = started_session.as_ref() {
                    self.fail_invocation(&session.key, error.to_string());
                }
                return Err(AgentInfrastructureError::new(format!(
                    "codex {} invocation failed: {error}",
                    request.role.as_str()
                ))
                .into());
            }
        };
        let completed_session =
            complete_session(self, started_session, &codex_result, request.role.as_str());
        Ok(AgentInvocationResult {
            stdout: codex_result.stdout,
            stderr: codex_result.stderr,
            returncode: codex_result.returncode,
            success: codex_result.success,
            timed_out: codex_result.timed_out,
            session_id: codex_result.session_id,
            model: codex_result.model,
            final_message: codex_result.final_message,
            command_display: codex_result.command_display,
            session: completed_session,
        })
    }
}

fn complete_session(
    agents: &AgentRuntime,
    started_session: Option<AgentSession>,
    result: &CodexRunResult,
    role: &str,
) -> Option<AgentSession> {
    started_session.map(|session| {
        if result.success {
            agents.complete_invocation(&session.key, result.session_id.clone())
        } else {
            agents.fail_invocation(
                &session.key,
                if result.stderr.trim().is_empty() {
                    format!("{role} invocation failed")
                } else {
                    result.stderr.clone()
                },
            )
        }
    })
}
