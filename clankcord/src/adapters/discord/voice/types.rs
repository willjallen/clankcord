use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

use crate::adapters::discord::voice::diagnostics::default_packet_debug;
use crate::config::format_timestamp_local;
use crate::runtime::{
    ArtifactStatus, RoomConfig, RuntimeSessionStatus, SessionArtifacts, SessionCaptureStats,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SpeakerBuffer {
    pub user_id: String,
    pub label: String,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub pcm: Vec<u8>,
    pub started_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_packet_monotonic: f64,
    #[serde(default)]
    pub active: bool,
    #[serde(default)]
    pub flush_in_flight: bool,
}

impl SpeakerBuffer {
    pub fn new(
        user_id: impl Into<String>,
        label: impl Into<String>,
        username: impl Into<String>,
    ) -> Self {
        Self {
            user_id: user_id.into(),
            label: label.into(),
            username: username.into(),
            pcm: Vec::new(),
            started_at: None,
            last_packet_monotonic: 0.0,
            active: false,
            flush_in_flight: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionAudioSegment {
    pub segment_index: i64,
    pub speaker_id: String,
    pub label: String,
    pub username: String,
    pub started_at: DateTime<Utc>,
    pub ended_at: DateTime<Utc>,
    pub wav_path: PathBuf,
    pub duration_ms: i64,
    #[serde(default)]
    pub event_id: String,
    #[serde(default)]
    pub audio_checksum: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VoiceSession {
    pub session_id: String,
    pub room: RoomConfig,
    pub bot_id: String,
    pub bot_user_id: String,
    pub thread_id: String,
    pub thread_name: String,
    pub started_at: DateTime<Utc>,
    pub session_dir: PathBuf,
    #[serde(default)]
    pub minute_message_ids: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub participants: BTreeMap<String, BTreeMap<String, String>>,
    #[serde(default)]
    pub buffers: BTreeMap<String, SpeakerBuffer>,
    #[serde(default = "default_packet_debug")]
    pub packet_debug: BTreeMap<String, i64>,
    #[serde(default)]
    pub debug_notes: BTreeMap<String, String>,
    #[serde(default)]
    pub segment_counter: i64,
    #[serde(default)]
    pub audio_segments: Vec<SessionAudioSegment>,
    #[serde(default)]
    pub transcription_task_ids: BTreeSet<String>,
    #[serde(default)]
    pub finalizing: bool,
    pub ended_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub voice_channel_id: String,
    #[serde(default)]
    pub transcript_event_count: i64,
    pub last_pcm_at: Option<DateTime<Utc>>,
    pub last_transcript_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_pcm_monotonic: f64,
    #[serde(default)]
    pub last_transcript_monotonic: f64,
    #[serde(default)]
    pub last_stall_log_monotonic: f64,
    #[serde(default)]
    pub voice_client_debug: BTreeMap<String, String>,
    #[serde(default)]
    pub capture_run_id: String,
    #[serde(default)]
    pub assignment_id: String,
    #[serde(default = "default_session_mode")]
    pub mode: String,
}

impl VoiceSession {
    pub fn metadata(&self, tz: Tz) -> RuntimeSessionStatus {
        let started = format_timestamp_local(self.started_at, tz);
        let ended = self.ended_at.map(|value| format_timestamp_local(value, tz));
        let recording_path = self.session_dir.join("recording.mp3");
        let transcript_path = self.session_dir.join("transcript.txt");
        let last_pcm = self
            .last_pcm_at
            .map(|value| format_timestamp_local(value, tz));
        let last_transcript = self
            .last_transcript_at
            .map(|value| format_timestamp_local(value, tz));
        RuntimeSessionStatus {
            session_id: self.session_id.clone(),
            room_id: self.room.room_id.clone(),
            guild_id: self.room.guild_id.clone(),
            guild_slug: self.room.guild_slug.clone(),
            channel_id: self.room.channel_id.clone(),
            channel_slug: self.room.channel_slug.clone(),
            channel_name: self.room.channel_name.clone(),
            bot_id: self.bot_id.clone(),
            bot_user_id: self.bot_user_id.clone(),
            voice_channel_id: if self.voice_channel_id.is_empty() {
                self.room.channel_id.clone()
            } else {
                self.voice_channel_id.clone()
            },
            thread_id: self.thread_id.clone(),
            thread_name: self.thread_name.clone(),
            capture_run_id: if self.capture_run_id.is_empty() {
                self.session_id.clone()
            } else {
                self.capture_run_id.clone()
            },
            assignment_id: self.assignment_id.clone(),
            mode: self.mode.clone(),
            started_at: timestamp_field(&started, "iso"),
            started_at_local: timestamp_field(&started, "local_iso"),
            started_at_discord: timestamp_field(&started, "discord_full"),
            started_at_relative: timestamp_field(&started, "discord_relative"),
            ended_at: timestamp_field_opt(ended.as_ref(), "iso"),
            ended_at_local: timestamp_field_opt(ended.as_ref(), "local_iso"),
            ended_at_discord: timestamp_field_opt(ended.as_ref(), "discord_full"),
            ended_at_relative: timestamp_field_opt(ended.as_ref(), "discord_relative"),
            participants: self.participants.clone(),
            active: self.ended_at.is_none() && !self.finalizing,
            finalizing: self.finalizing,
            capture_stats: SessionCaptureStats {
                audio_segments: self.audio_segments.len(),
                transcript_events: self.transcript_event_count,
                last_pcm_at: timestamp_field_opt(last_pcm.as_ref(), "iso"),
                last_pcm_at_local: timestamp_field_opt(last_pcm.as_ref(), "local_iso"),
                last_transcript_at: timestamp_field_opt(last_transcript.as_ref(), "iso"),
                last_transcript_at_local: timestamp_field_opt(
                    last_transcript.as_ref(),
                    "local_iso",
                ),
            },
            artifacts: SessionArtifacts {
                recording_mp3: artifact_status(recording_path),
                transcript_txt: artifact_status(transcript_path),
            },
        }
    }
}

fn artifact_status(path: PathBuf) -> ArtifactStatus {
    let metadata = std::fs::metadata(&path).ok();
    ArtifactStatus {
        path: path.display().to_string(),
        exists: metadata.is_some(),
        bytes: metadata.map(|value| value.len()).unwrap_or(0),
    }
}

fn timestamp_field(fields: &BTreeMap<String, String>, key: &str) -> String {
    fields.get(key).cloned().unwrap_or_default()
}

fn timestamp_field_opt(fields: Option<&BTreeMap<String, String>>, key: &str) -> String {
    fields
        .and_then(|value| value.get(key))
        .cloned()
        .unwrap_or_default()
}

fn default_session_mode() -> String {
    "local_buffering".to_string()
}
