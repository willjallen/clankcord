use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde_json::{Value, json};

use crate::Result;
use crate::adapters::discord::voice::artifacts::{
    PCM_20MS_SILENCE, duration_ms_for_pcm, write_segment_wav,
};
use crate::adapters::discord::voice::diagnostics::{DiagnosticsConfig, analyze_pcm_bytes};
use crate::adapters::discord::voice::types::{SessionAudioSegment, SpeakerBuffer, VoiceSession};
use crate::runtime::AudioSegmentPayload;

#[derive(Debug, Clone)]
pub struct SessionAudioPipeline {
    pub minimum_utterance_ms: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AudioPipelineOutcome {
    NoSession,
    Paused,
    Buffered,
    Ignored,
    SegmentTooShort {
        duration_ms: i64,
    },
    SegmentReady {
        payload: AudioSegmentPayload,
        segment: SessionAudioSegment,
    },
}

impl SessionAudioPipeline {
    pub fn new() -> Self {
        Self {
            minimum_utterance_ms: 350,
        }
    }

    pub fn with_minimum_utterance_ms(mut self, minimum_utterance_ms: i64) -> Self {
        self.minimum_utterance_ms = minimum_utterance_ms.max(0);
        self
    }

    pub fn handle_pcm_packet(
        &self,
        session: Option<&mut VoiceSession>,
        user_id: &str,
        label: &str,
        username: &str,
        pcm: &[u8],
    ) -> AudioPipelineOutcome {
        let Some(session) = active_session(session) else {
            return AudioPipelineOutcome::NoSession;
        };
        if session.mode == "deafened_paused" {
            note_packet_debug(session, "droppedPausedPcmPackets");
            return AudioPipelineOutcome::Paused;
        }
        let now = Utc::now();
        let now_monotonic = monotonic_seconds();
        let speaker = session
            .buffers
            .entry(user_id.to_string())
            .or_insert_with(|| SpeakerBuffer::new(user_id, label, username));
        if speaker.pcm.is_empty() {
            speaker.started_at = Some(now);
        }
        if !label.trim().is_empty() {
            speaker.label = label.to_string();
        }
        if !username.trim().is_empty() {
            speaker.username = username.to_string();
        }
        speaker.pcm.extend_from_slice(pcm);
        speaker.last_packet_monotonic = now_monotonic;
        speaker.active = true;
        session.participants.insert(
            user_id.to_string(),
            BTreeMap::from([
                ("label".to_string(), speaker.label.clone()),
                ("username".to_string(), speaker.username.clone()),
            ]),
        );
        session.last_pcm_at = Some(now);
        session.last_pcm_monotonic = now_monotonic;
        session.last_stall_log_monotonic = 0.0;
        AudioPipelineOutcome::Buffered
    }

    pub fn handle_silence_packet(
        &self,
        session: Option<&mut VoiceSession>,
        user_id: &str,
        label: &str,
        username: &str,
        pcm: &[u8],
    ) -> AudioPipelineOutcome {
        let Some(session) = active_session(session) else {
            return AudioPipelineOutcome::NoSession;
        };
        if session.mode == "deafened_paused" {
            note_packet_debug(session, "droppedPausedSilencePackets");
            return AudioPipelineOutcome::Paused;
        }
        let Some(speaker) = session.buffers.get_mut(user_id) else {
            return AudioPipelineOutcome::Ignored;
        };
        if speaker.pcm.is_empty() || speaker.flush_in_flight {
            return AudioPipelineOutcome::Ignored;
        }
        if !label.trim().is_empty() {
            speaker.label = label.to_string();
        }
        if !username.trim().is_empty() {
            speaker.username = username.to_string();
        }
        speaker.pcm.extend_from_slice(pcm);
        speaker.active = false;
        note_packet_debug(session, "preservedSilencePackets");
        AudioPipelineOutcome::Buffered
    }

