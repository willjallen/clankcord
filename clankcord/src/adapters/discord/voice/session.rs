use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde_json::{Value, json};

use crate::Result;
use crate::adapters::discord::voice::artifacts::{
    PCM_20MS_FRAME_BYTES, PCM_20MS_SILENCE, PCM_CHANNELS, PCM_SAMPLE_RATE, PCM_SAMPLE_WIDTH,
    duration_ms_for_pcm, write_segment_wav, write_wake_probe_wav,
};
use crate::adapters::discord::voice::diagnostics::{DiagnosticsConfig, analyze_pcm_bytes};
use crate::adapters::discord::voice::types::{
    LiveVoiceSession, SessionAudioSegment, SpeakerBuffer,
};
use crate::runtime::util::first_non_empty;
use crate::runtime::{AudioSegmentPayload, WakeProbePayload};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WakeProbeConfig {
    pub minimum_ms: i64,
    pub window_ms: i64,
    pub interval_ms: i64,
}

impl WakeProbeConfig {
    pub fn enabled(self) -> bool {
        self.minimum_ms > 0 && self.window_ms > 0 && self.interval_ms > 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpeechGateConfig {
    pub rms_start_threshold: f64,
    pub rms_continue_threshold: f64,
    pub start_ms: i64,
    pub soft_break_ms: i64,
    pub end_silence_ms: i64,
    pub preroll_ms: i64,
}

impl SpeechGateConfig {
    pub fn conservative() -> Self {
        Self {
            rms_start_threshold: 0.006,
            rms_continue_threshold: 0.002,
            start_ms: 80,
            soft_break_ms: 400,
            end_silence_ms: 1400,
            preroll_ms: 200,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentCloseReason {
    EndSilence,
    PacketTimeout,
    MaxSegment,
    Disconnect,
    Finalize,
    ManualFlush,
}

impl SegmentCloseReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::EndSilence => "end_silence",
            Self::PacketTimeout => "packet_timeout",
            Self::MaxSegment => "max_segment",
            Self::Disconnect => "disconnect",
            Self::Finalize => "finalize",
            Self::ManualFlush => "manual_flush",
        }
    }
}

#[derive(Debug, Clone)]
pub struct SessionAudioPipeline {
    pub minimum_utterance_ms: i64,
    pub speech_gate: SpeechGateConfig,
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
            speech_gate: SpeechGateConfig::conservative(),
        }
    }

    pub fn with_minimum_utterance_ms(mut self, minimum_utterance_ms: i64) -> Self {
        self.minimum_utterance_ms = minimum_utterance_ms.max(0);
        self
    }

    pub fn with_speech_gate(mut self, speech_gate: SpeechGateConfig) -> Self {
        self.speech_gate = speech_gate;
        self
    }

    pub fn should_flush_speaker(
        &self,
        speaker: &SpeakerBuffer,
        max_segment_ms: i64,
        silence_ms: i64,
        now_monotonic: f64,
    ) -> Option<SegmentCloseReason> {
        if speaker.pcm.is_empty() || speaker.flush_in_flight {
            return None;
        }
        let buffered_duration_ms = duration_ms_for_pcm(&speaker.pcm);
        if buffered_duration_ms >= max_segment_ms {
            return Some(SegmentCloseReason::MaxSegment);
        }
        if speaker.stt_trailing_silence_ms >= self.speech_gate.end_silence_ms {
            return Some(SegmentCloseReason::EndSilence);
        }
        if now_monotonic - speaker.last_packet_monotonic >= silence_ms as f64 / 1000.0 {
            return Some(SegmentCloseReason::PacketTimeout);
        }
        None
    }

