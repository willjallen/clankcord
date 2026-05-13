use serde_json::json;

mod common;

use clankcord::runtime::Runtime;
use clankcord::runtime::domain::interactions::{
    evaluate_router_candidate, router_candidate_packet, validate_router_result,
};

use common::{dt, merge_json};

#[test]
fn voice_command_classifier_stdout_parser_normalizes_against_heuristic() {
    let wrapped = json!({
        "payloads": [{"text": "{\"action\":\"dispatch_now\",\"command_kind\":\"agent_task\",\"arguments\":{\"question\":\"what did I miss\"}}"}],
        "meta": {"agentMeta": {"model": "test"}}
    })
    .to_string();
    let parsed = Runtime::parse_voice_command_classifier_stdout(&wrapped).unwrap();
    assert_eq!(parsed["action"], json!("dispatch_now"));

    let heuristic = json!({
        "guild_id": "guild",
        "voice_channel_id": "code",
        "requested_by_user_id": "user-a",
        "requested_by_speaker_label": "Will",
        "source_event_ids": ["evt1"],
        "confidence": 0.92,
        "acknowledgement_text": "I will check."
    });
    let normalized = Runtime::normalize_voice_command_classifier_result(&parsed, &heuristic);
    assert_eq!(normalized["is_command"], json!(true));
    assert_eq!(normalized["guild_id"], json!("guild"));
    assert_eq!(normalized["voice_channel_id"], json!("code"));
    assert_eq!(normalized["requested_by_user_id"], json!("user-a"));
    assert_eq!(normalized["acknowledgement_text"], json!("I will check."));
    assert!(normalized["dedupe_hash"].as_str().unwrap_or("").len() >= 32);
}

#[test]
fn router_result_hydrates_previous_job_context() {
    let mut result = json!({
        "action": "dispatch_now",
        "command_kind": "agent_task",
        "activated_text": "Hey Clanky, can you explain what you said in your last response?",
        "arguments": {"request": "explain your last response"}
    });
    let interaction_context = json!({
        "recent_jobs": [{
            "job_id": "job_previous",
            "request": "What does the rollout do?",
            "response_preview": "It stages the deployment and verifies health checks."
        }]
    });

    Runtime::hydrate_router_result_recent_job_context(&mut result, &interaction_context);

    assert_eq!(
        result["arguments"]["previous_job_id"],
        json!("job_previous")
    );
    assert_eq!(
        result["arguments"]["previous_job_request"],
        json!("What does the rollout do?")
    );
    assert_eq!(
        result["arguments"]["previous_job_response_preview"],
        json!("It stages the deployment and verifies health checks.")
    );
}

#[test]
fn router_detects_channel_local_voice_command() {
    let event = json!({
        "event_id": "evt_1",
        "guild_id": "guild",
        "voice_channel_id": "code",
        "capture_run_id": "cap_1",
        "voice_bot_id": "clanky-vc1",
        "speaker_user_id": "user-a",
        "speaker_label": "Will",
        "text_draft": "Hey Clanky, start a live transcript from ten minutes ago."
    });
    let result = evaluate_router_candidate(&event, &[event.clone()], &json!({}), None);
    let (valid, reason) = validate_router_result(&result);
    assert!(valid, "{reason}");
    assert_eq!(result["command_kind"], json!("start_live_transcript"));
    assert_eq!(result["arguments"]["relative_start"], json!("-10m"));
}

#[test]
fn router_validation_does_not_gate_on_confidence() {
    let result = json!({
        "action": "dispatch_now",
        "is_command": true,
        "confidence": 0.01,
        "guild_id": "guild",
        "voice_channel_id": "code",
        "source_event_ids": ["evt_1"],
        "reason": "The model routed this request."
    });
    let (valid, reason) = validate_router_result(&result);
    assert!(valid, "{reason}");
}

#[test]
fn router_wake_and_control_edge_cases() {
    let base = json!({
        "guild_id": "guild",
        "voice_channel_id": "code",
        "capture_run_id": "cap_1",
        "voice_bot_id": "clanky-vc1",
        "speaker_user_id": "user-a",
        "speaker_label": "Will"
    });
    let without_wake = merge_json(
        &base,
        json!({
            "event_id": "evt_leave_without_wake",
            "text_draft": "I told him to leave and he interpreted that as deafening."
        }),
    );
    let result =
        evaluate_router_candidate(&without_wake, &[without_wake.clone()], &json!({}), None);
    assert!(!validate_router_result(&result).0);
    assert_eq!(result["is_command"], json!(false));

    let leave = merge_json(
        &base,
        json!({"event_id": "evt_leave", "text_draft": "Hey Clanky, leave the room."}),
    );
    let deafen = merge_json(
        &base,
        json!({"event_id": "evt_deafen", "text_draft": "Hey Clanky, deafen yourself."}),
    );
    let leave_result = evaluate_router_candidate(&leave, &[leave.clone()], &json!({}), None);
    let deafen_result = evaluate_router_candidate(&deafen, &[deafen.clone()], &json!({}), None);
    assert!(validate_router_result(&leave_result).0);
    assert!(validate_router_result(&deafen_result).0);
    assert_eq!(leave_result["command_kind"], json!("leave_room"));
    assert_eq!(deafen_result["command_kind"], json!("deafen_listening"));

    let join = merge_json(
        &base,
        json!({"event_id": "evt_join", "text_draft": "Hey Clanky, join the art lounge."}),
    );
    let join_result = evaluate_router_candidate(&join, &[join.clone()], &json!({}), None);
    assert!(validate_router_result(&join_result).0);
    assert_eq!(join_result["command_kind"], json!("join_room"));
    assert_eq!(join_result["arguments"]["target_room"], json!("art lounge"));
}

