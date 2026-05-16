use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct DiagnosticsConfig {
    pub enabled: bool,
    pub audio_stats: bool,
    pub receiver: bool,
    pub event_paths: bool,
}

impl DiagnosticsConfig {
    pub fn from_config() -> Self {
        let configured = config::voice_diagnostics_config();
        Self {
            enabled: configured.enabled,
            audio_stats: configured.audio_stats,
            receiver: configured.receiver,
            event_paths: configured.event_paths,
        }
    }
}

pub fn default_packet_debug() -> BTreeMap<String, i64> {
    [
        ("writeCalls", 0),
        ("syntheticPackets", 0),
        ("syntheticPcmPackets", 0),
        ("droppedSyntheticPcmPackets", 0),
        ("silencePackets", 0),
        ("preservedSilencePackets", 0),
        ("missingUserPackets", 0),
        ("emptyPcmPackets", 0),
        ("droppedEmptyPcmPackets", 0),
        ("emptyPcmSilenceFrames", 0),
        ("pcmPackets", 0),
        ("droppedPausedPcmPackets", 0),
        ("droppedPausedSilencePackets", 0),
        ("droppedPausedEmptyPcmPackets", 0),
        ("droppedPausedSpeakingStates", 0),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_string(), value))
    .collect()
}

pub fn dbfs(amplitude: f64) -> f64 {
    if amplitude <= 0.0 {
        -999.0
    } else {
        (20.0 * (amplitude / 32768.0).log10() * 10.0).round() / 10.0
    }
}

pub fn analyze_pcm_bytes(pcm: &[u8]) -> Value {
    if pcm.is_empty() {
        return empty_audio_stats();
    }
    let samples: Vec<i16> = pcm
        .chunks_exact(2)
        .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
        .collect();
    if samples.is_empty() {
        return empty_audio_stats();
    }
    let mut max_abs = 0i64;
    let mut clipped = 0i64;
    let mut near_clipped = 0i64;
    let mut square_sum = 0.0;
    for sample in samples {
        let value = (sample as i32).abs() as i64;
        max_abs = max_abs.max(value);
        if value >= 32760 {
            clipped += 1;
        }
        if value >= 30000 {
            near_clipped += 1;
        }
        square_sum += (sample as f64) * (sample as f64);
    }
    let rms = (square_sum / pcm.chunks_exact(2).len() as f64).sqrt();
    serde_json::json!({
        "rmsDbFS": dbfs(rms),
        "peakDbFS": dbfs(max_abs as f64),
        "maxAbsSample": max_abs,
        "clippedSamples": clipped,
        "nearClippedSamples": near_clipped
    })
}

pub fn compact_recv_diagnostics(diagnostics: Value) -> Value {
    let Value::Object(mut compact) = diagnostics else {
        return diagnostics;
    };
    for key in [
        "decode_err_samples",
        "dave_unhandled_samples",
        "non_audio_rtp_samples",
        "voice_ws_recent_events",
        "dave_ws_recent_events",
    ] {
        if let Some(Value::Array(values)) = compact.get_mut(key) {
            if values.len() > 10 {
                *values = values[values.len() - 10..].to_vec();
            }
        }
    }
    Value::Object(compact)
}

fn empty_audio_stats() -> Value {
    serde_json::json!({
        "rmsDbFS": -999.0,
        "peakDbFS": -999.0,
        "maxAbsSample": 0,
        "clippedSamples": 0,
        "nearClippedSamples": 0
    })
}
