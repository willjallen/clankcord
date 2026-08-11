//! Transcription acceptance policy.
//!
//! The STT adapter reports raw provider results including confidence
//! metadata; whether a transcription is kept or dropped is decided here,
//! from configured thresholds — provider transport stays policy-free.

pub mod execution;
pub mod mux;

use serde_json::Value;

use crate::config;
use crate::util::finite_number;

pub fn stt_no_speech_probability(metadata: Option<&Value>) -> Option<f64> {
    let Value::Object(map) = metadata? else {
        return None;
    };
    let mut probabilities = Vec::new();
    if let Some(value) = finite_number(map.get("no_speech_prob")) {
        probabilities.push(value);
    }
    if let Some(local) = map.get("local").and_then(Value::as_object)
        && let Some(value) = finite_number(local.get("estimated_no_speech_prob"))
    {
        probabilities.push(value);
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
