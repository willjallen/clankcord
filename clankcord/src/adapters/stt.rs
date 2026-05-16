use std::env;
use std::path::Path;

use anyhow::Context;
use reqwest::blocking::multipart;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value};

use crate::Result;
use crate::config::load_stt_base_url;
use crate::runtime::util::{finite_number, number_or_null};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TranscriptionResult {
    pub text: String,
    pub metadata: Value,
}

pub fn stt_transcriptions_url() -> Result<String> {
    let base_url = load_stt_base_url()?.trim_end_matches('/').to_string();
    if base_url.ends_with("/audio/transcriptions") {
        Ok(base_url)
    } else {
        Ok(format!("{base_url}/audio/transcriptions"))
    }
}

pub fn stt_model() -> String {
    env::var("CLANKCORD_STT_MODEL")
        .unwrap_or_else(|_| "large-v3".to_string())
        .trim()
        .to_string()
        .chars()
        .collect::<String>()
        .if_empty("large-v3")
}

pub fn stt_language() -> String {
    env::var("CLANKCORD_STT_LANGUAGE")
        .unwrap_or_else(|_| "en".to_string())
        .trim()
        .to_string()
}

pub fn stt_response_format() -> String {
    env::var("CLANKCORD_STT_RESPONSE_FORMAT")
        .unwrap_or_else(|_| "json".to_string())
        .trim()
        .to_string()
        .if_empty("json")
}

pub fn stt_include_logprobs() -> bool {
    matches!(
        env::var("CLANKCORD_STT_INCLUDE_LOGPROBS")
            .unwrap_or_else(|_| "1".to_string())
            .trim()
            .to_lowercase()
            .as_str(),
        "1" | "true" | "yes" | "on"
    )
}

