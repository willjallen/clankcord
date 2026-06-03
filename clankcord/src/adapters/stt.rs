use std::path::Path;

use anyhow::Context;
use reqwest::StatusCode;
use reqwest::blocking::multipart;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value};

use crate::Result;
use crate::config::{self, NamedTranscriptionSourceConfig, TranscriptionProvider};
use crate::runtime::util::{finite_number, number_or_null};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TranscriptionResult {
    pub text: String,
    pub metadata: Value,
    pub words: Vec<TranscriptionWord>,
    pub segments: Vec<TranscriptionSpan>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TranscriptionWord {
    pub text: String,
    pub start_seconds: Option<f64>,
    pub end_seconds: Option<f64>,
    pub speaker_id: String,
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TranscriptionSpan {
    pub text: String,
    pub start_seconds: Option<f64>,
    pub end_seconds: Option<f64>,
    pub speaker_id: String,
}

#[derive(Debug, Clone)]
pub struct SttHttpStatusError {
    provider: &'static str,
    status: StatusCode,
    body: String,
}

impl SttHttpStatusError {
    fn new(provider: &'static str, status: StatusCode, body: String) -> Self {
        Self {
            provider,
            status,
            body,
        }
    }

    pub fn status(&self) -> StatusCode {
        self.status
    }
}

impl std::fmt::Display for SttHttpStatusError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{} speech-to-text HTTP {}: {}",
            self.provider, self.status, self.body
        )
    }
}

impl std::error::Error for SttHttpStatusError {}

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
    let active_source = config::active_transcription_source().ok();
    let no_speech_cutoff = no_speech_threshold.unwrap_or_else(|| {
        active_source
            .as_ref()
            .map(|source| source.config.drop_no_speech_probability)
            .unwrap_or(0.7)
    });
    let token_cutoff = avg_token_logprob_threshold.unwrap_or_else(|| {
        active_source
            .as_ref()
            .map(|source| source.config.drop_avg_token_logprob)
            .unwrap_or(-0.8)
    });
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
    let max_entries = limit.unwrap_or(64);
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
    parse_openai_compatible_payload(
        payload,
        "local-granite",
        "local-granite",
        "verbose_json",
        64,
    )
}

pub fn transcribe_file_result_sync(path: &Path) -> Result<TranscriptionResult> {
    let source = config::active_transcription_source()?;
    transcribe_file_with_source_result_sync(path, &source)
}

pub fn transcribe_file_with_source_result_sync(
    path: &Path,
    source: &NamedTranscriptionSourceConfig,
) -> Result<TranscriptionResult> {
    let bytes =
        std::fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    transcribe_fileobj_with_source_result_sync(
        bytes,
        &path.file_name().unwrap_or_default().to_string_lossy(),
        &content_type_for_path(path),
        source,
    )
}

pub fn transcribe_fileobj_with_source_result_sync(
    bytes: Vec<u8>,
    filename: &str,
    content_type: &str,
    source: &NamedTranscriptionSourceConfig,
) -> Result<TranscriptionResult> {
    match source.config.provider {
        TranscriptionProvider::OpenaiCompatible => {
            transcribe_openai_compatible(bytes, filename, content_type, source)
        }
        TranscriptionProvider::Elevenlabs => {
            transcribe_elevenlabs(bytes, filename, content_type, source)
        }
    }
}

