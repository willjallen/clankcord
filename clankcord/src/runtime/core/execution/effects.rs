use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;

use chrono::{DateTime, Utc};

use crate::Result;
use crate::runtime::{Job, RoomConfig, RuntimeBotStatus, RuntimeSessionStatus};

pub(crate) type JoinRoomEffectFuture<'a> =
    Pin<Box<dyn Future<Output = Result<JoinRoomEffectResult>> + Send + 'a>>;
pub(crate) type LeaveRoomEffectFuture<'a> =
    Pin<Box<dyn Future<Output = Result<LeaveRoomEffectResult>> + Send + 'a>>;

#[derive(Debug, Clone)]
pub(crate) struct JoinRoomEffectRequest {
    pub room: RoomConfig,
    pub bot_id: String,
    pub bot_user_id: String,
    pub capture_run_id: String,
    pub assignment_id: String,
    pub started_at: DateTime<Utc>,
    pub session_dir: PathBuf,
    pub requested_by_user_id: String,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub(crate) struct JoinRoomEffectResult {
    pub status: String,
    pub session: Option<RuntimeSessionStatus>,
    pub bot_status: Option<RuntimeBotStatus>,
    pub message: String,
}

#[derive(Debug, Clone)]
pub(crate) struct LeaveRoomEffectRequest {
    pub session_id: String,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub(crate) struct LeaveRoomEffectResult {
    pub session_id: String,
    pub status: String,
    pub session: Option<RuntimeSessionStatus>,
    pub bot_status: Option<RuntimeBotStatus>,
    pub guild_id: String,
    pub voice_channel_id: String,
    pub capture_run_id: String,
    pub audio_jobs: Vec<Job>,
}

pub(crate) trait RuntimeEffects: Send + Sync {
    fn join_room<'a>(&'a self, request: JoinRoomEffectRequest) -> JoinRoomEffectFuture<'a>;

    fn leave_room<'a>(&'a self, request: LeaveRoomEffectRequest) -> LeaveRoomEffectFuture<'a>;
}
