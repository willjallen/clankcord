//! Pins for CLI input hardening: body content arrives via stdin or file
//! only, and room mutations require explicit targets (123e918, 0a27e72).

use crate::support::cli::{clankcord, stderr, stdout};
use crate::support::initialize_test_config;
use crate::support::rooms::{room_runtime, test_room};
use crate::support::test_store;
use clankcord::model::job::CommandRequest;
use clankcord::model::job::JobKind;
use clankcord::model::rooms::RoomConfig;
use clankcord::store::TimelineStore;
use serde_json::json;

#[tokio::test(flavor = "current_thread")]
async fn room_mutating_command_rejects_missing_explicit_target() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(raw.path()).await;
    let room = test_room();
    let runtime = room_runtime(store.clone(), room.clone());
    let command = CommandRequest::from_json(&json!({
        "action": "dispatch_now",
        "command_kind": "leave_room",
        "guild_id": room.guild_id,
        "scope_id": "",
        "requested_by_user_id": "user-a",
        "arguments": {},
    }))
    .unwrap();

    let error =
        clankcord::domain::interactions::commands::create_command_job(&runtime, command, None)
            .await
            .unwrap_err()
            .to_string();

    assert!(error.contains("requires explicit room/channel target"));
    let jobs = store
        .list_jobs_by_scope_kind(&room.guild_id, &room.channel_id, JobKind::Command)
        .await
        .unwrap();
    assert!(jobs.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn room_mutating_command_uses_explicit_scope_instead_of_default_room() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(raw.path()).await;
    let art_room = add_art_room(&store).await;
    let runtime = room_runtime(store.clone(), art_room.clone());
    let command = CommandRequest::from_json(&json!({
        "action": "dispatch_now",
        "command_kind": "leave_room",
        "guild_id": art_room.guild_id,
        "scope_id": art_room.channel_id,
        "requested_by_user_id": "user-a",
        "arguments": {},
    }))
    .unwrap();

    let result =
        clankcord::domain::interactions::commands::create_command_job(&runtime, command, None)
            .await
            .unwrap();
    let job_id = result["job_ids"][0].as_str().unwrap();
    let job = store.get_job(job_id).await.unwrap();

    assert_eq!(job.scope_id, "art");
    assert_eq!(job.command().unwrap().scope_id, "art");
}

async fn add_art_room(store: &TimelineStore) -> RoomConfig {
    let pool = store.runtime_pool_config().await.unwrap();
    let control = store.control_config().await.unwrap();
    let guilds = store.list_guild_configs().await.unwrap();
    let mut rooms = store.list_room_configs().await.unwrap();
    let room = RoomConfig {
        room_id: "art-lounge".to_string(),
        guild_id: "guild".to_string(),
        guild_slug: "guild".to_string(),
        channel_id: "art".to_string(),
        channel_slug: "art-lounge".to_string(),
        channel_name: "Art Lounge".to_string(),
        auto_join: true,
    };
    rooms.push(room.clone());
    store
        .write_runtime_config_snapshot(&pool, &control, &guilds, &rooms)
        .await
        .unwrap();
    room
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
    assert!(help.contains(
        "clankcord transcripts render --since=-1h --file transcript.md --format markdown"
    ));
    assert!(help.contains("clankcord responses send <<'EOF'"));
    assert!(help.contains("clankcord automations validate < automation.json"));
    assert!(help.contains("clankcord coding spec"));
    assert!(help.contains("clankcord responses send --attachment result.zip"));
    assert!(help.contains("clankcord agent-sessions search"));
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
    assert!(help.contains("--attachment <ZIP>"));
    assert!(help.contains("Each attachment must be a .zip file"));
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
fn room_mutation_commands_require_room_target_before_submission() {
    for args in [
        &["rooms", "join"][..],
        &["rooms", "leave"],
        &["rooms", "mute"],
        &["rooms", "unmute"],
        &["rooms", "play-cue", "join"],
        &["pause"],
        &["resume"],
    ] {
        let output = clankcord(args);
        assert!(!output.status.success(), "{args:?}");
        let stderr = stderr(&output);
        assert!(
            stderr.contains("required") || stderr.contains("requires ROOM or --channel"),
            "{args:?}: {stderr}"
        );
    }
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
