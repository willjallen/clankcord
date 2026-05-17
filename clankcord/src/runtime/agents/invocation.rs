use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use crate::Result;
use crate::adapters::codex::{CodexRunRequest, CodexRunResult};
use crate::runtime::agents::{AgentInfrastructureError, AgentRuntime, AgentSession};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgentRole {
    Task,
    ThreadTitle,
}

impl AgentRole {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Task => "task",
            Self::ThreadTitle => "thread_title",
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
    pub prior_session_id: String,
    pub prompt: String,
    pub cwd: Option<PathBuf>,
    pub model: Option<String>,
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
        let started_session = AgentSession::running(
            request.role,
            &request.session_key,
            &request.guild_id,
            &request.voice_channel_id,
            &request.job_id,
            request.prior_session_id.clone(),
        );
        let codex_result = match self.codex().run(CodexRunRequest {
            prompt: request.prompt,
            session_id: if request.prior_session_id.trim().is_empty() {
                None
            } else {
                Some(request.prior_session_id)
            },
            cwd: request.cwd,
            model: request.model,
            env: request.env,
            output_last_message_path: request.result_path,
            stdout_path: Some(request.raw_result_path),
        }) {
            Ok(result) => result,
            Err(error) => {
                return Err(AgentInfrastructureError::new(format!(
                    "codex {} invocation failed: {error}",
                    request.role.as_str()
                ))
                .into());
            }
        };
        let completed_session =
            complete_session(started_session, &codex_result, request.role.as_str());
        Ok(AgentInvocationResult {
            stdout: codex_result.stdout,
            stderr: codex_result.stderr,
            returncode: codex_result.returncode,
            success: codex_result.success,
            session_id: codex_result.session_id,
            model: codex_result.model,
            final_message: codex_result.final_message,
            command_display: codex_result.command_display,
            session: Some(completed_session),
        })
    }
}

fn complete_session(
    started_session: AgentSession,
    result: &CodexRunResult,
    role: &str,
) -> AgentSession {
    if result.success {
        started_session.complete(result.session_id.clone())
    } else {
        started_session.fail(if result.stderr.trim().is_empty() {
            format!("{role} invocation failed")
        } else {
            result.stderr.clone()
        })
    }
}
