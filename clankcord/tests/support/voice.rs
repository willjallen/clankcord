//! Voice-capture fixtures shared across category binaries.

use clankcord::adapters::discord::voice::artifacts::PCM_20MS_SILENCE;

pub fn pcm_frame(amplitude: i16) -> Vec<u8> {
    let mut pcm = Vec::with_capacity(PCM_20MS_SILENCE.len());
    let bytes = amplitude.to_le_bytes();
    for _ in 0..(PCM_20MS_SILENCE.len() / 4) {
        pcm.extend_from_slice(&bytes);
        pcm.extend_from_slice(&bytes);
    }
    pcm
}

pub fn pcm_ms(amplitude: i16, ms: usize) -> Vec<u8> {
    let frame = pcm_frame(amplitude);
    let frames = (ms / 20).max(1);
    let mut pcm = Vec::with_capacity(frame.len() * frames);
    for _ in 0..frames {
        pcm.extend_from_slice(&frame);
    }
    pcm
}

pub fn string_field(value: &serde_json::Value, key: &str) -> String {
    match value.get(key) {
        Some(serde_json::Value::String(text)) => text.trim().to_string(),
        Some(serde_json::Value::Number(number)) => number.to_string(),
        Some(serde_json::Value::Bool(boolean)) => boolean.to_string(),
        _ => String::new(),
    }
}
