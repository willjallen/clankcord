use std::path::Path;
use std::sync::{Mutex, OnceLock};

use anyhow::Context;
use chrono::{DateTime, SecondsFormat, Utc};
use reqwest::blocking::multipart;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeCircuitAdmission {
    Closed,
    HalfOpen,
}

#[derive(Debug, Default)]
struct WakeCircuitState {
    consecutive_failures: u32,
    open_count: u32,
    open_until: Option<DateTime<Utc>>,
    half_open_probe_in_flight: bool,
    last_failure_at: Option<DateTime<Utc>>,
    last_success_at: Option<DateTime<Utc>>,
    last_error: String,
    suppressed_probes: u64,
}

#[derive(Debug)]
pub struct WakeCircuitBreaker {
    failure_threshold: u32,
    open_initial_seconds: u64,
    open_max_seconds: u64,
    state: Mutex<WakeCircuitState>,
}

impl WakeCircuitBreaker {
    pub fn new(failure_threshold: u32, open_initial_seconds: u64, open_max_seconds: u64) -> Self {
        let open_initial_seconds = open_initial_seconds.max(1);
        Self {
            failure_threshold: failure_threshold.max(1),
            open_initial_seconds,
            open_max_seconds: open_max_seconds.max(open_initial_seconds),
            state: Mutex::new(WakeCircuitState::default()),
        }
    }

    pub fn submission_suppressed(&self, now: DateTime<Utc>) -> bool {
        let mut state = self.state.lock().expect("wake circuit mutex poisoned");
        let suppressed = state.half_open_probe_in_flight
            || state.open_until.is_some_and(|open_until| now < open_until);
        if suppressed {
            state.suppressed_probes = state.suppressed_probes.saturating_add(1);
        }
        suppressed
    }

    pub fn admit(&self, now: DateTime<Utc>) -> Option<WakeCircuitAdmission> {
        let mut state = self.state.lock().expect("wake circuit mutex poisoned");
        let Some(open_until) = state.open_until else {
            return Some(WakeCircuitAdmission::Closed);
        };
        if now < open_until || state.half_open_probe_in_flight {
            state.suppressed_probes = state.suppressed_probes.saturating_add(1);
            return None;
        }
        state.half_open_probe_in_flight = true;
        Some(WakeCircuitAdmission::HalfOpen)
    }

    pub fn record_success(&self, now: DateTime<Utc>) {
        let mut state = self.state.lock().expect("wake circuit mutex poisoned");
        state.consecutive_failures = 0;
        state.open_count = 0;
        state.open_until = None;
        state.half_open_probe_in_flight = false;
        state.last_success_at = Some(now);
        state.last_error.clear();
    }

    pub fn record_failure(&self, now: DateTime<Utc>, error: &str) {
        let mut state = self.state.lock().expect("wake circuit mutex poisoned");
        state.last_failure_at = Some(now);
        state.last_error = sanitize_provider_error(error);
        state.consecutive_failures = state.consecutive_failures.saturating_add(1);
        if state.half_open_probe_in_flight {
            state.half_open_probe_in_flight = false;
            state.open_count = state.open_count.saturating_add(1).max(1);
            open_circuit(self, &mut state, now);
            return;
        }
        if state.open_until.is_some_and(|open_until| now < open_until) {
            return;
        }
        if state.consecutive_failures >= self.failure_threshold {
            state.open_count = state.open_count.max(1);
            open_circuit(self, &mut state, now);
        }
    }

    pub fn snapshot(&self, now: DateTime<Utc>) -> Value {
        let state = self.state.lock().expect("wake circuit mutex poisoned");
        let status = if state.open_until.is_none() {
            "closed"
        } else if state.open_until.is_some_and(|open_until| now < open_until) {
            "open"
        } else {
            "half_open"
        };
        json!({
            "status": status,
            "available": status == "closed",
            "consecutiveFailures": state.consecutive_failures,
            "failureThreshold": self.failure_threshold,
            "openCount": state.open_count,
            "nextProbeAt": state.open_until.map(format_instant).unwrap_or_default(),
            "halfOpenProbeInFlight": state.half_open_probe_in_flight,
            "lastFailureAt": state.last_failure_at.map(format_instant).unwrap_or_default(),
            "lastSuccessAt": state.last_success_at.map(format_instant).unwrap_or_default(),
            "lastError": state.last_error,
            "suppressedProbes": state.suppressed_probes,
        })
    }
}

fn open_circuit(circuit: &WakeCircuitBreaker, state: &mut WakeCircuitState, now: DateTime<Utc>) {
    let exponent = state.open_count.saturating_sub(1).min(30);
    let seconds = circuit
        .open_initial_seconds
        .saturating_mul(2_u64.saturating_pow(exponent))
        .min(circuit.open_max_seconds);
    state.open_until = Some(now + chrono::Duration::seconds(seconds as i64));
}

fn format_instant(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn sanitize_provider_error(error: &str) -> String {
    error
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(240)
        .collect()
}

fn wake_provider_circuit() -> &'static WakeCircuitBreaker {
    static CIRCUIT: OnceLock<WakeCircuitBreaker> = OnceLock::new();
    CIRCUIT.get_or_init(|| {
        WakeCircuitBreaker::new(
            config::wake_circuit_failure_threshold(),
            config::wake_circuit_open_initial_seconds(),
            config::wake_circuit_open_max_seconds(),
        )
    })
}


pub(crate) fn acquire_wake_probe_admission() -> Option<WakeCircuitAdmission> {
    wake_provider_circuit().admit(Utc::now())
}

pub(crate) fn record_wake_provider_success() {
    wake_provider_circuit().record_success(Utc::now());
}

pub(crate) fn record_wake_provider_failure(error: &anyhow::Error) {
    wake_provider_circuit().record_failure(Utc::now(), &error.to_string());
}

pub fn wake_provider_health() -> Value {
    wake_provider_circuit().snapshot(Utc::now())
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
        .connect_timeout(std::time::Duration::from_secs(
            config::wake_connect_timeout_seconds(),
        ))
        .timeout(std::time::Duration::from_secs(wake_timeout_seconds()))
        .build()?;
    let mut request = client.post(wake_url()?).multipart(form);
    let api_key = wake_api_key()?;
    if !api_key.is_empty() {
        request = request.bearer_auth(api_key);
    }
    parse_wake_response(request.send()?)
}
