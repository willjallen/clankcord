use std::path::Path;

use anyhow::Context;
use reqwest::blocking::multipart;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::Result;
use crate::adapters::stt::content_type_for_path;
use crate::config;
use crate::runtime::util::{finite_number, number_or_null, string_field};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WakeDetectionResult {
    pub wake: bool,
    pub score: Option<f64>,
    pub threshold: Option<f64>,
    pub model_label: String,
    pub stream_id: String,
    pub processed_frames: Option<u64>,
    pub scores: Value,
    pub metadata: Value,
}

impl WakeDetectionResult {
    pub fn to_json(&self) -> Value {
        let mut object = match self.metadata.as_object() {
            Some(object) => object.clone(),
            None => Map::new(),
        };
        object.insert("wake".to_string(), Value::Bool(self.wake));
        object.insert("score".to_string(), number_or_null(self.score));
        object.insert("threshold".to_string(), number_or_null(self.threshold));
        object.insert(
            "model_label".to_string(),
            Value::String(self.model_label.clone()),
        );
        object.insert(
            "stream_id".to_string(),
            Value::String(self.stream_id.clone()),
        );
        object.insert(
            "processed_frames".to_string(),
            self.processed_frames
                .map(serde_json::Number::from)
                .map(Value::Number)
                .unwrap_or(Value::Null),
        );
        object.insert("scores".to_string(), self.scores.clone());
        Value::Object(object)
    }
}

pub fn wake_url() -> Result<String> {
    config::wake_url()
}

pub fn wake_timeout_seconds() -> u64 {
    config::wake_timeout_seconds()
}

pub fn wake_api_key() -> Result<String> {
    config::wake_api_key()
}

pub fn parse_wake_payload(payload: &Value) -> WakeDetectionResult {
    let scores = payload
        .get("scores")
        .filter(|value| value.is_object())
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));
    WakeDetectionResult {
        wake: payload
            .get("wake")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        score: finite_number(payload.get("score")),
        threshold: finite_number(payload.get("threshold")),
        model_label: string_field(payload, "model_label"),
        stream_id: string_field(payload, "stream_id"),
        processed_frames: payload.get("processed_frames").and_then(Value::as_u64),
        scores,
        metadata: payload.clone(),
    }
}

pub fn parse_wake_response(response: reqwest::blocking::Response) -> Result<WakeDetectionResult> {
    let response = response.error_for_status()?;
    let payload = response.json::<Value>()?;
    Ok(parse_wake_payload(&payload))
}

pub fn detect_wake_file_sync(
    path: &Path,
    stream_id: &str,
    reset: bool,
) -> Result<WakeDetectionResult> {
    let bytes =
        std::fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    detect_wake_fileobj_sync(
        bytes,
        &path.file_name().unwrap_or_default().to_string_lossy(),
        &content_type_for_path(path),
        stream_id,
        reset,
    )
}

pub fn detect_wake_fileobj_sync(
    bytes: Vec<u8>,
    filename: &str,
    content_type: &str,
    stream_id: &str,
    reset: bool,
) -> Result<WakeDetectionResult> {
    let part = multipart::Part::bytes(bytes)
        .file_name(filename.to_string())
        .mime_str(content_type)?;
    let form = multipart::Form::new()
        .text("stream_id", stream_id.to_string())
        .text("reset", reset.to_string())
        .part("file", part);
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(wake_timeout_seconds()))
        .build()?;
    let mut request = client.post(wake_url()?).multipart(form);
    let api_key = wake_api_key()?;
    if !api_key.is_empty() {
        request = request.bearer_auth(api_key);
    }
    parse_wake_response(request.send()?)
}
