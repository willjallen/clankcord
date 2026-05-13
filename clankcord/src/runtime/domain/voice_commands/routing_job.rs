use std::env;
use std::fs;
use std::path::Path;
use std::process::Command;

use serde_json::{Value, json};

use crate::Result;
use crate::adapters::discord::api::send_message;
use crate::config::{
    MESSAGE_CHUNK_LIMIT, non_empty, split_message_chunks, string_field, write_json,
};
use crate::runtime::domain::voice_commands::{
    acknowledgement_text_for_command, dedupe_hash, requires_confirmation, router_action,
};

use crate::runtime::Runtime;
use crate::runtime::util::{
    first_non_empty, preview, preview_tail, set_if_blank, update_object_fields,
};

impl Runtime {
    pub fn parse_json_object_from_text(text: &str) -> Option<Value> {
        let raw = text.trim();
        if raw.is_empty() {
            return None;
        }
        if let Ok(parsed) = serde_json::from_str::<Value>(raw)
            && parsed.is_object()
        {
            return Some(parsed);
        }
        let start = raw.find('{')?;
        let end = raw.rfind('}')?;
        if end <= start {
            return None;
        }
        serde_json::from_str::<Value>(&raw[start..=end])
            .ok()
            .filter(Value::is_object)
    }

    pub fn parse_voice_command_classifier_stdout(stdout: &str) -> Option<Value> {
        let payload = Self::parse_openclaw_agent_stdout(stdout);
        if payload.is_object()
            && (payload.get("is_command").is_some() || payload.get("action").is_some())
        {
            return Some(payload);
        }
        if let Some(entries) = payload
            .get("payloads")
            .or_else(|| payload.get("outputs"))
            .and_then(Value::as_array)
        {
            let texts = entries
                .iter()
                .filter_map(|entry| entry.get("text").and_then(Value::as_str))
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>();
            if !texts.is_empty()
                && let Some(parsed) = Self::parse_json_object_from_text(&texts.join("\n\n"))
            {
                return Some(parsed);
            }
        }
        if let Some(meta) = payload.get("meta").and_then(Value::as_object) {
            for key in ["finalAssistantVisibleText", "finalAssistantRawText"] {
                if let Some(parsed) = Self::parse_json_object_from_text(
                    meta.get(key).and_then(Value::as_str).unwrap_or_default(),
                ) {
                    return Some(parsed);
                }
            }
        }
        if let Some(text) = payload.as_str()
            && let Some(parsed) = Self::parse_json_object_from_text(text)
        {
            return Some(parsed);
        }
        Self::parse_json_object_from_text(stdout)
    }

    pub fn build_voice_command_classifier_prompt(packet_path: &Path, packet: &Value) -> String {
        let compact_packet =
            serde_json::to_string_pretty(packet).unwrap_or_else(|_| "{}".to_string());
        [
            "You are the Clawcord voice router.",
            "",
            &format!("Router packet path: {}", packet_path.display()),
            "",
            "Decide whether this wake-window contains an actionable request addressed to Clanky.",
            "The deterministic gate already found a Hey Clanky wake variant, or this is a follow-up after Clawcord asked for missing detail. Your job is nuance: inspect the entire window_events list, not just candidate_event.",
            "Return an action: dispatch_now, wait_for_more, ignore, cancel_job, amend_job, or replace_job.",
            "The request may be explained across the idle-closed settle window.",
            "Do not require imperative grammar: direct questions, requests for opinions, corrections, complaints about a prior Clanky answer, and statements like 'what I want to know is ...' are actionable when addressed to Clanky.",
            "Reject casual discussion, jokes, examples, quoted text, third-person mentions, and wake windows with no actionable information need.",
            "For requests that need reasoning, retrieval, summarization, research, planning, or external tool use, use command_kind=voice_agent_task or omit command_kind.",
            "Use specific command_kind values only for built-in voice controls or transcript materialization that Clawcord should execute directly, including join_room and leave_room.",
            "If activation_mode=followup, a new wake phrase is not required; compose the current window with interaction_context.turn_history and the prior wait_for_more request.",
            "Address override: if the wake-gated request says 'actually do this' or 'actually <verb>', treat it as intentionally addressed to Clanky even if the speaker frames it as an example.",
            "When that override is present, do not reject solely because of third-person, hypothetical, quoted, or example framing; normalize the concrete action from surrounding words.",
            "Use interaction_context for previous acknowledgements, active jobs, recent completed jobs, cancellable job ids, and follow-up corrections.",
            "Use interaction_context.recent_jobs to resolve references like 'your last response', 'my last question', or 'the thing you just said'.",
            "When routing a request that references prior Clanky work, include arguments.previous_job_id from interaction_context.recent_jobs when identifiable.",
            "For corrections to completed Clanky work, dispatch a new voice_agent_task rather than trying to cancel or amend a terminal job.",
            "If the user names another voice room, preserve that phrase in arguments.target_room or arguments.voice_channel_name.",
            "If the user says today, yesterday, or an explicit YYYY-MM-DD date, preserve it in arguments.date_reference.",
            "If the user asks for work-related material, set arguments.work_related=true.",
            "Route when it is pretty clear someone is talking to Clanky and giving Clanky something to answer, redo, investigate, summarize, or operate.",
            "Return one JSON object only. No markdown, no prose outside JSON.",
            "If action will dispatch, cancel, amend, or replace work, include acknowledgement_text: a short first-person status phrase for agent-chat. Do not include a Discord mention.",
            "",
            "ROUTER_PACKET_JSON:",
            &compact_packet,
        ]
        .join("\n")
    }