fn transcribe_openai_compatible(
    bytes: Vec<u8>,
    filename: &str,
    content_type: &str,
    source: &NamedTranscriptionSourceConfig,
) -> Result<TranscriptionResult> {
    let config = &source.config;
    let mut form = multipart::Form::new()
        .text("model", config.model.trim().to_string())
        .text("response_format", config.response_format.trim().to_string());
    let language = config.language.trim();
    if !language.is_empty() {
        form = form.text("language", language.to_string());
    }
    if config.response_format.trim() == "verbose_json" {
        form = form.text(
            "timestamp_granularities[]",
            config.timestamp_granularity.trim().to_string(),
        );
    }
    if config.include_logprobs {
        form = form.text("include[]", "logprobs");
    }
    let part = multipart::Part::bytes(bytes)
        .file_name(filename.to_string())
        .mime_str(content_type)?;
    form = form.part("file", part);
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(config.timeout_seconds))
        .build()?;
    let mut request = client
        .post(openai_transcriptions_url(source)?)
        .multipart(form);
    let api_key = config::transcription_source_api_key(config)?;
    if !api_key.is_empty() {
        request = request.bearer_auth(api_key);
    }
    let response = request.send()?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().unwrap_or_default();
        return Err(SttHttpStatusError::new("openai-compatible", status, body).into());
    }
    let payload: Value = response.json()?;
    if !payload.is_object() {
        return Ok(invalid_payload_result(
            source,
            type_name(&payload),
            "openai_compatible",
        ));
    }
    Ok(parse_openai_compatible_payload(
        &payload,
        &source.id,
        &config.model,
        &config.response_format,
        config.max_token_logprobs,
    ))
}

fn transcribe_elevenlabs(
    bytes: Vec<u8>,
    filename: &str,
    content_type: &str,
    source: &NamedTranscriptionSourceConfig,
) -> Result<TranscriptionResult> {
    let config = &source.config;
    let mut form = multipart::Form::new()
        .text("model_id", config.model.trim().to_string())
        .text(
            "timestamps_granularity",
            config.timestamp_granularity.trim().to_string(),
        )
        .text("diarize", config.diarize.to_string());
    let language = config.language.trim();
    if !language.is_empty() {
        form = form.text("language_code", language.to_string());
    }
    let part = multipart::Part::bytes(bytes)
        .file_name(filename.to_string())
        .mime_str(content_type)?;
    form = form.part("file", part);
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(config.timeout_seconds))
        .build()?;
    let mut request = client
        .post(config.base_url.trim().trim_end_matches('/'))
        .multipart(form);
    let api_key = config::transcription_source_api_key(config)?;
    if !api_key.is_empty() {
        request = request.header("xi-api-key", api_key);
    }
    let response = request.send()?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().unwrap_or_default();
        return Err(SttHttpStatusError::new("elevenlabs", status, body).into());
    }
    let payload: Value = response.json()?;
    if !payload.is_object() {
        return Ok(invalid_payload_result(
            source,
            type_name(&payload),
            "elevenlabs",
        ));
    }
    Ok(parse_elevenlabs_payload(&payload, source))
}

fn openai_transcriptions_url(source: &NamedTranscriptionSourceConfig) -> Result<String> {
    let base_url = source.config.base_url.trim().trim_end_matches('/');
    if base_url.is_empty() {
        anyhow::bail!(
            "config.toml transcription source `{}` base_url is not set",
            source.id
        );
    }
    if base_url.ends_with("/audio/transcriptions") {
        Ok(base_url.to_string())
    } else {
        Ok(format!("{base_url}/audio/transcriptions"))
    }
}

fn parse_openai_compatible_payload(
    payload: &Value,
    source_id: &str,
    model: &str,
    response_format: &str,
    max_token_logprobs: usize,
) -> TranscriptionResult {
    let text = payload_text(payload);
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
    let (token_logprobs, token_stats) =
        compact_token_logprobs(payload.get("logprobs"), Some(max_token_logprobs));
    let avg_logprob = finite_number(local.get("avg_logprob"))
        .or_else(|| finite_number(first_segment.get("avg_logprob")));
    let response_keys = response_keys(payload);
    let words = parse_words(payload.get("words"));
    let segments = parse_segments(payload.get("segments"));
    let timestamp_granularity = if !words.is_empty() {
        "word"
    } else if !segments.is_empty() {
        "segment"
    } else {
        ""
    };
    TranscriptionResult {
        text,
        words,
        segments,
        metadata: serde_json::json!({
            "provider": "openai_compatible",
            "transcription_source_id": source_id,
            "model": model,
            "response_format": response_format,
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
            "response_keys": response_keys,
            "timestamp_granularity": timestamp_granularity,
        }),
    }
}

