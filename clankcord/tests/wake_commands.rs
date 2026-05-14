use chrono::SecondsFormat;
use serde_json::{Value, json};

mod common;

use clankcord::runtime::domain::interactions::{
    activation_context_events, evaluate_voice_command, validate_voice_command_result,
    voice_command_action,
};

use common::{dt, merge_json};

fn base_event() -> Value {
    json!({
        "guild_id": "guild",
        "voice_channel_id": "code",
        "capture_run_id": "cap_1",
        "voice_bot_id": "clanky-vc1",
        "speaker_user_id": "user-a",
        "speaker_label": "Will"
    })
}

fn wake_metadata() -> Value {
    json!({
        "wake": true,
        "score": 0.73,
        "threshold": 0.50,
        "model_label": "hey_clanky",
        "stream_id": "guild:code:user-a",
        "processed_frames": 3,
        "scores": {"hey_clanky": 0.73}
    })
}

#[test]
fn wake_word_dispatches_general_agent_task_without_agent_classifier() {
    let event = merge_json(
        &base_event(),
        json!({
            "event_id": "evt_1",
            "text_draft": "Hey Clanky, what did I miss while I was away?",
            "wake": wake_metadata()
        }),
    );

    let result = evaluate_voice_command(&event, &[event.clone()], &json!({}));
    let (valid, reason) = validate_voice_command_result(&result);

    assert!(valid, "{reason}");
    assert_eq!(voice_command_action(&result), "dispatch_now");
    assert_eq!(result["command_kind"], json!("agent_task"));
    assert_eq!(result["requested_by_user_id"], json!("user-a"));
    assert_eq!(result["confidence"], json!(0.73));
    assert_eq!(
        result["arguments"]["request"],
        json!("what did I miss while I was away?")
    );
}

#[test]
fn voice_text_without_wake_does_not_dispatch() {
    let event = merge_json(
        &base_event(),
        json!({
            "event_id": "evt_leave_without_wake",
            "text_draft": "leave the room"
        }),
    );

    let result = evaluate_voice_command(&event, &[event.clone()], &json!({}));

    assert_eq!(voice_command_action(&result), "ignore");
    assert!(!validate_voice_command_result(&result).0);
    assert_eq!(result["wake_detected"], json!(false));
}

#[test]
fn wake_word_dispatches_builtin_room_commands() {
    let leave = merge_json(
        &base_event(),
        json!({
            "event_id": "evt_leave",
            "text_draft": "Hey Clanky, leave the room.",
            "wake": wake_metadata()
        }),
    );
    let join = merge_json(
        &base_event(),
        json!({
            "event_id": "evt_join",
            "text_draft": "Hey Clanky, join the art lounge.",
            "wake": wake_metadata()
        }),
    );

    let leave_result = evaluate_voice_command(&leave, &[leave.clone()], &json!({}));
    let join_result = evaluate_voice_command(&join, &[join.clone()], &json!({}));

    assert!(validate_voice_command_result(&leave_result).0);
    assert_eq!(leave_result["command_kind"], json!("leave_room"));
    assert!(validate_voice_command_result(&join_result).0);
    assert_eq!(join_result["command_kind"], json!("join_room"));
    assert_eq!(join_result["arguments"]["target_room"], json!("art lounge"));
}

#[test]
fn prior_wake_activates_same_speaker_followup() {
    let start = dt(2026, 5, 12, 16, 0, 0);
    let wake = merge_json(
        &base_event(),
        json!({
            "event_id": "evt_wake",
            "text_draft": "Hey Clanky",
            "segment_start_time": start.to_rfc3339_opts(SecondsFormat::Secs, true),
            "wake": wake_metadata()
        }),
    );
    let candidate = merge_json(
        &base_event(),
        json!({
            "event_id": "evt_question",
            "text_draft": "tell me what we were talking about",
            "segment_start_time": (start + chrono::Duration::seconds(4)).to_rfc3339_opts(SecondsFormat::Secs, true)
        }),
    );
    let events = vec![wake.clone(), candidate.clone()];

    let context = activation_context_events(&candidate, &events);
    let result = evaluate_voice_command(&candidate, &events, &json!({}));

    assert_eq!(
        context
            .iter()
            .map(|event| event["event_id"].clone())
            .collect::<Vec<_>>(),
        vec![json!("evt_wake"), json!("evt_question")]
    );
    assert!(validate_voice_command_result(&result).0);
    assert_eq!(result["wake_on_candidate"], json!(false));
    assert_eq!(result["command_kind"], json!("agent_task"));
    assert_eq!(
        result["arguments"]["request"],
        json!("what we were talking about")
    );
    assert_eq!(
        result["source_event_ids"],
        json!(["evt_wake", "evt_question"])
    );
}

#[test]
fn wake_only_segment_is_not_an_actionable_command() {
    let event = merge_json(
        &base_event(),
        json!({
            "event_id": "evt_wake_only",
            "text_draft": "Hey Clanky",
            "wake": wake_metadata()
        }),
    );

    let result = evaluate_voice_command(&event, &[event.clone()], &json!({}));

    assert_eq!(voice_command_action(&result), "ignore");
    assert!(!validate_voice_command_result(&result).0);
    assert_eq!(result["wake_detected"], json!(true));
}