    pub fn hydrate_router_result_recent_job_context(
        result: &mut Value,
        interaction_context: &Value,
    ) {
        if router_action(result) != "dispatch_now" {
            return;
        }
        let arguments_snapshot = result
            .get("arguments")
            .filter(|value| value.is_object())
            .cloned()
            .unwrap_or_else(|| json!({}));
        if !string_field(&arguments_snapshot, "previous_job_id").is_empty() {
            return;
        }
        let lowered = [
            string_field(result, "activated_text"),
            string_field(result, "instruction_text"),
            string_field(&arguments_snapshot, "request"),
            string_field(&arguments_snapshot, "query"),
            string_field(&arguments_snapshot, "question"),
        ]
        .join(" ")
        .to_lowercase();
        let references_prior_work = [
            "last response",
            "last answer",
            "last question",
            "previous response",
            "previous answer",
            "you just",
            "what you said",
        ]
        .iter()
        .any(|marker| lowered.contains(marker));
        if !references_prior_work {
            return;
        }
        let Some(previous) = interaction_context
            .get("recent_jobs")
            .and_then(Value::as_array)
            .and_then(|jobs| {
                jobs.iter()
                    .find(|job| job.is_object() && !string_field(job, "job_id").is_empty())
            })
        else {
            return;
        };
        let previous_job_id = string_field(previous, "job_id");
        if previous_job_id.is_empty() {
            return;
        }
        if !result.get("arguments").is_some_and(Value::is_object) {
            if let Some(map) = result.as_object_mut() {
                map.insert("arguments".to_string(), json!({}));
            } else {
                return;
            }
        }
        let Some(arguments) = result.get_mut("arguments").and_then(Value::as_object_mut) else {
            return;
        };
        arguments.insert("previous_job_id".to_string(), json!(previous_job_id));
        let previous_request = string_field(previous, "request");
        if !previous_request.is_empty() {
            arguments.insert(
                "previous_job_request".to_string(),
                json!(preview(&previous_request, 1000)),
            );
        }
        let previous_response_preview = string_field(previous, "response_preview");
        if !previous_response_preview.is_empty() {
            arguments.insert(
                "previous_job_response_preview".to_string(),
                json!(preview(&previous_response_preview, 1200)),
            );
        }
    }

    pub fn normalize_voice_command_classifier_result(
        agent_result: &Value,
        heuristic_result: &Value,
    ) -> Value {
        let mut result = agent_result.clone();
        if !result.is_object() {
            result = json!({});
        }
        update_object_fields(&mut result, [("router_model_invoked", json!(true))]).ok();
        if let Some(raw) = result.get("is_command").and_then(Value::as_str) {
            let parsed = matches!(
                raw.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes"
            );
            update_object_fields(&mut result, [("is_command", json!(parsed))]).ok();
        }
        if string_field(&result, "action").is_empty() {
            let action = if result
                .get("is_command")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                "dispatch_now"
            } else {
                "ignore"
            };
            update_object_fields(&mut result, [("action", json!(action))]).ok();
        }
        let action = router_action(&result);
        update_object_fields(&mut result, [("action", json!(action.clone()))]).ok();
        if matches!(action.as_str(), "ignore" | "wait_for_more") {
            set_if_blank(&mut result, "is_command", json!(false));
            set_if_blank(&mut result, "confidence", json!(0.0));
            set_if_blank(
                &mut result,
                "source_event_ids",
                heuristic_result
                    .get("source_event_ids")
                    .cloned()
                    .unwrap_or_else(|| json!([])),
            );
            set_if_blank(&mut result, "wake_phrase_detected", json!(true));
            set_if_blank(
                &mut result,
                "reason",
                json!("Router model did not classify the wake window as a command."),
            );
            if action == "wait_for_more" {
                set_if_blank(
                    &mut result,
                    "acknowledgement_text",
                    json!("I need a little more detail before I can do that."),
                );
            }
            return result;
        }