pub fn stt_max_token_logprobs() -> usize {
    env::var("CLANKCORD_STT_MAX_TOKEN_LOGPROBS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(64)
}

pub fn stt_timeout_seconds() -> u64 {
    env::var("CLANKCORD_STT_TIMEOUT_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(120)
}

pub fn stt_drop_no_speech_threshold() -> f64 {
    env::var("CLANKCORD_STT_DROP_NO_SPEECH_PROB")
        .ok()
        .and_then(|value| value.parse::<f64>().ok())
        .unwrap_or(0.7)
}

pub fn stt_drop_avg_token_logprob_threshold() -> f64 {
    env::var("CLANKCORD_STT_DROP_AVG_TOKEN_LOGPROB")
        .ok()
        .and_then(|value| value.parse::<f64>().ok())
        .unwrap_or(-0.8)
}

pub fn stt_api_key() -> String {
    env::var("CLANKCORD_STT_API_KEY")
        .unwrap_or_default()
        .trim()
        .to_string()
}

pub fn content_type_for_path(path: &Path) -> String {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_lowercase()
        .as_str()
    {
        "wav" => "audio/wav".to_string(),
        "mp3" => "audio/mpeg".to_string(),
        "ogg" | "opus" => "audio/ogg".to_string(),
        "m4a" => "audio/mp4".to_string(),
        "flac" => "audio/flac".to_string(),
        _ => mime_guess::from_path(path)
            .first_raw()
            .unwrap_or("application/octet-stream")
            .to_string(),
    }
}

pub fn stt_no_speech_probability(metadata: Option<&Value>) -> Option<f64> {
    let Value::Object(map) = metadata? else {
        return None;
    };
    let mut probabilities = Vec::new();
    if let Some(value) = finite_number(map.get("no_speech_prob")) {
        probabilities.push(value);
    }
    if let Some(local) = map.get("local").and_then(Value::as_object) {
        if let Some(value) = finite_number(local.get("estimated_no_speech_prob")) {
            probabilities.push(value);
        }
    }
    probabilities.into_iter().reduce(f64::max)
}

pub fn stt_avg_token_logprob(metadata: Option<&Value>) -> Option<f64> {
    metadata?
        .get("tokens")
        .and_then(Value::as_object)
        .and_then(|tokens| finite_number(tokens.get("avg_token_logprob")))
}

pub fn stt_drop_decision(
    metadata: Option<&Value>,
    no_speech_threshold: Option<f64>,
    avg_token_logprob_threshold: Option<f64>,
) -> Value {
    let no_speech_cutoff = no_speech_threshold.unwrap_or_else(stt_drop_no_speech_threshold);
    let token_cutoff =
        avg_token_logprob_threshold.unwrap_or_else(stt_drop_avg_token_logprob_threshold);
    let no_speech_prob = stt_no_speech_probability(metadata);
    let token_avg = stt_avg_token_logprob(metadata);
    let mut reasons = Vec::<Value>::new();
    if no_speech_prob.is_some_and(|value| value > no_speech_cutoff) {
        reasons.push(Value::String("no_speech".to_string()));
    }
    if token_avg.is_some_and(|value| value < token_cutoff) {
        reasons.push(Value::String("avg_token_logprob".to_string()));
    }
    serde_json::json!({
        "drop": !reasons.is_empty(),
        "reasons": reasons,
        "no_speech_prob": no_speech_prob,
        "no_speech_threshold": no_speech_cutoff,
        "avg_token_logprob": token_avg,
        "avg_token_logprob_threshold": token_cutoff
    })
}

pub fn should_drop_low_confidence_transcription(
    metadata: Option<&Value>,
    no_speech_threshold: Option<f64>,
    avg_token_logprob_threshold: Option<f64>,
) -> bool {
    stt_drop_decision(metadata, no_speech_threshold, avg_token_logprob_threshold)
        .get("drop")
        .and_then(Value::as_bool)
        == Some(true)
}

pub fn compact_token_logprobs(entries: Option<&Value>, limit: Option<usize>) -> (Value, Value) {
    let Some(entries) = entries.and_then(Value::as_array) else {
        return (
            Value::Array(Vec::new()),
            serde_json::json!({"token_count": 0}),
        );
    };
    let max_entries = limit.unwrap_or_else(stt_max_token_logprobs);
    let mut compact = Vec::new();
    let mut logprobs = Vec::new();
    for entry in entries {
        let Some(entry_map) = entry.as_object() else {
            continue;
        };
        let logprob = finite_number(entry_map.get("logprob"));
        if let Some(value) = logprob {
            logprobs.push(value);
        }
        if compact.len() < max_entries {
            let mut token_entry = Map::new();
            token_entry.insert(
                "token".to_string(),
                Value::String(
                    entry_map
                        .get("token")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                ),
            );
            token_entry.insert("logprob".to_string(), number_or_null(logprob));
            if let Some(raw_bytes) = entry_map.get("bytes").and_then(Value::as_array) {
                if raw_bytes.len() <= 16 {
                    token_entry.insert("bytes".to_string(), Value::Array(raw_bytes.clone()));
                }
            }
            compact.push(Value::Object(token_entry));
        }
    }
    let mut stats = Map::new();
    stats.insert(
        "token_count".to_string(),
        Value::Number(Number::from(entries.len())),
    );
    stats.insert(
        "token_logprobs_truncated".to_string(),
        Value::Bool(compact.len() < entries.len()),
    );
    if !logprobs.is_empty() {
        let avg = logprobs.iter().sum::<f64>() / logprobs.len() as f64;
        stats.insert(
            "avg_token_logprob".to_string(),
            number_or_null(Some(round4(avg))),
        );
        stats.insert(
            "min_token_logprob".to_string(),
            number_or_null(Some(round4(
                logprobs.iter().copied().fold(f64::INFINITY, f64::min),
            ))),
        );
        stats.insert(
            "max_token_logprob".to_string(),
            number_or_null(Some(round4(
                logprobs.iter().copied().fold(f64::NEG_INFINITY, f64::max),
            ))),
        );
    }
    (Value::Array(compact), Value::Object(stats))
}

pub fn parse_stt_payload(payload: &Value) -> TranscriptionResult {
    let text = payload
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let first_segment = payload
        .get("segments")
        .and_then(Value::as_array)
        .and_then(|segments| segments.first())
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let local = payload
        .get("local")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let (token_logprobs, token_stats) = compact_token_logprobs(payload.get("logprobs"), None);
    let avg_logprob = finite_number(local.get("avg_logprob"))
        .or_else(|| finite_number(first_segment.get("avg_logprob")));
    let response_keys = payload
        .as_object()
        .map(|map| {
            let mut keys = map.keys().cloned().collect::<Vec<_>>();
            keys.sort();
            keys.into_iter().map(Value::String).collect::<Vec<_>>()
        })
        .unwrap_or_default();
    TranscriptionResult {
        text,
        metadata: serde_json::json!({
            "provider": "local",
            "model": stt_model(),
            "response_format": stt_response_format(),
            "avg_logprob": avg_logprob,
            "compression_ratio": finite_number(first_segment.get("compression_ratio")),
            "no_speech_prob": finite_number(first_segment.get("no_speech_prob")),
            "local": {
                "avg_logprob": finite_number(local.get("avg_logprob")),
                "audio_rms": finite_number(local.get("audio_rms")),
                "audio_peak": finite_number(local.get("audio_peak")),
                "estimated_no_speech_prob": finite_number(local.get("estimated_no_speech_prob"))
            },
            "tokens": token_stats,
            "token_logprobs": token_logprobs,
            "response_keys": response_keys
        }),
    }
}

pub fn parse_stt_response(response: reqwest::blocking::Response) -> Result<TranscriptionResult> {
    let response = response.error_for_status()?;
    let payload: Value = response.json()?;
    if !payload.is_object() {
        return Ok(TranscriptionResult {
            text: String::new(),
            metadata: serde_json::json!({
                "provider": "local",
                "model": stt_model(),
                "invalid_payload_type": type_name(&payload)
            }),
        });
    }
    Ok(parse_stt_payload(&payload))
}

pub fn transcribe_fileobj_result_sync(
    bytes: Vec<u8>,
    filename: &str,
    content_type: &str,
) -> Result<TranscriptionResult> {
    let mut form = multipart::Form::new()
        .text("model", stt_model())
        .text("response_format", stt_response_format());
    let language = stt_language();
    if !language.is_empty() {
        form = form.text("language", language);
    }
    if stt_include_logprobs() && stt_response_format() == "json" {
        form = form.text("include[]", "logprobs");
    }
    let part = multipart::Part::bytes(bytes)
        .file_name(filename.to_string())
        .mime_str(content_type)?;
    form = form.part("file", part);
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(stt_timeout_seconds()))
        .build()?;
    let mut request = client.post(stt_transcriptions_url()?).multipart(form);
    let api_key = stt_api_key();
    if !api_key.is_empty() {
        request = request.bearer_auth(api_key);
    }
    parse_stt_response(request.send()?)
}

pub fn transcribe_file_result_sync(path: &Path) -> Result<TranscriptionResult> {
    let bytes =
        std::fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    transcribe_fileobj_result_sync(
        bytes,
        &path.file_name().unwrap_or_default().to_string_lossy(),
        &content_type_for_path(path),
    )
}

fn round4(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}

fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

trait IfEmpty {
    fn if_empty(self, fallback: &str) -> String;
}

impl IfEmpty for String {
    fn if_empty(self, fallback: &str) -> String {
        if self.trim().is_empty() {
            fallback.to_string()
        } else {
            self
        }
    }
}
