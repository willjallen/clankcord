//! CLI copy and usage: help text, spec descriptions, and transcript render output.

use crate::support::cli::{clankcord, stderr, stdout};
use std::fs;
use std::io::Read;
use std::io::Write;
use std::net::TcpListener;
use std::process::Command;
use std::thread;
use std::time::Duration;
use std::time::Instant;

#[test]
fn agent_sessions_help_exposes_lifecycle_commands() {
    let output = clankcord(&["agent-sessions", "--help"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let help = stdout(&output);
    assert!(help.contains("current"));
    assert!(help.contains("search"));
    assert!(help.contains("sunset"));
    assert!(help.contains("resume"));
}

#[test]
fn coding_spec_describes_single_file_c_and_zip_submission() {
    let output = clankcord(&["coding", "spec"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let spec = stdout(&output);
    assert!(spec.contains("single-file C program"));
    assert!(spec.contains("Compiler Explorer"));
    assert!(spec.contains("CLANKCORD_OBSERVE"));
    assert!(spec.contains("volatile"));
    assert!(spec.contains("zip -r artifact.zip"));
    assert!(spec.contains("clankcord responses send --attachment artifact.zip"));
}

#[test]
fn feedback_help_uses_stdin_or_file_body_transport() {
    let output = clankcord(&["feedback", "submit", "--help"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let help = stdout(&output);
    assert!(help.contains("Record feedback text in the current room timeline"));
    assert!(help.contains("--file <PATH>"));
    assert!(!help.contains("--content"));
    assert!(!help.contains("--stdin"));
}

#[test]
fn transcript_render_markdown_writes_content_file() {
    let tempdir = tempfile::tempdir().expect("create tempdir");
    let body = serde_json::json!({
        "window": {
            "start_time": "2026-06-02T00:00:00Z",
            "end_time": "2026-06-02T01:00:00Z"
        },
        "content": "# Transcript\n\nparticipants:\n- user-a: Will\n\n## Conversation\n\n[2026-06-02T00:00:00Z] Will: hello",
        "events": [
            {"event_id": "evt_1", "speaker_user_id": "user-a"}
        ]
    })
    .to_string();
    let (base_url, server) = serve_once(body);
    write_config(tempdir.path(), &base_url);

    let transcript_path = tempdir.path().join("transcript.md");
    let output = Command::new(env!("CARGO_BIN_EXE_clankcord"))
        .current_dir(tempdir.path())
        .args([
            "transcripts",
            "render",
            "--since=-1h",
            "--format",
            "markdown",
            "--file",
        ])
        .arg(&transcript_path)
        .output()
        .expect("clankcord binary runs");

    assert!(output.status.success(), "{}", stderr(&output));
    let request = server.join().expect("server thread joins");
    assert!(request.contains("GET /v1/transcript/render?"));
    assert!(request.contains("format=markdown"));
    assert_eq!(
        fs::read_to_string(&transcript_path).expect("transcript file exists"),
        "# Transcript\n\nparticipants:\n- user-a: Will\n\n## Conversation\n\n[2026-06-02T00:00:00Z] Will: hello\n"
    );
    let stdout = stdout(&output);
    assert!(stdout.contains("Wrote markdown to"));
    assert!(stdout.contains("Records: 1"));
    assert!(stdout.contains("Window: 2026-06-02T00:00:00Z to 2026-06-02T01:00:00Z"));
}

#[test]
fn transcript_render_rejects_unknown_format_before_runtime_request() {
    let output = Command::new(env!("CARGO_BIN_EXE_clankcord"))
        .args(["transcripts", "render", "--format", "yaml"])
        .output()
        .expect("clankcord binary runs");

    assert!(!output.status.success());
    assert!(stderr(&output).contains("--format must be json or markdown for transcript render"));
}

fn serve_once(body: String) -> (String, thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
    listener
        .set_nonblocking(true)
        .expect("set listener nonblocking");
    let addr = listener.local_addr().expect("local addr");
    let base_url = format!("http://{addr}");
    let handle = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        let (mut stream, _) = loop {
            match listener.accept() {
                Ok(connection) => break connection,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        Instant::now() < deadline,
                        "test server did not receive a request"
                    );
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("accept test request: {error}"),
            }
        };
        // The listener is nonblocking so accept can poll against the deadline,
        // but macOS accepted sockets inherit that flag; reads must block.
        stream.set_nonblocking(false).expect("set stream blocking");
        let mut request = Vec::new();
        let mut buf = [0_u8; 1024];
        loop {
            let read = stream.read(&mut buf).expect("read request");
            if read == 0 {
                break;
            }
            request.extend_from_slice(&buf[..read]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        stream
            .write_all(response.as_bytes())
            .expect("write response");
        String::from_utf8_lossy(&request).to_string()
    });
    (base_url, handle)
}

fn write_config(dir: &std::path::Path, base_url: &str) {
    let config = include_str!("../../../config.ex.toml").replace(
        "base_url = \"http://127.0.0.1:8091\"",
        &format!("base_url = \"{base_url}\""),
    );
    fs::write(dir.join("config.toml"), config).expect("write config");
}