#[test]
fn router_packet_uses_wake_window_for_split_question() {
    let start = dt(2026, 5, 12, 16, 0, 0);
    let base = json!({
        "guild_id": "guild",
        "voice_channel_id": "code",
        "capture_run_id": "cap_1",
        "voice_bot_id": "clanky-vc1",
        "speaker_user_id": "user-a",
        "speaker_label": "Will"
    });
    let prior = merge_json(
        &base,
        json!({
            "event_id": "evt_prior",
            "text_draft": "we were talking about birds a little earlier",
            "segment_start_time": (start - chrono::Duration::seconds(20)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        }),
    );
    let wake = merge_json(
        &base,
        json!({
            "event_id": "evt_wake",
            "text_draft": "Hey Planky...",
            "segment_start_time": start.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        }),
    );
    let candidate = merge_json(
        &base,
        json!({
            "event_id": "evt_birds",
            "text_draft": "birds tell me some facts about birds",
            "segment_start_time": (start + chrono::Duration::seconds(4)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        }),
    );
    let events = vec![prior.clone(), wake.clone(), candidate.clone()];
    let result = evaluate_router_candidate(&wake, &events, &json!({}), None);

    assert!(!validate_router_result(&result).0);
    assert_eq!(result["source_event_ids"], json!(["evt_wake", "evt_birds"]));
    assert_eq!(
        result["context_source_event_ids"],
        json!(["evt_prior", "evt_wake", "evt_birds"])
    );
    assert!(
        result["instruction_text"]
            .as_str()
            .unwrap()
            .contains("birds")
    );

    let packet = router_candidate_packet(
        &wake,
        &events,
        &json!({"mode": "local_buffering"}),
        None,
        None,
        None,
    );
    assert_eq!(packet["window_events"][0]["event_id"], json!("evt_prior"));
    assert_eq!(
        packet["instruction_events"][0]["event_id"],
        json!("evt_wake")
    );
    assert!(
        packet["window_events"][0]["text"]
            .as_str()
            .unwrap()
            .contains("we were talking about birds")
    );
    assert!(
        !packet["instruction_text"]
            .as_str()
            .unwrap()
            .contains("we were talking about birds")
    );
    let instructions = packet["instructions"].as_array().unwrap();
    assert!(instructions.iter().any(|instruction| {
        instruction
            .as_str()
            .unwrap_or_default()
            .contains("Do not require imperative grammar")
    }));
    assert!(instructions.iter().any(|instruction| {
        instruction
            .as_str()
            .unwrap_or_default()
            .contains("what I want to know")
    }));
    assert!(instructions.iter().any(|instruction| {
        instruction
            .as_str()
            .unwrap_or_default()
            .contains("recent_jobs")
    }));
    assert!(
        packet["response_schema"]
            .as_object()
            .unwrap()
            .contains_key("arguments.previous_job_id")
    );
}

#[test]
fn router_packet_compacts_active_job_payloads() {
    let event = json!({
        "event_id": "evt_packet_jobs",
        "guild_id": "guild",
        "voice_channel_id": "code",
        "speaker_user_id": "user-a",
        "speaker_label": "Will",
        "text_draft": "Hey Clanky, tell me what we are talking about."
    });
    let room_status = json!({
        "mode": "local_buffering",
        "activeJobs": [{
            "job_id": "job_large",
            "kind": "confirmation_required",
            "state": "confirmation_pending",
            "guild_id": "guild",
            "voice_channel_id": "code",
            "requested_by_user_id": "user-a",
            "payload": {
                "command": {"command_kind": "forget_window", "arguments": {"request": "forget that"}},
                "confirmation": {"forget_items": [{"text": "x".repeat(50_000)}]}
            }
        }]
    });

    let packet = router_candidate_packet(&event, &[event.clone()], &room_status, None, None, None);
    let job = &packet["room_status"]["activeJobs"][0];
    assert_eq!(job["job_id"], json!("job_large"));
    assert_eq!(job["command_kind"], json!("forget_window"));
    assert!(job.get("payload").is_none());
    assert!(serde_json::to_string(&packet["room_status"]).unwrap().len() < 1000);
}
