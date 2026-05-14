use std::collections::BTreeMap;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::Result;

use super::output::{extract_codex_model, extract_codex_session_id};

#[derive(Debug, Clone)]
pub(crate) struct CodexAdapter {
    executable: String,
}

impl Default for CodexAdapter {
    fn default() -> Self {
        Self {
            executable: env::var("CLANKCORD_CODEX_BIN")
                .or_else(|_| env::var("CODEX_BIN"))
                .unwrap_or_else(|_| "codex".to_string()),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct CodexRunRequest {
    pub prompt: String,
    pub session_id: Option<String>,
    pub cwd: Option<PathBuf>,
    pub model: Option<String>,
    pub timeout: Duration,
    pub env: BTreeMap<String, String>,
    pub output_last_message_path: PathBuf,
}

#[derive(Debug, Clone)]
pub(crate) struct CodexRunResult {
    pub stdout: String,
    pub stderr: String,
    pub returncode: Option<i32>,
    pub success: bool,
    pub timed_out: bool,
    pub session_id: String,
    pub model: String,
    pub final_message: String,
    pub command_display: String,
}

impl CodexAdapter {
    pub(crate) fn run(&self, request: CodexRunRequest) -> Result<CodexRunResult> {
        if let Some(parent) = request.output_last_message_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&request.output_last_message_path, "")?;

        let args = codex_args(&request);
        let command_display = display_command(&self.executable, &args);
        let stdout_file = tempfile::NamedTempFile::new()?;
        let stderr_file = tempfile::NamedTempFile::new()?;
        let mut command = Command::new(&self.executable);
        command
            .args(&args)
            .envs(&request.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::from(stdout_file.reopen()?))
            .stderr(Stdio::from(stderr_file.reopen()?));

        let mut child = command.spawn()?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(request.prompt.as_bytes())?;
        }

        let deadline = Instant::now() + request.timeout;
        let mut timed_out = false;
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if Instant::now() >= deadline {
                timed_out = true;
                let _ = child.kill();
                break child.wait()?;
            }
            thread::sleep(Duration::from_millis(100));
        };

        let stdout = fs::read_to_string(stdout_file.path()).unwrap_or_default();
        let mut stderr = fs::read_to_string(stderr_file.path()).unwrap_or_default();
        if timed_out {
            if !stderr.trim().is_empty() {
                stderr.push('\n');
            }
            stderr.push_str(&format!(
                "codex command timed out after {} seconds",
                request.timeout.as_secs()
            ));
        }
        let final_message =
            fs::read_to_string(&request.output_last_message_path).unwrap_or_default();
        let session_id = non_empty(
            extract_codex_session_id(&stdout),
            request.session_id.unwrap_or_default(),
        );
        let model = non_empty(
            extract_codex_model(&stdout),
            request.model.unwrap_or_else(|| "codex-default".to_string()),
        );

        Ok(CodexRunResult {
            stdout,
            stderr,
            returncode: status.code(),
            success: status.success() && !timed_out,
            timed_out,
            session_id,
            model,
            final_message,
            command_display,
        })
    }
}

fn codex_args(request: &CodexRunRequest) -> Vec<OsString> {
    let mut args = Vec::new();
    if let Some(cwd) = &request.cwd {
        args.push(OsString::from("-C"));
        args.push(cwd.as_os_str().to_os_string());
    }
    if let Some(model) = request
        .model
        .as_ref()
        .filter(|model| !model.trim().is_empty())
    {
        args.push(OsString::from("-m"));
        args.push(OsString::from(model));
    }
    if let Some(sandbox) = env_value("CLANKCORD_CODEX_SANDBOX") {
        args.push(OsString::from("-s"));
        args.push(OsString::from(sandbox));
    }
    if truthy_env("CLANKCORD_CODEX_BYPASS_SANDBOX") {
        args.push(OsString::from("--dangerously-bypass-approvals-and-sandbox"));
    } else {
        args.push(OsString::from("-a"));
        args.push(OsString::from(
            env_value("CLANKCORD_CODEX_APPROVAL_POLICY").unwrap_or_else(|| "never".to_string()),
        ));
    }

    args.push(OsString::from("exec"));
    if let Some(session_id) = request
        .session_id
        .as_ref()
        .map(|session_id| session_id.trim())
        .filter(|session_id| !session_id.is_empty())
    {
        args.push(OsString::from("resume"));
        args.push(OsString::from("--json"));
        args.push(OsString::from("--output-last-message"));
        args.push(request.output_last_message_path.as_os_str().to_os_string());
        args.push(OsString::from(session_id));
        args.push(OsString::from("-"));
    } else {
        args.push(OsString::from("--json"));
        args.push(OsString::from("--output-last-message"));
        args.push(request.output_last_message_path.as_os_str().to_os_string());
        args.push(OsString::from("-"));
    }
    args
}

fn display_command(executable: &str, args: &[OsString]) -> String {
    let mut parts = vec![executable.to_string()];
    parts.extend(args.iter().map(|arg| arg.to_string_lossy().to_string()));
    parts.join(" ")
}

fn env_value(key: &str) -> Option<String> {
    env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn truthy_env(key: &str) -> bool {
    env::var(key)
        .ok()
        .map(|value| matches!(value.trim(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(false)
}

fn non_empty(primary: String, fallback: String) -> String {
    if primary.trim().is_empty() {
        fallback
    } else {
        primary
    }
}