    fn ingest_stt_pcm(
        &self,
        speaker: &mut SpeakerBuffer,
        pcm: &[u8],
        packet_start: DateTime<Utc>,
    ) -> bool {
        if pcm.is_empty() {
            return false;
        }
        let mut accepted = false;
        let mut offset_ms = 0;
        for frame in pcm.chunks(PCM_20MS_FRAME_BYTES) {
            let frame_ms = duration_ms_for_pcm(frame).max(1);
            let frame_start = packet_start + chrono::Duration::milliseconds(offset_ms);
            let frame_end = frame_start + chrono::Duration::milliseconds(frame_ms);
            offset_ms += frame_ms;
            speaker.stt_input_ms += frame_ms;
            if speaker.pcm.is_empty() {
                append_preroll_frame(speaker, frame, frame_start, self.speech_gate.preroll_ms);
                let rms = normalized_rms(frame);
                if rms >= self.speech_gate.rms_start_threshold {
                    speaker.stt_voiced_ms += frame_ms;
                    if speaker.stt_voiced_ms >= self.speech_gate.start_ms {
                        speaker.started_at = speaker.stt_preroll_started_at.or(Some(frame_start));
                        speaker.pcm.extend_from_slice(&speaker.stt_preroll_pcm);
                        speaker.last_pcm_at = Some(frame_end);
                        speaker.stt_trailing_silence_ms = 0;
                        accepted = true;
                    }
                } else {
                    speaker.stt_voiced_ms = 0;
                }
                continue;
            }
            speaker.pcm.extend_from_slice(frame);
            let rms = normalized_rms(frame);
            if rms >= self.speech_gate.rms_continue_threshold {
                speaker.stt_trailing_silence_ms = 0;
                speaker.last_pcm_at = Some(frame_end);
            } else {
                speaker.stt_trailing_silence_ms += frame_ms;
                if speaker.stt_trailing_silence_ms >= self.speech_gate.soft_break_ms {
                    speaker.stt_soft_break_ms = speaker.stt_trailing_silence_ms;
                }
            }
            accepted = true;
        }
        accepted
    }

    pub fn handle_pcm_packet(
        &self,
        session: Option<&mut LiveVoiceSession>,
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
        let observed_end = Utc::now();
        let now_monotonic = monotonic_seconds();
        let speaker = session
            .buffers
            .entry(user_id.to_string())
            .or_insert_with(|| SpeakerBuffer::new(user_id, label, username));
        if !label.trim().is_empty() {
            speaker.label = label.to_string();
        }
        if !username.trim().is_empty() {
            speaker.username = username.to_string();
        }
        let (packet_start, packet_end, continuous) =
            packet_audio_bounds(speaker, pcm, observed_end);
        if !continuous && speaker.pcm.is_empty() {
            reset_stt_gate_state(speaker);
            reset_wake_stream_state(speaker);
        }
        append_wake_pcm(speaker, pcm, packet_start);
        let accepted = self.ingest_stt_pcm(speaker, pcm, packet_start);
        if !pcm.is_empty() {
            speaker.last_input_at = Some(packet_end);
        }
        speaker.last_packet_monotonic = now_monotonic;
        speaker.active = has_stt_buffered_audio(speaker);
        session.participants.insert(
            user_id.to_string(),
            BTreeMap::from([
                ("label".to_string(), speaker.label.clone()),
                ("username".to_string(), speaker.username.clone()),
            ]),
        );
        if accepted {
            session.last_pcm_at = speaker.last_pcm_at;
        }
        session.last_pcm_monotonic = now_monotonic;
        session.last_stall_log_monotonic = 0.0;
        AudioPipelineOutcome::Buffered
    }

