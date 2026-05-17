use std::process::Command;

fn clankcord(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_clankcord"))
        .args(args)
        .output()
        .expect("clankcord binary runs")
}

fn stdout(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn stderr(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).to_string()
}

#[test]
fn top_level_help_describes_command_groups_and_agent_workflows() {
    let output = clankcord(&["--help"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let help = stdout(&output);
    assert!(help.contains("Inspect raw timeline events"));
    assert!(help.contains("Materialize, render, and search voice transcripts"));
    assert!(help.contains("Publish public replies, questions, and DMs"));
    assert!(help.contains("Common agent workflows"));
    assert!(help.contains("clankcord responses send <<'EOF'"));
    assert!(help.contains("clankcord automations validate < automation.json"));
    assert!(help.contains("clankcord feedback submit <<'EOF'"));
}

#[test]
fn response_help_requires_stdin_or_file_body_transport() {
    let output = clankcord(&["responses", "send", "--help"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let help = stdout(&output);
    assert!(help.contains("Read Markdown/plain text from stdin by default"));
    assert!(help.contains("single-quoted heredoc"));
    assert!(help.contains("--file <PATH>"));
    assert!(!help.contains("--content"));
    assert!(!help.contains("--stdin"));
}

#[test]
fn response_content_flag_is_rejected_before_runtime_submission() {
    let output = clankcord(&["responses", "send", "--content", "bad"]);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("unexpected argument '--content'"));
}

#[test]
fn automation_help_uses_stdin_or_file_json_transport() {
    let output = clankcord(&["automations", "create", "--help"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let help = stdout(&output);
    assert!(help.contains("Read JSON from stdin by default"));
    assert!(help.contains("clankcord automations validate < automation.json"));
    assert!(help.contains("--file <PATH>"));
    assert!(!help.contains("--content"));
    assert!(!help.contains("--stdin"));
}

#[test]
fn automation_stdin_flag_is_rejected_before_runtime_submission() {
    let output = clankcord(&["automations", "create", "--stdin"]);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("unexpected argument '--stdin'"));
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
