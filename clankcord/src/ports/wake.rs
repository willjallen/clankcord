//! Wake-word seam.
//!
//! Adapters implement [`WakeDetector`] over a concrete provider; the wake
//! circuit breaker (policy in `runtime::domain::voice_capture::wake_circuit`,
//! state in the `wake_circuit` table) decides whether a probe runs at all.

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::Result;
use crate::runtime::util::number_or_null;

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

pub trait WakeDetector: Send + Sync {
    fn detect_file(
        &self,
        path: &Path,
        stream_id: &str,
        reset_stream: bool,
    ) -> Result<WakeDetectionResult>;
}