    pub fn handle_silence_packet(
        &self,
        session: Option<&mut LiveVoiceSession>,
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
        if !has_any_buffered_audio(speaker) || speaker.flush_in_flight {
            return AudioPipelineOutcome::Ignored;
        }
        let observed_end = Utc::now();
        let now_monotonic = monotonic_seconds();
        if !label.trim().is_empty() {
            speaker.label = label.to_string();
        }
        if !username.trim().is_empty() {
            speaker.username = username.to_string();
        }
        let (packet_start, packet_end, continuous) =
            packet_audio_bounds(speaker, pcm, observed_end);
        if !continuous && speaker.pcm.is_empty() {
            reset_stt_gate_state(speaker);
            reset_wake_stream_state(speaker);
        }
        let accepted = self.ingest_stt_pcm(speaker, pcm, packet_start);
        append_wake_pcm(speaker, pcm, packet_start);
        if !pcm.is_empty() {
            speaker.last_input_at = Some(packet_end);
        }
        speaker.last_packet_monotonic = now_monotonic;
        speaker.active = has_stt_buffered_audio(speaker);
        if accepted {
            session.last_pcm_at = speaker.last_pcm_at;
        }
        session.last_pcm_monotonic = now_monotonic;
        session.last_stall_log_monotonic = 0.0;
        note_packet_debug(session, "preservedSilencePackets");
        AudioPipelineOutcome::Buffered
    }

    pub fn handle_empty_pcm_packet(
        &self,
        session: Option<&mut LiveVoiceSession>,
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
        if !has_any_buffered_audio(speaker) || speaker.flush_in_flight {
            note_packet_debug(session, "droppedEmptyPcmPackets");
            return AudioPipelineOutcome::Ignored;
        }
        let observed_end = Utc::now();
        let now_monotonic = monotonic_seconds();
        if !label.trim().is_empty() {
            speaker.label = label.to_string();
        }
        if !username.trim().is_empty() {
            speaker.username = username.to_string();
        }
        let (packet_start, packet_end, continuous) =
            packet_audio_bounds(speaker, &PCM_20MS_SILENCE, observed_end);
        if !continuous && speaker.pcm.is_empty() {
            reset_stt_gate_state(speaker);
            reset_wake_stream_state(speaker);
        }
        self.ingest_stt_pcm(speaker, &PCM_20MS_SILENCE, packet_start);
        append_wake_pcm(speaker, &PCM_20MS_SILENCE, packet_start);
        speaker.last_input_at = Some(packet_end);
        speaker.last_packet_monotonic = now_monotonic;
        speaker.active = has_stt_buffered_audio(speaker);
        if speaker.active {
            session.last_pcm_at = speaker.last_pcm_at;
        }
        session.last_pcm_monotonic = speaker.last_packet_monotonic;
        session.last_stall_log_monotonic = 0.0;
        note_packet_debug(session, "emptyPcmSilenceFrames");
        AudioPipelineOutcome::Buffered
    }