        if let Some(replacement) = result
            .get("replacement_command")
            .and_then(Value::as_object)
            .cloned()
        {
            for (key, value) in replacement {
                set_if_blank(&mut result, &key, value);
            }
        }

        if action == "cancel_job" {
            set_if_blank(&mut result, "is_command", json!(false));
            for key in [
                "requested_by_user_id",
                "requested_by_speaker_label",
                "guild_id",
                "voice_channel_id",
                "capture_run_id",
                "voice_bot_id",
                "source_event_ids",
            ] {
                if let Some(value) = heuristic_result.get(key) {
                    set_if_blank(&mut result, key, value.clone());
                }
            }
            set_if_blank(&mut result, "wake_phrase_detected", json!(true));
            set_if_blank(
                &mut result,
                "reason",
                json!("Router model classified this as a cancellation."),
            );
            set_if_blank(
                &mut result,
                "acknowledgement_text",
                json!("Got it. I am cancelling that."),
            );
            return result;
        }

        update_object_fields(&mut result, [("is_command", json!(true))]).ok();
        for key in [
            "command_kind",
            "requested_by_user_id",
            "requested_by_speaker_label",
            "guild_id",
            "voice_channel_id",
            "capture_run_id",
            "voice_bot_id",
            "arguments",
            "requires_confirmation",
            "source_event_ids",
            "dedupe_hash",
            "matched_wake_phrase",
            "activated_text",
        ] {
            if let Some(value) = heuristic_result.get(key) {
                set_if_blank(&mut result, key, value.clone());
            }
        }
        set_if_blank(&mut result, "wake_phrase_detected", json!(true));
        set_if_blank(
            &mut result,
            "reason",
            heuristic_result
                .get("reason")
                .cloned()
                .unwrap_or_else(|| json!("Router model classified this as a Hey Clanky command.")),
        );
        let command_kind = string_field(&result, "command_kind");
        if result.get("requires_confirmation").is_none() {
            set_if_blank(
                &mut result,
                "requires_confirmation",
                json!(requires_confirmation(&command_kind)),
            );
        }
        set_if_blank(
            &mut result,
            "acknowledgement_text",
            heuristic_result
                .get("acknowledgement_text")
                .cloned()
                .unwrap_or_else(|| json!(acknowledgement_text_for_command(&command_kind))),
        );
        let guild_id = string_field(&result, "guild_id");
        let channel_id = string_field(&result, "voice_channel_id");
        if !guild_id.is_empty() && !channel_id.is_empty() && !command_kind.is_empty() {
            let source_event_ids = result
                .get("source_event_ids")
                .and_then(Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .map(|value| value.as_str().unwrap_or_default().to_string())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
                .join("|");
            let arguments = result
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            update_object_fields(
                &mut result,
                [(
                    "dedupe_hash",
                    json!(dedupe_hash(
                        &guild_id,
                        &channel_id,
                        &source_event_ids,
                        &command_kind,
                        &arguments
                    )),
                )],
            )
            .ok();
        }
        result
    }

