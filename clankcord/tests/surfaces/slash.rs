//! Slash-command response copy.

use crate::support::cli::slash_payload;
use clankcord::adapters::discord::gateway::slash::slash_missing_voice_channel_response_content;
use clankcord::adapters::discord::gateway::slash::slash_success_response_content;
use serde_json::json;

#[test]
fn slash_command_responses_are_human_readable() {
    let join = slash_success_response_content(&slash_payload(
        "interaction-join",
        "join",
        "slash-text",
        "code",
        json!([]),
    ));
    assert_eq!(join, "Connecting Clanky to <#code>.");

    let deafen = slash_success_response_content(&slash_payload(
        "interaction-deafen",
        "deafen",
        "slash-text",
        "code",
        json!([]),
    ));
    assert_eq!(deafen, "Deafening Clanky in <#code>.");

    let feedback = slash_success_response_content(&slash_payload(
        "interaction-feedback",
        "feedback",
        "slash-text",
        "",
        json!([{"name": "message", "value": "The join command stalled."}]),
    ));
    assert_eq!(feedback, "Feedback sent: The join command stalled.");

    let responses = [
        join,
        deafen,
        feedback,
        slash_missing_voice_channel_response_content().to_string(),
    ];
    for response in responses {
        assert!(!response.contains("job_"));
        assert!(!response.contains("queued"));
    }
    assert_eq!(
        slash_missing_voice_channel_response_content(),
        "You are not in a voice channel."
    );
}