    pub fn handle_empty_pcm_packet(
        &self,
        session: Option<&mut VoiceSession>,
        user_id: &str,
        label: &str,
        username: &str,
    ) -> AudioPipelineOutcome {
        let Some(session) = active_session(session) else {
            return AudioPipelineOutcome::NoSession;
        };
        if session.mode == "deafened_paused" {
            note_packet_debug(session, "droppedPausedEmptyPcmPackets");
            return AudioPipelineOutcome::Paused;
        }
        let Some(speaker) = session.buffers.get_mut(user_id) else {
            note_packet_debug(session, "droppedEmptyPcmPackets");
            return AudioPipelineOutcome::Ignored;
        };
        if speaker.pcm.is_empty() || speaker.flush_in_flight {
            note_packet_debug(session, "droppedEmptyPcmPackets");
            return AudioPipelineOutcome::Ignored;
        }
        let now = Utc::now();
        if !label.trim().is_empty() {
            speaker.label = label.to_string();
        }
        if !username.trim().is_empty() {
            speaker.username = username.to_string();
        }
        speaker.pcm.extend_from_slice(&PCM_20MS_SILENCE);
        speaker.last_packet_monotonic = monotonic_seconds();
        speaker.active = true;
        session.last_pcm_at = Some(now);
        session.last_pcm_monotonic = speaker.last_packet_monotonic;
        session.last_stall_log_monotonic = 0.0;
        note_packet_debug(session, "emptyPcmSilenceFrames");
        AudioPipelineOutcome::Buffered
    }

    pub fn handle_speaking_state(
        &self,
        session: Option<&mut VoiceSession>,
        user_id: &str,
        label: &str,
        username: &str,
        active: bool,
    ) -> AudioPipelineOutcome {
        let Some(session) = active_session(session) else {
            return AudioPipelineOutcome::NoSession;
        };
        if session.mode == "deafened_paused" {
            note_packet_debug(session, "droppedPausedSpeakingStates");
            return AudioPipelineOutcome::Paused;
        }
        let Some(speaker) = session.buffers.get_mut(user_id) else {
            if active {
                session.participants.insert(
                    user_id.to_string(),
                    BTreeMap::from([
                        ("label".to_string(), non_empty(label, user_id)),
                        ("username".to_string(), username.to_string()),
                    ]),
                );
            }
            return AudioPipelineOutcome::Ignored;
        };
        if !label.trim().is_empty() {
            speaker.label = label.to_string();
        }
        if !username.trim().is_empty() {
            speaker.username = username.to_string();
        }
        speaker.active = active;
        AudioPipelineOutcome::Buffered
    }

    pub fn close_speaker_segment(
        &self,
        session: &mut VoiceSession,
        user_id: &str,
    ) -> Result<AudioPipelineOutcome> {
        let Some(speaker) = session.buffers.get_mut(user_id) else {
            return Ok(AudioPipelineOutcome::Ignored);
        };
        if speaker.pcm.is_empty() || speaker.flush_in_flight {
            return Ok(AudioPipelineOutcome::Ignored);
        }
        speaker.flush_in_flight = true;
        let pcm = std::mem::take(&mut speaker.pcm);
        let started_at = speaker.started_at.unwrap_or_else(Utc::now);
        let ended_at = Utc::now();
        let label = non_empty(&speaker.label, user_id);
        let username = speaker.username.clone();
        speaker.started_at = None;
        speaker.active = false;
        speaker.flush_in_flight = false;
        let duration_ms = duration_ms_for_pcm(&pcm);
        if duration_ms < self.minimum_utterance_ms {
            return Ok(AudioPipelineOutcome::SegmentTooShort { duration_ms });
        }
        let segment_index = next_segment_index(session);
        self.capture_segment(
            session,
            segment_index,
            user_id,
            &label,
            &username,
            &pcm,
            started_at,
            ended_at,
        )
    }