    pub fn router_model_candidates() -> Vec<String> {
        let default_model = env::var("OPENCLAW_ROUTER_MODEL")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "openrouter/auto".to_string());
        let fallback_model = env::var("OPENCLAW_ROUTER_FALLBACK_MODEL")
            .ok()
            .map(|value| value.trim().to_string())
            .unwrap_or_default();
        let mut models = Vec::new();
        for model in [default_model, fallback_model] {
            if !model.is_empty() && !models.contains(&model) {
                models.push(model);
            }
        }
        models
    }

    pub fn evaluate_voice_command_with_classifier(
        &self,
        guild_id: &str,
        channel_id: &str,
        packet: &Value,
        heuristic_result: &Value,
    ) -> Result<Value> {
        let packet_id = first_non_empty([
            string_field(packet, "packet_id"),
            uuid::Uuid::new_v4().simple().to_string(),
        ]);
        let router_dir = self
            .timeline_store
            .channel_dir(guild_id, channel_id)
            .join("router");
        fs::create_dir_all(&router_dir)?;
        let packet_path = router_dir.join(format!("{packet_id}.packet.json"));
        write_json(&packet_path, packet)?;
        let message = Self::build_voice_command_classifier_prompt(&packet_path, packet);
        let message_path = router_dir.join(format!("{packet_id}.prompt.txt"));
        fs::write(&message_path, &message)?;
        let result_path = router_dir.join(format!("{packet_id}.agent-result.json"));
        let openclaw_bin = env::var("OPENCLAW_BIN").unwrap_or_else(|_| "openclaw".to_string());
        let mut attempts = Vec::new();
        let mut last_failure_reason = String::new();
        for model in Self::router_model_candidates() {
            let model_name = model.clone();
            let output = Command::new(&openclaw_bin)
                .args([
                    "infer", "model", "run", "--local", "--model", &model, "--prompt", &message,
                    "--json",
                ])
                .output()?;
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            fs::write(&result_path, format!("{}\n", stdout.trim()))?;
            let mut attempt = json!({
                "model": model_name,
                "returncode": output.status.code(),
                "stderr": preview_tail(&stderr, 2000),
            });
            if !output.status.success() {
                last_failure_reason =
                    first_non_empty([stderr.trim().to_string(), stdout.trim().to_string()]);
                if last_failure_reason.is_empty() {
                    last_failure_reason = format!(
                        "router model exited {}",
                        output
                            .status
                            .code()
                            .map(|code| code.to_string())
                            .unwrap_or_else(|| "without a status code".to_string())
                    );
                }
                update_object_fields(
                    &mut attempt,
                    [("reason", json!(preview_tail(&last_failure_reason, 2000)))],
                )?;
                attempts.push(attempt);
                continue;
            }
            let Some(agent_result) = Self::parse_voice_command_classifier_stdout(&stdout) else {
                last_failure_reason = "router model did not return a JSON object".to_string();
                update_object_fields(&mut attempt, [("reason", json!(last_failure_reason))])?;
                attempts.push(attempt);
                continue;
            };
            attempts.push(attempt);
            let mut result =
                Self::normalize_voice_command_classifier_result(&agent_result, heuristic_result);
            Self::hydrate_router_result_recent_job_context(
                &mut result,
                packet.get("interaction_context").unwrap_or(&Value::Null),
            );
            update_object_fields(
                &mut result,
                [
                    ("router_model", json!(model)),
                    ("router_model_attempts", json!(attempts)),
                    (
                        "router_inference_command",
                        json!("openclaw infer model run --local --prompt --json"),
                    ),
                    (
                        "router_packet_path",
                        json!(packet_path.display().to_string()),
                    ),
                    (
                        "router_prompt_path",
                        json!(message_path.display().to_string()),
                    ),
                    (
                        "router_result_path",
                        json!(result_path.display().to_string()),
                    ),
                ],
            )?;
            return Ok(result);
        }
        Ok(json!({
            "is_command": false,
            "confidence": 0.0,
            "wake_phrase_detected": true,
            "source_event_ids": heuristic_result.get("source_event_ids").cloned().unwrap_or_else(|| json!([])),
            "reason": if last_failure_reason.is_empty() {
                "no router model candidates were configured".to_string()
            } else {
                last_failure_reason
            },
            "router_model_invoked": !attempts.is_empty(),
            "router_model": attempts.last().map(|attempt| string_field(attempt, "model")).unwrap_or_else(|| env::var("OPENCLAW_ROUTER_MODEL").unwrap_or_else(|_| "openrouter/auto".to_string())),
            "router_model_attempts": attempts,
            "router_inference_command": "openclaw infer model run --local --prompt --json",
            "router_packet_path": packet_path.display().to_string(),
            "router_prompt_path": message_path.display().to_string(),
            "router_result_path": result_path.display().to_string(),
        }))
    }

    pub fn post_voice_command_acknowledgement(&self, result: &Value) -> Value {
        let channel_id = self.control_config.bots_channel_id.clone();
        if channel_id.is_empty() {
            return json!({});
        }
        let requested_by = string_field(result, "requested_by_user_id");
        let acknowledgement = non_empty(
            string_field(result, "acknowledgement_text"),
            "Working on that for you.".to_string(),
        );
        let content = if requested_by.is_empty() {
            acknowledgement
        } else {
            format!("<@{requested_by}> {acknowledgement}")
        };
        let mut posted = Vec::new();
        for chunk in split_message_chunks(&content, MESSAGE_CHUNK_LIMIT) {
            match send_message(&channel_id, &chunk) {
                Ok(payload) => posted.push(json!({
                    "channel_id": channel_id,
                    "message_id": string_field(&payload, "id"),
                })),
                Err(error) => {
                    return json!({
                        "channel_id": channel_id,
                        "messages": posted,
                        "error": error.to_string(),
                    });
                }
            }
        }
        json!({"channel_id": channel_id, "messages": posted})
    }
}
