use serde::{Deserialize, Serialize};

use crate::adapters::discord::voice::artifacts::PCM_20MS_SILENCE;

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

    pub fn wants_opus(&self) -> bool {
        false
    }

    pub fn on_voice_member_speaking_start(&self, member: &CaptureUser) -> CaptureAction {
        CaptureAction::SpeakingState {
            session_id: self.session_id.clone(),
            user_id: member.id.clone(),
            label: user_label(member),
            username: member.name.clone(),
            active: true,
        }
    }

    pub fn on_voice_member_speaking_stop(&self, member: &CaptureUser) -> CaptureAction {
        CaptureAction::SpeakingState {
            session_id: self.session_id.clone(),
            user_id: member.id.clone(),
            label: user_label(member),
            username: member.name.clone(),
            active: false,
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

    pub fn cleanup(&self) {}
}

pub fn user_label(user: &CaptureUser) -> String {
    for value in [&user.display_name, &user.global_name, &user.name, &user.id] {
        if !value.trim().is_empty() {
            return value.trim().to_string();
        }
    }
    String::new()
}

pub fn log(message: &str) {
    println!("[discord-voice] {message}");
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