    pub fn handle_speaking_state(
        &self,
        session: Option<&mut LiveVoiceSession>,
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
            if active && !label.trim().is_empty() {
                session.participants.insert(
                    user_id.to_string(),
                    BTreeMap::from([
                        ("label".to_string(), label.to_string()),
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
        speaker.active = active && has_stt_buffered_audio(speaker);
        AudioPipelineOutcome::Buffered
    }

    pub fn close_speaker_segment(
        &self,
        session: &mut LiveVoiceSession,
        user_id: &str,
    ) -> Result<AudioPipelineOutcome> {
        self.close_speaker_segment_with_reason(session, user_id, SegmentCloseReason::ManualFlush)
    }

    pub fn close_speaker_segment_with_reason(
        &self,
        session: &mut LiveVoiceSession,
        user_id: &str,
        close_reason: SegmentCloseReason,
    ) -> Result<AudioPipelineOutcome> {
        let Some(speaker) = session.buffers.get_mut(user_id) else {
            return Ok(AudioPipelineOutcome::Ignored);
        };
        if speaker.pcm.is_empty() || speaker.flush_in_flight {
            return Ok(AudioPipelineOutcome::Ignored);
        }
        speaker.flush_in_flight = true;
        let mut pcm = std::mem::take(&mut speaker.pcm);
        let trimmed_trailing_ms = trim_trailing_silence(&mut pcm, speaker.stt_trailing_silence_ms);
        let started_at = speaker.started_at.unwrap_or_else(Utc::now);
        let ended_at = speaker
            .last_pcm_at
            .as_ref()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("speaker buffer {user_id} missing last_pcm_at"))?;
        let label = speaker.label.clone();
        let username = speaker.username.clone();
        let stt_input_ms = speaker.stt_input_ms;
        let stt_preroll_ms = stt_preroll_duration_ms(speaker);
        let stt_soft_break_ms = speaker.stt_soft_break_ms;
        speaker.started_at = None;
        reset_stt_gate_state(speaker);
        speaker.active = false;
        speaker.flush_in_flight = false;
        reset_wake_stream_state(speaker);
        let duration_ms = duration_ms_for_pcm(&pcm);
        if duration_ms < self.minimum_utterance_ms {
            return Ok(AudioPipelineOutcome::SegmentTooShort { duration_ms });
        }
        let stt_dropped_ms = (stt_input_ms - duration_ms - trimmed_trailing_ms).max(0);
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
            stt_input_ms,
            stt_dropped_ms,
            stt_preroll_ms,
            trimmed_trailing_ms,
            stt_soft_break_ms,
            close_reason,
        )
    }

    pub fn flush_speaker(
        &self,
        session: &mut LiveVoiceSession,
        user_id: &str,
    ) -> Result<AudioPipelineOutcome> {
        self.close_speaker_segment(session, user_id)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn capture_segment(
        &self,
        session: &mut LiveVoiceSession,
        segment_index: i64,
        speaker_id: &str,
        label: &str,
        username: &str,
        pcm: &[u8],
        started_at: DateTime<Utc>,
        ended_at: DateTime<Utc>,
        stt_input_ms: i64,
        stt_dropped_ms: i64,
        stt_preroll_ms: i64,
        trimmed_trailing_ms: i64,
        stt_soft_break_ms: i64,
        close_reason: SegmentCloseReason,
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
        let post_processing = segment_post_processing(
            &artifact.post_processing,
            stt_input_ms,
            stt_dropped_ms,
            stt_preroll_ms,
            trimmed_trailing_ms,
            stt_soft_break_ms,
            close_reason,
            pcm,
        );
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
                "postProcessing": post_processing,
        });
        if DiagnosticsConfig::from_config().audio_stats {
            merge_object(&mut log_fields, analyze_pcm_bytes(pcm));
        }
        note_session_log(session, "captured-segment", log_fields);
        let payload = AudioSegmentPayload {
            guild_id: session.room.guild_id.clone(),
            guild_slug: session.room.guild_slug.clone(),
            voice_channel_id: session.room.channel_id.clone(),
            voice_channel_name: session.room.channel_name.clone(),
            voice_channel_slug: session.room.channel_slug.clone(),
            capture_run_id: first_non_empty([
                session.capture_run_id.clone(),
                session.session_id.clone(),
            ]),
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
            post_processing,
        };
        Ok(AudioPipelineOutcome::SegmentReady { payload, segment })
    }

    pub fn capture_wake_probe(
        &self,
        session: &mut LiveVoiceSession,
        user_id: &str,
        config: WakeProbeConfig,
        now_monotonic: f64,
        force: bool,
    ) -> Result<Option<WakeProbePayload>> {
        if !config.enabled() {
            return Ok(None);
        }
        let Some(speaker) = session.buffers.get_mut(user_id) else {
            return Ok(None);
        };
        if speaker.wake_pcm.is_empty() || speaker.flush_in_flight {
            return Ok(None);
        }
        let buffered_duration_ms = duration_ms_for_pcm(&speaker.wake_pcm);
        if buffered_duration_ms < config.minimum_ms {
            return Ok(None);
        }
        if !force
            && speaker.last_wake_probe_monotonic > 0.0
            && now_monotonic - speaker.last_wake_probe_monotonic
                < config.interval_ms as f64 / 1000.0
        {
            return Ok(None);
        }

        let current_pcm_len = align_pcm_len(speaker.wake_pcm.len());
        let probe_start_byte = wake_probe_start_byte(speaker, current_pcm_len, config);
        if probe_start_byte >= current_pcm_len {
            return Ok(None);
        }
        let probe_pcm = speaker.wake_pcm[probe_start_byte..current_pcm_len].to_vec();
        let duration_ms = duration_ms_for_pcm(&probe_pcm);
        let minimum_ms = wake_probe_minimum_duration_ms(speaker, config);
        if duration_ms < minimum_ms {
            return Ok(None);
        }
        let buffer_started_at = speaker.wake_started_at.unwrap_or_else(Utc::now);
        let probe_start_time = buffer_started_at
            + chrono::Duration::milliseconds(duration_ms_for_pcm(
                &speaker.wake_pcm[..probe_start_byte],
            ));
        let probe_end_time = probe_start_time + chrono::Duration::milliseconds(duration_ms);
        let probe_index = speaker.wake_probe_counter;
        let reset_stream = speaker.wake_probe_counter == 0;
        let label = speaker.label.clone();
        let username = speaker.username.clone();
        let speaker_id = speaker.user_id.clone();

        let artifact = write_wake_probe_wav(
            &session.session_dir,
            &speaker_id,
            &label,
            probe_index,
            probe_start_time,
            &probe_pcm,
        )?;
        speaker.wake_probe_counter += 1;
        speaker.last_wake_probe_monotonic = now_monotonic;
        speaker.last_wake_probe_pcm_len = 0;
        speaker.wake_pcm.clear();
        speaker.wake_started_at = None;
        let stream_id = format!(
            "{}:{}:{}:{}",
            session.room.guild_id, session.room.channel_id, session.capture_run_id, speaker_id
        );
        note_session_log(
            session,
            "captured-wake-probe",
            json!({
                "probeIndex": probe_index,
                "speakerId": speaker_id,
                "speakerLabel": label,
                "durationMs": duration_ms,
                "pcmBytes": probe_pcm.len(),
                "sourceAudioPath": artifact.path.display().to_string(),
                "audioChecksum": artifact.checksum.clone(),
                "audioBytes": artifact.bytes,
                "streamId": stream_id,
                "resetStream": reset_stream,
                "forced": force,
            }),
        );
        Ok(Some(WakeProbePayload {
            guild_id: session.room.guild_id.clone(),
            guild_slug: session.room.guild_slug.clone(),
            voice_channel_id: session.room.channel_id.clone(),
            voice_channel_name: session.room.channel_name.clone(),
            voice_channel_slug: session.room.channel_slug.clone(),
            capture_run_id: first_non_empty([
                session.capture_run_id.clone(),
                session.session_id.clone(),
            ]),
            voice_bot_id: session.bot_id.clone(),
            voice_bot_discord_user_id: session.bot_user_id.clone(),
            speaker_user_id: speaker_id,
            speaker_label: label,
            speaker_username: username,
            probe_start_time,
            probe_end_time,
            probe_index,
            duration_ms,
            source_audio_path: artifact.path,
            audio_checksum: artifact.checksum,
            audio_bytes: artifact.bytes,
            audio_format: artifact.format,
            sample_rate_hz: artifact.sample_rate_hz,
            channels: artifact.channels,
            sample_width_bits: artifact.sample_width_bits,
            post_processing: artifact.post_processing,
            stream_id,
            reset_stream,
        }))
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

fn active_session(session: Option<&mut LiveVoiceSession>) -> Option<&mut LiveVoiceSession> {
    session.filter(|session| session.ended_at.is_none() && !session.finalizing)
}

fn packet_audio_bounds(
    speaker: &SpeakerBuffer,
    pcm: &[u8],
    observed_end: DateTime<Utc>,
) -> (DateTime<Utc>, DateTime<Utc>, bool) {
    const CONTINUITY_JITTER_MS: i64 = 100;

    let duration_ms = duration_ms_for_pcm(pcm);
    let duration = chrono::Duration::milliseconds(duration_ms);
    let observed_start = observed_end - duration;
    let Some(previous_end) = speaker.last_input_at else {
        return (observed_start, observed_end, true);
    };
    let continuity_deadline =
        previous_end + duration + chrono::Duration::milliseconds(CONTINUITY_JITTER_MS);
    if observed_end <= continuity_deadline {
        let synthetic_end = previous_end + duration;
        return (previous_end, synthetic_end, true);
    }
    (observed_start, observed_end, false)
}

fn append_wake_pcm(speaker: &mut SpeakerBuffer, pcm: &[u8], packet_start: DateTime<Utc>) {
    if pcm.is_empty() {
        return;
    }
    if speaker.wake_pcm.is_empty() {
        speaker.wake_started_at = Some(packet_start);
    }
    speaker.wake_pcm.extend_from_slice(pcm);
}

fn append_preroll_frame(
    speaker: &mut SpeakerBuffer,
    frame: &[u8],
    frame_start: DateTime<Utc>,
    preroll_ms: i64,
) {
    if frame.is_empty() || preroll_ms <= 0 {
        speaker.stt_preroll_pcm.clear();
        speaker.stt_preroll_started_at = None;
        return;
    }
    if speaker.stt_preroll_pcm.is_empty() {
        speaker.stt_preroll_started_at = Some(frame_start);
    }
    speaker.stt_preroll_pcm.extend_from_slice(frame);
    let max_bytes = pcm_bytes_for_duration_ms(preroll_ms);
    if speaker.stt_preroll_pcm.len() > max_bytes {
        let remove = align_pcm_len(speaker.stt_preroll_pcm.len() - max_bytes);
        if remove > 0 {
            let removed_ms = duration_ms_for_pcm(&speaker.stt_preroll_pcm[..remove]);
            speaker.stt_preroll_pcm.drain(..remove);
            if let Some(started_at) = speaker.stt_preroll_started_at {
                speaker.stt_preroll_started_at =
                    Some(started_at + chrono::Duration::milliseconds(removed_ms));
            }
        }
    }
}

fn normalized_rms(pcm: &[u8]) -> f64 {
    let mut sum = 0.0;
    let mut count = 0.0;
    for sample in pcm.chunks_exact(2) {
        let value = i16::from_le_bytes([sample[0], sample[1]]) as f64 / i16::MAX as f64;
        sum += value * value;
        count += 1.0;
    }
    if count == 0.0 {
        return 0.0;
    }
    (sum / count).sqrt().min(1.0)
}

fn normalized_peak(pcm: &[u8]) -> f64 {
    let mut peak = 0.0;
    for sample in pcm.chunks_exact(2) {
        let value = (i16::from_le_bytes([sample[0], sample[1]]) as f64 / i16::MAX as f64).abs();
        if value > peak {
            peak = value;
        }
    }
    peak.min(1.0)
}

fn has_stt_buffered_audio(speaker: &SpeakerBuffer) -> bool {
    !speaker.pcm.is_empty()
}

fn has_any_buffered_audio(speaker: &SpeakerBuffer) -> bool {
    !speaker.pcm.is_empty() || !speaker.wake_pcm.is_empty()
}

fn stt_preroll_duration_ms(speaker: &SpeakerBuffer) -> i64 {
    duration_ms_for_pcm(&speaker.stt_preroll_pcm)
}

fn trim_trailing_silence(pcm: &mut Vec<u8>, trailing_silence_ms: i64) -> i64 {
    if trailing_silence_ms <= 0 || pcm.is_empty() {
        return 0;
    }
    let trim_bytes = pcm_bytes_for_duration_ms(trailing_silence_ms).min(pcm.len());
    let trim_bytes = align_pcm_len(trim_bytes);
    if trim_bytes == 0 || trim_bytes >= pcm.len() {
        return 0;
    }
    let trimmed_ms = duration_ms_for_pcm(&pcm[pcm.len() - trim_bytes..]);
    pcm.truncate(pcm.len() - trim_bytes);
    trimmed_ms
}

fn reset_stt_gate_state(speaker: &mut SpeakerBuffer) {
    speaker.stt_preroll_pcm.clear();
    speaker.stt_preroll_started_at = None;
    speaker.stt_voiced_ms = 0;
    speaker.stt_trailing_silence_ms = 0;
    speaker.stt_input_ms = 0;
    speaker.stt_soft_break_ms = 0;
}

fn segment_post_processing(
    base: &str,
    stt_input_ms: i64,
    stt_dropped_ms: i64,
    stt_preroll_ms: i64,
    trimmed_trailing_ms: i64,
    stt_soft_break_ms: i64,
    close_reason: SegmentCloseReason,
    pcm: &[u8],
) -> String {
    let stats = analyze_pcm_bytes(pcm);
    let rms = normalized_rms(pcm);
    let peak = normalized_peak(pcm);
    let rms_dbfs = stats
        .get("rmsDbFS")
        .and_then(Value::as_f64)
        .unwrap_or(-999.0);
    let peak_dbfs = stats
        .get("peakDbFS")
        .and_then(Value::as_f64)
        .unwrap_or(-999.0);
    format!(
        "{base};stt_gate=rms;stt_close_reason={};stt_input_ms={};stt_dropped_ms={};stt_preroll_ms={};stt_soft_break_ms={};stt_trimmed_trailing_ms={};audio_rms={:.6};audio_peak={:.6};rms_dbfs={:.1};peak_dbfs={:.1}",
        close_reason.as_str(),
        stt_input_ms,
        stt_dropped_ms,
        stt_preroll_ms,
        stt_soft_break_ms,
        trimmed_trailing_ms,
        rms,
        peak,
        rms_dbfs,
        peak_dbfs,
    )
}

fn note_packet_debug(session: &mut LiveVoiceSession, key: &str) {
    *session.packet_debug.entry(key.to_string()).or_insert(0) += 1;
}

fn next_segment_index(session: &mut LiveVoiceSession) -> i64 {
    let segment_index = session.segment_counter;
    session.segment_counter += 1;
    segment_index
}

fn wake_probe_start_byte(
    speaker: &SpeakerBuffer,
    current_pcm_len: usize,
    config: WakeProbeConfig,
) -> usize {
    if speaker.wake_probe_counter == 0 {
        let window_bytes = pcm_bytes_for_duration_ms(config.window_ms);
        return align_pcm_len(current_pcm_len.saturating_sub(window_bytes));
    }
    speaker.last_wake_probe_pcm_len.min(current_pcm_len)
}

fn wake_probe_minimum_duration_ms(speaker: &SpeakerBuffer, config: WakeProbeConfig) -> i64 {
    if speaker.wake_probe_counter == 0 {
        return config.minimum_ms;
    }
    config.interval_ms.min(config.minimum_ms).max(80)
}

fn reset_wake_stream_state(speaker: &mut SpeakerBuffer) {
    speaker.wake_pcm.clear();
    speaker.wake_started_at = None;
    speaker.wake_probe_counter = 0;
    speaker.last_wake_probe_monotonic = 0.0;
    speaker.last_wake_probe_pcm_len = 0;
}

fn pcm_frame_bytes() -> usize {
    PCM_CHANNELS as usize * PCM_SAMPLE_WIDTH as usize
}

fn align_pcm_len(len: usize) -> usize {
    len - (len % pcm_frame_bytes())
}

fn pcm_bytes_for_duration_ms(duration_ms: i64) -> usize {
    let bytes_per_second =
        PCM_SAMPLE_RATE as usize * PCM_CHANNELS as usize * PCM_SAMPLE_WIDTH as usize;
    let bytes = ((bytes_per_second as i64 * duration_ms.max(1)) / 1000)
        .max(pcm_frame_bytes() as i64) as usize;
    bytes - (bytes % pcm_frame_bytes())
}

fn note_session_log(session: &mut LiveVoiceSession, action: &str, fields: Value) {
    session.debug_notes.insert(
        format!("last_{action}"),
        serde_json::to_string(&fields).unwrap_or_default(),
    );
}

pub(crate) fn monotonic_seconds() -> f64 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    START
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_secs_f64()
}
