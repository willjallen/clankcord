use std::collections::{BTreeMap, BTreeSet};

use chrono::Utc;
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

use crate::adapters::discord::voice::artifacts::PCM_20MS_SILENCE;
use crate::adapters::discord::voice::session::{
    AudioPipelineOutcome, SessionAudioPipeline, WakeProbeConfig, monotonic_seconds,
};
use crate::adapters::discord::voice::types::LiveVoiceSession;
use crate::runtime::{Job, VoiceCaptureSessionStatus, log};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CaptureUser {
    pub id: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub global_name: String,
    #[serde(default)]
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VoiceData {
    pub user: Option<CaptureUser>,
    #[serde(default)]
    pub pcm: Vec<u8>,
    #[serde(default = "default_has_packet")]
    pub has_packet: bool,
    #[serde(default)]
    pub is_silence: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureAction {
    PacketDebug {
        session_id: String,
        key: String,
    },
    SyntheticPacket {
        session_id: String,
        has_pcm: bool,
    },
    SpeakingState {
        session_id: String,
        user_id: String,
        label: String,
        username: String,
        active: bool,
    },
    PcmPacket {
        session_id: String,
        user_id: String,
        label: String,
        username: String,
        pcm: Vec<u8>,
    },
    SilencePacket {
        session_id: String,
        user_id: String,
        label: String,
        username: String,
        pcm: Vec<u8>,
    },
    EmptyPcmPacket {
        session_id: String,
        user_id: String,
        label: String,
        username: String,
    },
    Log(String),
}

pub trait VoiceCaptureHandler {
    fn note_packet_debug(&mut self, session_id: &str, key: &str);
    fn note_synthetic_packet(&mut self, session_id: &str, has_pcm: bool);
    fn handle_speaking_state(
        &mut self,
        session_id: &str,
        user_id: &str,
        label: &str,
        username: &str,
        active: bool,
    );
    fn handle_pcm_packet(
        &mut self,
        session_id: &str,
        user_id: &str,
        label: &str,
        username: &str,
        pcm: &[u8],
    );
    fn handle_silence_packet(
        &mut self,
        session_id: &str,
        user_id: &str,
        label: &str,
        username: &str,
        pcm: &[u8],
    );
    fn handle_empty_pcm_packet(
        &mut self,
        session_id: &str,
        user_id: &str,
        label: &str,
        username: &str,
    );
    fn log(&mut self, message: &str);
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceCaptureSink {
    pub session_id: String,
    #[serde(default)]
    pub missing_user_warnings: usize,
    #[serde(default)]
    pub empty_pcm_warnings: usize,
    #[serde(default)]
    pub synthetic_packet_warnings: usize,
}

impl VoiceCaptureSink {
    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            missing_user_warnings: 0,
            empty_pcm_warnings: 0,
            synthetic_packet_warnings: 0,
        }
    }

    pub fn write_actions(&mut self, data: VoiceData) -> Vec<CaptureAction> {
        let mut actions = vec![CaptureAction::PacketDebug {
            session_id: self.session_id.clone(),
            key: "writeCalls".to_string(),
        }];
        let mut pcm = data.pcm;
        if !data.has_packet {
            actions.push(CaptureAction::SyntheticPacket {
                session_id: self.session_id.clone(),
                has_pcm: !pcm.is_empty(),
            });
            if self.synthetic_packet_warnings < 5 {
                actions.push(CaptureAction::Log(format!(
                    "voice packet dropped for {}: synthetic concealment packet",
                    self.session_id
                )));
                self.synthetic_packet_warnings += 1;
            }
            return actions;
        }
        if data.is_silence {
            actions.push(CaptureAction::PacketDebug {
                session_id: self.session_id.clone(),
                key: "silencePackets".to_string(),
            });
            if pcm.is_empty() {
                pcm = PCM_20MS_SILENCE.to_vec();
            }
        }
        let Some(user) = data.user else {
            actions.push(CaptureAction::PacketDebug {
                session_id: self.session_id.clone(),
                key: "missingUserPackets".to_string(),
            });
            if self.missing_user_warnings < 5 {
                actions.push(CaptureAction::Log(format!(
                    "voice packet dropped for {}: missing user mapping",
                    self.session_id
                )));
                self.missing_user_warnings += 1;
            }
            return actions;
        };
        let label = user_label(&user);
        let username = user.name.clone();
        if pcm.is_empty() {
            actions.push(CaptureAction::PacketDebug {
                session_id: self.session_id.clone(),
                key: "emptyPcmPackets".to_string(),
            });
            if self.empty_pcm_warnings < 5 {
                actions.push(CaptureAction::Log(format!(
                    "voice packet missing pcm for {}: preserving decode-loss frame as silence",
                    self.session_id
                )));
                self.empty_pcm_warnings += 1;
            }
            actions.push(CaptureAction::EmptyPcmPacket {
                session_id: self.session_id.clone(),
                user_id: user.id,
                label,
                username,
            });
            return actions;
        }
        if data.is_silence {
            actions.push(CaptureAction::SilencePacket {
                session_id: self.session_id.clone(),
                user_id: user.id,
                label,
                username,
                pcm,
            });
        } else {
            actions.push(CaptureAction::PacketDebug {
                session_id: self.session_id.clone(),
                key: "pcmPackets".to_string(),
            });
            actions.push(CaptureAction::PcmPacket {
                session_id: self.session_id.clone(),
                user_id: user.id,
                label,
                username,
                pcm,
            });
        }
        actions
    }

    pub fn write<H: VoiceCaptureHandler>(&mut self, handler: &mut H, data: VoiceData) {
        for action in self.write_actions(data) {
            apply_action(handler, action);
        }
    }
}

pub(super) struct LiveCaptureSession {
    session: LiveVoiceSession,
    pipeline: SessionAudioPipeline,
    wake_probe: WakeProbeConfig,
    sink: VoiceCaptureSink,
    ssrc_users: BTreeMap<u32, CaptureUser>,
}

impl LiveCaptureSession {
    pub(super) fn new(
        session: LiveVoiceSession,
        minimum_utterance_ms: i64,
        wake_probe: WakeProbeConfig,
    ) -> Self {
        let session_id = session.session_id.clone();
        Self {
            session,
            pipeline: SessionAudioPipeline::new().with_minimum_utterance_ms(minimum_utterance_ms),
            wake_probe,
            sink: VoiceCaptureSink::new(session_id),
            ssrc_users: BTreeMap::new(),
        }
    }

    pub(super) fn metadata(&self, tz: Tz) -> VoiceCaptureSessionStatus {
        self.session.metadata(tz)
    }

    pub(super) fn set_debug_note(&mut self, key: &str, value: String) {
        self.session.debug_notes.insert(key.to_string(), value);
    }

    pub(super) fn debug_note(&self, key: &str) -> Option<&str> {
        self.session.debug_notes.get(key).map(String::as_str)
    }

    pub(super) fn debug_notes(&self) -> BTreeMap<String, String> {
        self.session.debug_notes.clone()
    }

    pub(super) fn discord_lookup_context(&self) -> (String, String) {
        (
            self.session.bot_id.clone(),
            self.session.room.guild_id.clone(),
        )
    }

    pub(super) fn note_speaking_state(&mut self, ssrc: u32, user: CaptureUser, active: bool) {
        self.ssrc_users.insert(ssrc, user.clone());
        let pipeline = self.pipeline.clone();
        let session_id = self.session.session_id.clone();
        let mut handler = SessionCaptureHandler {
            pipeline,
            session: &mut self.session,
        };
        handler.handle_speaking_state(
            &session_id,
            &user.id,
            &user.display_name,
            &user.name,
            active,
        );
    }

    pub(super) fn note_client_disconnect(&mut self, user_id: &str) -> Vec<Job> {
        self.ssrc_users.retain(|_, user| user.id != user_id);
        let session_id = self.session.session_id.clone();
        let pipeline = self.pipeline.clone();
        {
            let mut handler = SessionCaptureHandler {
                pipeline: pipeline.clone(),
                session: &mut self.session,
            };
            handler.handle_speaking_state(&session_id, user_id, "", "", false);
        }
        let mut jobs = self.capture_wake_probes(vec![user_id.to_string()], true);
        match pipeline.flush_speaker(&mut self.session, user_id) {
            Ok(outcome) => collect_audio_job(outcome, &mut jobs),
            Err(error) => {
                log(&format!("voice disconnect flush failed: {error}"));
            }
        }
        jobs
    }

    pub(super) fn write_voice_tick(
        &mut self,
        speaking: Vec<(u32, VoiceData)>,
        silent: Vec<u32>,
    ) -> Vec<Job> {
        let mut touched_user_ids = BTreeSet::new();
        for (ssrc, data) in speaking {
            let user = self.ssrc_users.get(&ssrc).cloned();
            if let Some(user_id) = data
                .user
                .as_ref()
                .map(|user| user.id.clone())
                .or_else(|| user.as_ref().map(|user| user.id.clone()))
            {
                touched_user_ids.insert(user_id);
            }
            self.write_voice_data(user, data);
        }
        for ssrc in silent {
            let Some(user) = self.ssrc_users.get(&ssrc).cloned() else {
                continue;
            };
            touched_user_ids.insert(user.id.clone());
            self.write_voice_data(
                Some(user),
                VoiceData {
                    user: None,
                    pcm: Vec::new(),
                    has_packet: true,
                    is_silence: true,
                },
            );
        }
        self.capture_wake_probes(touched_user_ids.into_iter().collect(), false)
    }

    pub(super) fn flush_ready_buffers(&mut self, max_segment_ms: i64, silence_ms: i64) -> Vec<Job> {
        if self.session.ended_at.is_some() || self.session.finalizing {
            return Vec::new();
        }
        let now = monotonic_seconds();
        let user_ids = self
            .session
            .buffers
            .iter()
            .filter_map(|(user_id, speaker)| {
                if speaker.pcm.is_empty() || speaker.flush_in_flight {
                    return None;
                }
                let buffered_duration_ms =
                    crate::adapters::discord::voice::artifacts::duration_ms_for_pcm(&speaker.pcm);
                let should_flush = buffered_duration_ms >= max_segment_ms
                    || now - speaker.last_packet_monotonic >= silence_ms as f64 / 1000.0;
                if should_flush {
                    Some(user_id.clone())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        let mut jobs = self.capture_wake_probes(user_ids.clone(), true);
        jobs.extend(self.flush_speakers(user_ids));
        jobs
    }

    pub(super) fn finish(&mut self, reason: String, tz: Tz) -> FinishedCaptureSession {
        self.session.finalizing = true;
        let user_ids = self.session.buffers.keys().cloned().collect::<Vec<_>>();
        let mut audio_jobs = self.capture_wake_probes(user_ids.clone(), true);
        audio_jobs.extend(self.flush_speakers(user_ids));
        self.session.ended_at = Some(Utc::now());
        self.session.finalizing = false;
        self.session
            .debug_notes
            .insert("leaveReason".to_string(), reason);
        FinishedCaptureSession {
            session_id: self.session.session_id.clone(),
            metadata: self.session.metadata(tz),
            bot_id: self.session.bot_id.clone(),
            guild_id: self.session.room.guild_id.clone(),
            voice_channel_id: self.session.room.channel_id.clone(),
            capture_run_id: self.session.capture_run_id.clone(),
            audio_jobs,
        }
    }

    fn flush_speakers(&mut self, user_ids: Vec<String>) -> Vec<Job> {
        let pipeline = self.pipeline.clone();
        let mut jobs = Vec::new();
        for user_id in user_ids {
            match pipeline.flush_speaker(&mut self.session, &user_id) {
                Ok(outcome) => collect_audio_job(outcome, &mut jobs),
                Err(error) => log(&format!("voice buffer flush failed: {error}")),
            }
        }
        jobs
    }

    fn capture_wake_probes(&mut self, user_ids: Vec<String>, force: bool) -> Vec<Job> {
        let pipeline = self.pipeline.clone();
        let now = monotonic_seconds();
        let mut jobs = Vec::new();
        for user_id in user_ids {
            match pipeline.capture_wake_probe(
                &mut self.session,
                &user_id,
                self.wake_probe,
                now,
                force,
            ) {
                Ok(Some(payload)) => jobs.push(Job::wake_probe(payload)),
                Ok(None) => {}
                Err(error) => log(&format!("wake probe capture failed: {error}")),
            }
        }
        jobs
    }

    fn write_voice_data(&mut self, user: Option<CaptureUser>, mut data: VoiceData) {
        if data.user.is_none() {
            data.user = user;
        }
        let mut sink = std::mem::replace(
            &mut self.sink,
            VoiceCaptureSink::new(&self.session.session_id),
        );
        let mut handler = SessionCaptureHandler {
            pipeline: self.pipeline.clone(),
            session: &mut self.session,
        };
        sink.write(&mut handler, data);
        self.sink = sink;
    }
}

pub(super) struct FinishedCaptureSession {
    pub(super) session_id: String,
    pub(super) metadata: VoiceCaptureSessionStatus,
    pub(super) bot_id: String,
    pub(super) guild_id: String,
    pub(super) voice_channel_id: String,
    pub(super) capture_run_id: String,
    pub(super) audio_jobs: Vec<Job>,
}

pub fn user_label(user: &CaptureUser) -> String {
    for value in [&user.display_name, &user.global_name, &user.name, &user.id] {
        if !value.trim().is_empty() {
            return value.trim().to_string();
        }
    }
    String::new()
}

pub fn apply_action<H: VoiceCaptureHandler>(handler: &mut H, action: CaptureAction) {
    match action {
        CaptureAction::PacketDebug { session_id, key } => {
            handler.note_packet_debug(&session_id, &key)
        }
        CaptureAction::SyntheticPacket {
            session_id,
            has_pcm,
        } => handler.note_synthetic_packet(&session_id, has_pcm),
        CaptureAction::SpeakingState {
            session_id,
            user_id,
            label,
            username,
            active,
        } => handler.handle_speaking_state(&session_id, &user_id, &label, &username, active),
        CaptureAction::PcmPacket {
            session_id,
            user_id,
            label,
            username,
            pcm,
        } => handler.handle_pcm_packet(&session_id, &user_id, &label, &username, &pcm),
        CaptureAction::SilencePacket {
            session_id,
            user_id,
            label,
            username,
            pcm,
        } => handler.handle_silence_packet(&session_id, &user_id, &label, &username, &pcm),
        CaptureAction::EmptyPcmPacket {
            session_id,
            user_id,
            label,
            username,
        } => handler.handle_empty_pcm_packet(&session_id, &user_id, &label, &username),
        CaptureAction::Log(message) => handler.log(&message),
    }
}

fn default_has_packet() -> bool {
    true
}

struct SessionCaptureHandler<'a> {
    pipeline: SessionAudioPipeline,
    session: &'a mut LiveVoiceSession,
}

impl VoiceCaptureHandler for SessionCaptureHandler<'_> {
    fn note_packet_debug(&mut self, _session_id: &str, key: &str) {
        *self
            .session
            .packet_debug
            .entry(key.to_string())
            .or_insert(0) += 1;
    }

    fn note_synthetic_packet(&mut self, _session_id: &str, has_pcm: bool) {
        self.note_packet_debug("", "syntheticPackets");
        if has_pcm {
            self.note_packet_debug("", "syntheticPcmPackets");
        }
    }

    fn handle_speaking_state(
        &mut self,
        _session_id: &str,
        user_id: &str,
        label: &str,
        username: &str,
        active: bool,
    ) {
        let _ = self.pipeline.handle_speaking_state(
            Some(&mut *self.session),
            user_id,
            label,
            username,
            active,
        );
    }

    fn handle_pcm_packet(
        &mut self,
        _session_id: &str,
        user_id: &str,
        label: &str,
        username: &str,
        pcm: &[u8],
    ) {
        let _ = self.pipeline.handle_pcm_packet(
            Some(&mut *self.session),
            user_id,
            label,
            username,
            pcm,
        );
    }

    fn handle_silence_packet(
        &mut self,
        _session_id: &str,
        user_id: &str,
        label: &str,
        username: &str,
        pcm: &[u8],
    ) {
        let _ = self.pipeline.handle_silence_packet(
            Some(&mut *self.session),
            user_id,
            label,
            username,
            pcm,
        );
    }

    fn handle_empty_pcm_packet(
        &mut self,
        _session_id: &str,
        user_id: &str,
        label: &str,
        username: &str,
    ) {
        let _ = self.pipeline.handle_empty_pcm_packet(
            Some(&mut *self.session),
            user_id,
            label,
            username,
        );
    }

    fn log(&mut self, message: &str) {
        log(message);
    }
}

fn collect_audio_job(outcome: AudioPipelineOutcome, jobs: &mut Vec<Job>) {
    if let Some(job) = audio_job_from_outcome(outcome) {
        jobs.push(job);
    }
}

fn audio_job_from_outcome(outcome: AudioPipelineOutcome) -> Option<Job> {
    match outcome {
        AudioPipelineOutcome::SegmentReady { payload, .. } => Some(Job::audio_segment(payload)),
        _ => None,
    }
}
