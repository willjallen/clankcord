pub mod capture;
pub(crate) mod discord_io;
pub(crate) mod playback;
pub(crate) mod room_placement;
mod status;

pub use status::{
    ArtifactStatus, SessionArtifacts, SessionCaptureStats, SessionSpeakerCaptureStats,
    VoiceAssignment, VoiceBotStatus, VoiceCaptureSessionStatus,
};