fn parse_elevenlabs_payload(
    payload: &Value,
    source: &NamedTranscriptionSourceConfig,
) -> TranscriptionResult {
    let words = parse_words(payload.get("words"));
    let segments = words_to_segments(&words);
    TranscriptionResult {
        text: payload_text(payload),
        metadata: serde_json::json!({
            "provider": "elevenlabs",
            "transcription_source_id": source.id,
            "model": source.config.model,
            "language_code": payload.get("language_code").and_then(Value::as_str).unwrap_or(""),
            "language_probability": finite_number(payload.get("language_probability")),
            "timestamp_granularity": source.config.timestamp_granularity,
            "response_keys": response_keys(payload),
        }),
        words,
        segments,
    }
}

fn invalid_payload_result(
    source: &NamedTranscriptionSourceConfig,
    invalid_payload_type: &str,
    provider: &str,
) -> TranscriptionResult {
    TranscriptionResult {
        text: String::new(),
        words: Vec::new(),
        segments: Vec::new(),
        metadata: serde_json::json!({
            "provider": provider,
            "transcription_source_id": source.id,
            "model": source.config.model,
            "invalid_payload_type": invalid_payload_type
        }),
    }
}

fn payload_text(payload: &Value) -> String {
    payload
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string()
}

fn parse_words(value: Option<&Value>) -> Vec<TranscriptionWord> {
    value
        .and_then(Value::as_array)
        .map(|words| {
            words
                .iter()
                .filter_map(|word| {
                    let object = word.as_object()?;
                    Some(TranscriptionWord {
                        text: object
                            .get("word")
                            .or_else(|| object.get("text"))
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        start_seconds: finite_number(object.get("start")),
                        end_seconds: finite_number(object.get("end")),
                        speaker_id: object
                            .get("speaker")
                            .or_else(|| object.get("speaker_id"))
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        kind: object
                            .get("type")
                            .or_else(|| object.get("kind"))
                            .and_then(Value::as_str)
                            .unwrap_or("word")
                            .to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn parse_segments(value: Option<&Value>) -> Vec<TranscriptionSpan> {
    value
        .and_then(Value::as_array)
        .map(|segments| {
            segments
                .iter()
                .filter_map(|segment| {
                    let object = segment.as_object()?;
                    Some(TranscriptionSpan {
                        text: object
                            .get("text")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .trim()
                            .to_string(),
                        start_seconds: finite_number(object.get("start")),
                        end_seconds: finite_number(object.get("end")),
                        speaker_id: object
                            .get("speaker")
                            .or_else(|| object.get("speaker_id"))
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn words_to_segments(words: &[TranscriptionWord]) -> Vec<TranscriptionSpan> {
    let mut segments = Vec::new();
    let mut text = String::new();
    let mut start_seconds = None;
    let mut end_seconds = None;
    let mut speaker_id = String::new();
    for word in words {
        if word.kind == "audio_event" {
            continue;
        }
        if start_seconds.is_none() {
            start_seconds = word.start_seconds;
        }
        if word.end_seconds.is_some() {
            end_seconds = word.end_seconds;
        }
        if speaker_id.is_empty() {
            speaker_id = word.speaker_id.clone();
        }
        append_word_text(&mut text, &word.text);
    }
    if !text.trim().is_empty() {
        segments.push(TranscriptionSpan {
            text: text.trim().to_string(),
            start_seconds,
            end_seconds,
            speaker_id,
        });
    }
    segments
}

fn append_word_text(target: &mut String, text: &str) {
    if text.is_empty() {
        return;
    }
    if target.is_empty()
        || text.starts_with(char::is_whitespace)
        || matches!(text, "." | "," | "!" | "?" | ":" | ";" | ")" | "]")
    {
        target.push_str(text);
    } else {
        target.push(' ');
        target.push_str(text);
    }
}

fn response_keys(payload: &Value) -> Vec<Value> {
    payload
        .as_object()
        .map(|map| {
            let mut keys = map.keys().cloned().collect::<Vec<_>>();
            keys.sort();
            keys.into_iter().map(Value::String).collect::<Vec<_>>()
        })
        .unwrap_or_default()
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
