use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactStatus {
    pub path: String,
    pub exists: bool,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SessionArtifacts {
    pub recording_mp3: ArtifactStatus,
    pub transcript_txt: ArtifactStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SessionSpeakerCaptureStats {
    pub user_id: String,
    pub label: String,
    pub username: String,
    pub active: bool,
    pub buffered_audio_bytes: usize,
    pub flush_in_flight: bool,
    pub segment_started_at: String,
    pub last_pcm_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SessionCaptureStats {
    pub audio_segments: usize,
    pub transcript_events: i64,
    pub last_pcm_at: String,
    pub last_pcm_at_local: String,
    pub last_transcript_at: String,
    pub last_transcript_at_local: String,
    pub speakers: BTreeMap<String, SessionSpeakerCaptureStats>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeSessionStatus {
    pub session_id: String,
    pub room_id: String,
    pub guild_id: String,
    pub guild_slug: String,
    pub channel_id: String,
    pub channel_slug: String,
    pub channel_name: String,
    pub bot_id: String,
    pub bot_user_id: String,
    pub voice_channel_id: String,
    pub thread_id: String,
    pub thread_name: String,
    pub capture_run_id: String,
    pub assignment_id: String,
    pub mode: String,
    pub started_at: String,
    pub started_at_local: String,
    pub started_at_discord: String,
    pub started_at_relative: String,
    pub ended_at: String,
    pub ended_at_local: String,
    pub ended_at_discord: String,
    pub ended_at_relative: String,
    pub participants: BTreeMap<String, BTreeMap<String, String>>,
    pub active: bool,
    pub finalizing: bool,
    pub capture_stats: SessionCaptureStats,
    pub artifacts: SessionArtifacts,
}

impl RuntimeSessionStatus {
    pub fn mark_ended(&mut self, ended_at: String) {
        self.ended_at = ended_at;
        self.active = false;
        self.finalizing = false;
    }

    pub fn to_json(&self) -> Value {
        serde_json::to_value(self).unwrap_or_else(|_| json!({}))
    }
}
