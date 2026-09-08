#![allow(missing_docs)]

use super::Interconnect;
use crate::driver::DecodeConfig;
use dashmap::DashMap;
use serenity_voice_model::id::UserId;

pub enum UdpRxMessage {
    SetConfig(DecodeConfig),
    ReplaceInterconnect(Interconnect),
}

#[derive(Debug, Default)]
pub struct SsrcTracker {
    // Speaking binds a transport stream to a user for this voice connection.
    // ClientDisconnect has no SSRC or device/session ID: it cannot revoke this
    // binding safely when the same user is transferring between devices.
    pub ssrc_user_map: DashMap<u32, UserId>,
}
