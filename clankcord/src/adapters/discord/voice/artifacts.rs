use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};

use crate::Result;
use crate::config::slugify;
use crate::runtime::util::first_non_empty;

pub const PCM_SAMPLE_RATE: u32 = 48_000;
pub const PCM_CHANNELS: u16 = 2;
pub const PCM_SAMPLE_WIDTH: u16 = 2;
pub const PCM_20MS_FRAME_BYTES: usize =
    PCM_SAMPLE_RATE as usize * PCM_CHANNELS as usize * PCM_SAMPLE_WIDTH as usize / 50;
pub const PCM_20MS_SILENCE: [u8; PCM_20MS_FRAME_BYTES] = [0; PCM_20MS_FRAME_BYTES];
pub const WAKE_SAMPLE_RATE: u32 = 16_000;
pub const WAKE_CHANNELS: u16 = 1;

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
    let speaker_slug = speaker_slug(speaker_id, speaker_label);
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

pub fn write_wake_probe_wav(
    session_dir: &Path,
    speaker_id: &str,
    speaker_label: &str,
    probe_index: i64,
    started_at: DateTime<Utc>,
    pcm: &[u8],
) -> Result<SegmentAudioArtifact> {
    let wav_bytes = build_wake_wav_bytes(pcm)?;
    let speaker_slug = speaker_slug(speaker_id, speaker_label);
    let filename = format!(
        "{:06}-{}-{speaker_slug}.wav",
        probe_index,
        started_at.format("%Y%m%dT%H%M%S%6f")
    );
    let path = session_dir
        .join("wake-probes")
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
        sample_rate_hz: WAKE_SAMPLE_RATE,
        channels: WAKE_CHANNELS,
        sample_width_bits: PCM_SAMPLE_WIDTH * 8,
        post_processing: "pcm_s16le_48khz_stereo_to_wav_16khz_mono".to_string(),
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

pub fn build_wake_wav_bytes(pcm_bytes: &[u8]) -> Result<Vec<u8>> {
    let mut cursor = Cursor::new(Vec::<u8>::new());
    let spec = hound::WavSpec {
        channels: WAKE_CHANNELS,
        sample_rate: WAKE_SAMPLE_RATE,
        bits_per_sample: PCM_SAMPLE_WIDTH * 8,
        sample_format: hound::SampleFormat::Int,
    };
    {
        let mut writer = hound::WavWriter::new(&mut cursor, spec)?;
        for source_frame in pcm_bytes.chunks_exact(12) {
            let mut sum = 0i32;
            for stereo_frame in source_frame.chunks_exact(4) {
                let left = i16::from_le_bytes([stereo_frame[0], stereo_frame[1]]) as i32;
                let right = i16::from_le_bytes([stereo_frame[2], stereo_frame[3]]) as i32;
                sum += (left + right) / 2;
            }
            writer.write_sample((sum / 3).clamp(i16::MIN as i32, i16::MAX as i32) as i16)?;
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

fn speaker_slug(speaker_id: &str, speaker_label: &str) -> String {
    let label = first_non_empty([speaker_label.to_string(), speaker_id.to_string()]);
    first_non_empty([
        slugify(&label),
        first_non_empty([speaker_id.to_string(), "speaker".to_string()]),
    ])
}
