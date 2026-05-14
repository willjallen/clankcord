use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};

use crate::Result;
use crate::config::slugify;

pub const PCM_SAMPLE_RATE: u32 = 48_000;
pub const PCM_CHANNELS: u16 = 2;
pub const PCM_SAMPLE_WIDTH: u16 = 2;
pub const PCM_20MS_FRAME_BYTES: usize =
    PCM_SAMPLE_RATE as usize * PCM_CHANNELS as usize * PCM_SAMPLE_WIDTH as usize / 50;
pub const PCM_20MS_SILENCE: [u8; PCM_20MS_FRAME_BYTES] = [0; PCM_20MS_FRAME_BYTES];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentAudioArtifact {
    pub path: PathBuf,
    pub checksum: String,
    pub bytes: u64,
    pub format: String,
    pub sample_rate_hz: u32,
    pub channels: u16,
    pub sample_width_bits: u16,
    pub post_processing: String,
}

pub fn write_segment_wav(
    session_dir: &Path,
    speaker_id: &str,
    speaker_label: &str,
    segment_index: i64,
    started_at: DateTime<Utc>,
    pcm: &[u8],
) -> Result<SegmentAudioArtifact> {
    let wav_bytes = build_wav_bytes(pcm)?;
    let speaker_slug = non_empty(
        &slugify(&non_empty(speaker_label, speaker_id)),
        &non_empty(speaker_id, "speaker"),
    );
    let filename = format!(
        "{:06}-{}-{speaker_slug}.wav",
        segment_index,
        started_at.format("%Y%m%dT%H%M%S%6f")
    );
    let path = session_dir
        .join("segments")
        .join(&speaker_slug)
        .join(filename);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, &wav_bytes)?;
    Ok(SegmentAudioArtifact {
        checksum: sha256_bytes(&wav_bytes),
        bytes: wav_bytes.len() as u64,
        path,
        format: "wav".to_string(),
        sample_rate_hz: PCM_SAMPLE_RATE,
        channels: PCM_CHANNELS,
        sample_width_bits: PCM_SAMPLE_WIDTH * 8,
        post_processing: "pcm_s16le_to_wav".to_string(),
    })
}

pub fn build_wav_bytes(pcm_bytes: &[u8]) -> Result<Vec<u8>> {
    let mut cursor = Cursor::new(Vec::<u8>::new());
    let spec = hound::WavSpec {
        channels: PCM_CHANNELS,
        sample_rate: PCM_SAMPLE_RATE,
        bits_per_sample: PCM_SAMPLE_WIDTH * 8,
        sample_format: hound::SampleFormat::Int,
    };
    {
        let mut writer = hound::WavWriter::new(&mut cursor, spec)?;
        for chunk in pcm_bytes.chunks_exact(2) {
            writer.write_sample(i16::from_le_bytes([chunk[0], chunk[1]]))?;
        }
        writer.finalize()?;
    }
    Ok(cursor.into_inner())
}

pub fn duration_ms_for_pcm(pcm_bytes: &[u8]) -> i64 {
    let bytes_per_second =
        PCM_SAMPLE_RATE as usize * PCM_CHANNELS as usize * PCM_SAMPLE_WIDTH as usize;
    if bytes_per_second == 0 {
        return 0;
    }
    ((pcm_bytes.len() as f64 / bytes_per_second as f64) * 1000.0) as i64
}

fn sha256_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("sha256:{:x}", hasher.finalize())
}

fn non_empty(value: &str, fallback: &str) -> String {
    if value.trim().is_empty() {
        fallback.to_string()
    } else {
        value.trim().to_string()
    }
}