    pub fn flush_speaker(
        &self,
        session: &mut VoiceSession,
        user_id: &str,
    ) -> Result<AudioPipelineOutcome> {
        self.close_speaker_segment(session, user_id)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn capture_segment(
        &self,
        session: &mut VoiceSession,
        segment_index: i64,
        speaker_id: &str,
        label: &str,
        username: &str,
        pcm: &[u8],
        started_at: DateTime<Utc>,
        ended_at: DateTime<Utc>,
    ) -> Result<AudioPipelineOutcome> {
        let duration_ms = duration_ms_for_pcm(pcm);
        let artifact = write_segment_wav(
            &session.session_dir,
            speaker_id,
            label,
            segment_index,
            started_at,
            pcm,
        )?;
        let segment = SessionAudioSegment {
            segment_index,
            speaker_id: speaker_id.to_string(),
            label: label.to_string(),
            username: username.to_string(),
            started_at,
            ended_at,
            wav_path: artifact.path.clone(),
            duration_ms,
            event_id: String::new(),
            audio_checksum: artifact.checksum.clone(),
        };
        session.audio_segments.push(segment.clone());
        let mut log_fields = json!({
                "segmentIndex": segment_index,
                "speakerId": speaker_id,
                "speakerLabel": label,
                "durationMs": duration_ms,
                "pcmBytes": pcm.len(),
                "sourceAudioPath": artifact.path.display().to_string(),
                "audioChecksum": artifact.checksum.clone(),
                "audioBytes": artifact.bytes,
                "audioFormat": artifact.format.clone(),
                "sampleRateHz": artifact.sample_rate_hz,
                "channels": artifact.channels,
                "postProcessing": artifact.post_processing.clone(),
        });
        if DiagnosticsConfig::from_env().audio_stats {
            merge_object(&mut log_fields, analyze_pcm_bytes(pcm));
        }
        note_session_log(session, "captured-segment", log_fields);
        let payload = AudioSegmentPayload {
            guild_id: session.room.guild_id.clone(),
            guild_slug: session.room.guild_slug.clone(),
            voice_channel_id: session.room.channel_id.clone(),
            voice_channel_name: session.room.channel_name.clone(),
            voice_channel_slug: session.room.channel_slug.clone(),
            capture_run_id: non_empty(&session.capture_run_id, &session.session_id),
            voice_bot_id: session.bot_id.clone(),
            voice_bot_discord_user_id: session.bot_user_id.clone(),
            speaker_user_id: speaker_id.to_string(),
            speaker_label: label.to_string(),
            speaker_username: username.to_string(),
            segment_start_time: started_at,
            segment_end_time: ended_at,
            segment_index,
            duration_ms,
            source_audio_path: artifact.path,
            audio_checksum: artifact.checksum,
            audio_bytes: artifact.bytes,
            audio_format: artifact.format,
            sample_rate_hz: artifact.sample_rate_hz,
            channels: artifact.channels,
            sample_width_bits: artifact.sample_width_bits,
            post_processing: artifact.post_processing,
        };
        Ok(AudioPipelineOutcome::SegmentReady { payload, segment })
    }
}

fn merge_object(target: &mut Value, extra: Value) {
    let (Value::Object(target), Value::Object(extra)) = (target, extra) else {
        return;
    };
    for (key, value) in extra {
        target.insert(key, value);
    }
}

fn active_session(session: Option<&mut VoiceSession>) -> Option<&mut VoiceSession> {
    session.filter(|session| session.ended_at.is_none() && !session.finalizing)
}

fn note_packet_debug(session: &mut VoiceSession, key: &str) {
    *session.packet_debug.entry(key.to_string()).or_insert(0) += 1;
}

fn next_segment_index(session: &mut VoiceSession) -> i64 {
    let segment_index = session.segment_counter;
    session.segment_counter += 1;
    segment_index
}

fn note_session_log(session: &mut VoiceSession, action: &str, fields: Value) {
    session.debug_notes.insert(
        format!("last_{action}"),
        serde_json::to_string(&fields).unwrap_or_default(),
    );
}

fn non_empty(value: &str, fallback: &str) -> String {
    if value.trim().is_empty() {
        fallback.to_string()
    } else {
        value.trim().to_string()
    }
}

pub(crate) fn monotonic_seconds() -> f64 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    START
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_secs_f64()
}
