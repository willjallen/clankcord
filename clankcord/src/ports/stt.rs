//! Speech-to-text seam.
//!
//! Adapters implement [`Transcriber`] over a concrete provider; domain code
//! consumes transcription results without knowing the transport. Acceptance
//! policy (drop thresholds) lives in `runtime::domain::transcription`.

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::Result;
use crate::config::NamedTranscriptionSourceConfig;

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

pub trait Transcriber: Send + Sync {
    fn transcribe_file(
        &self,
        path: &Path,
        source: &NamedTranscriptionSourceConfig,
    ) -> Result<TranscriptionResult>;
}
