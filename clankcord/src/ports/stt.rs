//! Speech-to-text boundary types. The STT adapter produces these; domain
//! transcription consumes them. Acceptance policy (drop thresholds) lives
//! in `domain::transcription`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

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
