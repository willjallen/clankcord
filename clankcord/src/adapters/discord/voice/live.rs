use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use serenity::model::gateway::Ready;
use serenity::model::guild::Member;
use serenity::model::id::{GuildId, UserId};
use serenity::model::voice::VoiceState;
use tokio::sync::Mutex;

use crate::Result;
use crate::adapters::discord::voice::capture::{CaptureUser, LiveCaptureSession, VoiceData};
use crate::adapters::discord::voice::client_connection::{
    DiscordVoiceClient, describe_error, join_voice_channel, leave_voice_channel,
    load_client_token_specs, parse_discord_id, play_voice_file, set_voice_mute,
};
use crate::adapters::discord::voice::session::WakeProbeConfig;
use crate::adapters::discord::voice::types::LiveVoiceSession;
use crate::config::{local_tz, transcription_config};
use crate::errors::discord_tool_error;
use crate::runtime::timeline::TimelineStore;
use crate::runtime::{
    DiscordVoiceJoinOutput, DiscordVoiceJoinPayload, DiscordVoiceLeaveOutput,
    DiscordVoiceLeavePayload, DiscordVoiceMuteOutput, DiscordVoiceMutePayload,
    DiscordVoicePlayAudioOutput, DiscordVoicePlayAudioPayload, DiscordVoiceStatusSnapshotOutput,
    RuntimeJobSink, VoiceBotStatus, log,
};

type LiveCaptureSessionLock = Arc<Mutex<LiveCaptureSession>>;

pub struct LiveVoiceAdapter {
    job_sink: RuntimeJobSink,
    timeline_store: TimelineStore,
    voice_clients_lock: Mutex<BTreeMap<String, DiscordVoiceClient>>,
    capture_sessions_lock: Mutex<BTreeMap<String, LiveCaptureSessionLock>>,
    speaker_profiles_lock: Mutex<BTreeMap<String, CaptureUser>>,
    flush_interval: Duration,
    silence_ms: i64,
    max_segment_ms: i64,
    minimum_utterance_ms: i64,
    wake_probe: WakeProbeConfig,
    no_token_warning_logged: AtomicBool,
}

impl fmt::Debug for LiveVoiceAdapter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LiveVoiceAdapter")
            .field("flush_interval", &self.flush_interval)
            .field("silence_ms", &self.silence_ms)
            .field("max_segment_ms", &self.max_segment_ms)
            .field("minimum_utterance_ms", &self.minimum_utterance_ms)
            .field("wake_probe", &self.wake_probe)
            .finish_non_exhaustive()
    }
}

impl LiveVoiceAdapter {
    pub fn new(job_sink: RuntimeJobSink, timeline_store: TimelineStore) -> Self {
        let transcription = transcription_config();
        let wake = crate::config::app_config().wake.clone();
        let wake_probe = WakeProbeConfig {
            minimum_ms: wake.probe_minimum_ms.max(0),
            window_ms: wake.probe_window_ms.max(0),
            interval_ms: wake.probe_interval_ms.max(0),
        };
        Self {
            job_sink,
            timeline_store,
            voice_clients_lock: Mutex::new(BTreeMap::new()),
            capture_sessions_lock: Mutex::new(BTreeMap::new()),
            speaker_profiles_lock: Mutex::new(BTreeMap::new()),
            flush_interval: Duration::from_millis(
                (crate::config::voice_flush_interval_seconds() * 1000.0) as u64,
            ),
            silence_ms: transcription.silence_ms.max(0),
            max_segment_ms: transcription.max_segment_ms.max(250),
            minimum_utterance_ms: transcription.minimum_utterance_ms.max(0),
            wake_probe,
            no_token_warning_logged: AtomicBool::new(false),
        }
    }

    pub fn flush_interval(&self) -> Duration {
        self.flush_interval
    }

    pub(super) fn job_sink(&self) -> RuntimeJobSink {
        self.job_sink.clone()
    }

    pub async fn start_missing_clients(self: &Arc<Self>) -> Result<()> {
        let specs = load_client_token_specs()?;
        if specs.is_empty() {
            let clients = self.voice_clients_lock.lock().await;
            if clients.is_empty() && !self.no_token_warning_logged.swap(true, Ordering::Relaxed) {
                log("no dedicated voice bot tokens configured; discord voice bots are disabled");
            }
            return Ok(());
        }
        self.no_token_warning_logged.store(false, Ordering::Relaxed);

        for (client_id, token) in specs {
            if self
                .voice_clients_lock
                .lock()
                .await
                .contains_key(&client_id)
            {
                continue;
            }
            self.start_client(client_id, token).await?;
        }
        Ok(())
    }

    async fn start_client(self: &Arc<Self>, client_id: String, token: String) -> Result<()> {
        let client = DiscordVoiceClient::start(self, client_id.clone(), token).await?;
        self.voice_clients_lock
            .lock()
            .await
            .insert(client_id.clone(), client);
        log(&format!("starting Discord voice client {client_id}"));
        Ok(())
    }

    pub async fn shutdown(&self) {
        let clients = self.voice_clients_lock.lock().await;
        for client in clients.values() {
            client.shutdown().await;
        }
    }

    pub(crate) async fn join_assigned_room(
        self: &Arc<Self>,
        request: DiscordVoiceJoinPayload,
    ) -> Result<DiscordVoiceJoinOutput> {
        let room = request.room.clone();
        let session_id = request.capture_run_id.clone();
        let (voice, bot_user_id) = {
            let mut clients = self.voice_clients_lock.lock().await;
            let client = clients.get_mut(&request.bot_id).ok_or_else(|| {
                discord_tool_error(format!("voice bot {} is not running", request.bot_id))
            })?;
            client.joining_session_id = Some(session_id.clone());
            client.last_error.clear();
            (client.voice(), client.discord_user_id()?)
        };

        let guild_id = parse_discord_id("guild_id", &room.guild_id)?;
        let channel_id = parse_discord_id("channel_id", &room.channel_id)?;
        fs::create_dir_all(request.session_dir.join("minutes"))?;
        let session = LiveVoiceSession {
            session_id: session_id.clone(),
            room: room.clone(),
            bot_id: request.bot_id.clone(),
            bot_user_id: bot_user_id.clone(),
            thread_id: String::new(),
            thread_name: String::new(),
            started_at: request.started_at,
            session_dir: request.session_dir.clone(),
            minute_message_ids: BTreeMap::new(),
            participants: BTreeMap::new(),
            buffers: BTreeMap::new(),
            packet_debug: Default::default(),
            debug_notes: BTreeMap::from([
                ("receiveBackend".to_string(), "songbird".to_string()),
                ("joinReason".to_string(), request.reason.clone()),
                (
                    "requestedByUserId".to_string(),
                    request.requested_by_user_id.clone(),
                ),
            ]),
            segment_counter: 0,
            audio_segments: Vec::new(),
            transcription_task_ids: Default::default(),
            finalizing: false,
            ended_at: None,
            voice_channel_id: room.channel_id.clone(),
            transcript_event_count: 0,
            last_pcm_at: None,
            last_transcript_at: None,
            last_pcm_monotonic: 0.0,
            last_transcript_monotonic: 0.0,
            last_stall_log_monotonic: 0.0,
            voice_client_debug: BTreeMap::from([(
                "receiveBackend".to_string(),
                "songbird".to_string(),
            )]),
            capture_run_id: session_id.clone(),
            assignment_id: request.assignment_id.clone(),
            mode: "local_buffering".to_string(),
        };
        let session_metadata = session.metadata(local_tz());
        self.capture_sessions_lock.lock().await.insert(
            session_id.clone(),
            Arc::new(Mutex::new(LiveCaptureSession::new(
                session,
                self.minimum_utterance_ms,
                self.wake_probe,
            ))),
        );

        if let Err(error) = join_voice_channel(self, voice, &session_id, guild_id, channel_id).await
        {
            self.capture_sessions_lock.lock().await.remove(&session_id);
            let error_text = describe_error(&error);
            let status = self.mark_join_failed(&request.bot_id, &error_text).await;
            return Err(discord_tool_error(format!(
                "failed to join {} with {}: {error_text}",
                room.channel_name, request.bot_id
            ))
            .context(format!("bot status after failure: {status:?}")));
        }

        let bot_status = {
            let mut clients = self.voice_clients_lock.lock().await;
            let client = clients.get_mut(&request.bot_id).ok_or_else(|| {
                discord_tool_error(format!("voice bot {} is not running", request.bot_id))
            })?;
            client.joining_session_id = None;
            client.assigned_session_id = Some(session_id.clone());
            client.current_guild_id = room.guild_id.clone();
            client.current_channel_id = room.channel_id.clone();
            client.last_error.clear();
            Some(client.status())
        };
        Ok(DiscordVoiceJoinOutput {
            status: "assigned".to_string(),
            session: Some(session_metadata),
            bot_status,
            message: String::new(),
        })
    }

    async fn mark_join_failed(&self, bot_id: &str, error: &str) -> Option<VoiceBotStatus> {
        let mut clients = self.voice_clients_lock.lock().await;
        let client = clients.get_mut(bot_id)?;
        client.joining_session_id = None;
        client.last_error = error.to_string();
        Some(client.status())
    }

    pub(crate) async fn finish_session(
        self: &Arc<Self>,
        request: DiscordVoiceLeavePayload,
    ) -> Result<DiscordVoiceLeaveOutput> {
        let session_id = request.session_id;
        let live_session = self.capture_sessions_lock.lock().await.remove(&session_id);
        let Some(live_session) = live_session else {
            return Ok(DiscordVoiceLeaveOutput {
                session_id,
                status: "missing_session".to_string(),
                session: None,
                bot_status: None,
                guild_id: String::new(),
                voice_channel_id: String::new(),
                capture_run_id: String::new(),
                audio_jobs: Vec::new(),
            });
        };

        let finished = {
            let mut live_session = live_session.lock().await;
            live_session.finish(request.reason, local_tz())
        };

        let voice = {
            let mut clients = self.voice_clients_lock.lock().await;
            let client = clients.get_mut(&finished.bot_id).ok_or_else(|| {
                discord_tool_error(format!("voice bot {} is not running", finished.bot_id))
            })?;
            client.assigned_session_id = None;
            client.joining_session_id = None;
            client.current_guild_id.clear();
            client.current_channel_id.clear();
            client.voice()
        };
        let guild_id = parse_discord_id("guild_id", &finished.guild_id)?;
        leave_voice_channel(voice, guild_id).await;
        let bot_status = {
            let clients = self.voice_clients_lock.lock().await;
            clients
                .get(&finished.bot_id)
                .map(DiscordVoiceClient::status)
        };
        Ok(DiscordVoiceLeaveOutput {
            session_id: finished.session_id,
            status: "ended".to_string(),
            session: Some(finished.metadata),
            bot_status,
            guild_id: finished.guild_id,
            voice_channel_id: finished.voice_channel_id,
            capture_run_id: finished.capture_run_id,
            audio_jobs: finished.audio_jobs,
        })
    }

    pub(crate) async fn set_session_mute(
        self: &Arc<Self>,
        request: DiscordVoiceMutePayload,
    ) -> Result<DiscordVoiceMuteOutput> {
        let session_id = request.session_id.clone();
        let Some(live_session) = self.session(&session_id).await else {
            return Ok(DiscordVoiceMuteOutput {
                session_id,
                status: "missing_session".to_string(),
                muted: request.muted,
                guild_id: String::new(),
                voice_channel_id: String::new(),
                message: "Voice session is not active.".to_string(),
            });
        };
        let session = {
            let live_session = live_session.lock().await;
            live_session.metadata(local_tz())
        };
        let voice = {
            let clients = self.voice_clients_lock.lock().await;
            let client = clients.get(&session.bot_id).ok_or_else(|| {
                discord_tool_error(format!("voice bot {} is not running", session.bot_id))
            })?;
            client.voice()
        };
        let guild_id = parse_discord_id("guild_id", &session.guild_id)?;
        set_voice_mute(voice, guild_id, request.muted).await?;
        Ok(DiscordVoiceMuteOutput {
            session_id,
            muted: request.muted,
            status: "set".to_string(),
            guild_id: session.guild_id,
            voice_channel_id: session.voice_channel_id,
            message: String::new(),
        })
    }

    pub(crate) async fn play_session_cue(
        self: &Arc<Self>,
        request: DiscordVoicePlayAudioPayload,
    ) -> Result<DiscordVoicePlayAudioOutput> {
        let session_id = request.session_id.clone();
        let Some(live_session) = self.session(&session_id).await else {
            return Ok(DiscordVoicePlayAudioOutput {
                session_id,
                cue: request.cue,
                status: "missing_session".to_string(),
                guild_id: String::new(),
                voice_channel_id: String::new(),
                audio_path: String::new(),
                duration_ms: 0,
                message: "Voice session is not active.".to_string(),
            });
        };
        let session = {
            let live_session = live_session.lock().await;
            live_session.metadata(local_tz())
        };
        let voice = {
            let clients = self.voice_clients_lock.lock().await;
            let client = clients.get(&session.bot_id).ok_or_else(|| {
                discord_tool_error(format!("voice bot {} is not running", session.bot_id))
            })?;
            client.voice()
        };
        let guild_id = parse_discord_id("guild_id", &session.guild_id)?;
        let audio_path = sound_asset_path(request.cue);
        if !audio_path.is_file() {
            anyhow::bail!(
                "voice cue asset is missing for {}: {}",
                request.cue.as_str(),
                audio_path.display()
            );
        }
        let duration = play_voice_file(voice, guild_id, &audio_path, playback_timeout()).await?;
        Ok(DiscordVoicePlayAudioOutput {
            session_id,
            cue: request.cue,
            status: "played".to_string(),
            guild_id: session.guild_id,
            voice_channel_id: session.voice_channel_id,
            audio_path: audio_path.display().to_string(),
            duration_ms: duration.as_millis().min(i64::MAX as u128) as i64,
            message: String::new(),
        })
    }

    pub async fn flush_ready_buffers(&self) -> Result<()> {
        let sessions = {
            let sessions = self.capture_sessions_lock.lock().await;
            sessions
                .iter()
                .map(|(id, session)| (id.clone(), session.clone()))
                .collect::<Vec<_>>()
        };
        let mut audio_jobs = Vec::new();
        for (_session_id, session) in sessions {
            let mut live_session = session.lock().await;
            audio_jobs
                .extend(live_session.flush_ready_buffers(self.max_segment_ms, self.silence_ms));
        }
        for job in audio_jobs {
            self.job_sink.submit_detached(job);
        }
        Ok(())
    }

    pub async fn bot_statuses(&self) -> Vec<VoiceBotStatus> {
        let clients = self.voice_clients_lock.lock().await;
        clients.values().map(DiscordVoiceClient::status).collect()
    }

    pub(crate) async fn voice_status_snapshot(&self) -> DiscordVoiceStatusSnapshotOutput {
        DiscordVoiceStatusSnapshotOutput {
            bots: self.bot_statuses().await,
            sessions: self.session_statuses().await,
        }
    }

    pub async fn session_statuses(&self) -> Vec<crate::runtime::VoiceCaptureSessionStatus> {
        let sessions = {
            let sessions = self.capture_sessions_lock.lock().await;
            sessions.values().cloned().collect::<Vec<_>>()
        };
        let mut statuses = Vec::new();
        for session in sessions {
            statuses.push(session.lock().await.metadata(local_tz()));
        }
        statuses
    }

    pub(super) async fn mark_client_ready(&self, bot_id: &str, ready: Ready) {
        {
            let mut clients = self.voice_clients_lock.lock().await;
            if let Some(client) = clients.get_mut(bot_id) {
                client.ready = true;
                client.user_id = ready.user.id.get().to_string();
                client.username = ready.user.name.clone();
                client.last_error.clear();
            }
        }
        log(&format!("bot {bot_id} ready as {}", ready.user.name));
    }

    pub(super) async fn note_voice_state(
        &self,
        bot_id: &str,
        old: Option<VoiceState>,
        state: VoiceState,
    ) {
        if let Some(member) = old.as_ref().and_then(|state| state.member.as_ref()) {
            self.cache_speaker_profile(capture_user_from_member(member))
                .await;
        }
        if let Some(member) = state.member.as_ref() {
            self.cache_speaker_profile(capture_user_from_member(member))
                .await;
        }
        let user_id = state.user_id.get().to_string();
        let guild_id = state
            .guild_id
            .map(|value| value.get().to_string())
            .unwrap_or_default();
        let channel_id = state
            .channel_id
            .map(|value| value.get().to_string())
            .unwrap_or_default();
        let bot_user_ids = {
            let clients = self.voice_clients_lock.lock().await;
            clients
                .values()
                .map(|client| client.user_id.clone())
                .filter(|value| !value.is_empty())
                .collect::<Vec<_>>()
        };
        if !guild_id.is_empty() && !bot_user_ids.iter().any(|bot_id| bot_id == &user_id) {
            let old_payload = old.as_ref().map(voice_state_payload);
            let new_payload = voice_state_payload(&state);
            if let Err(error) = self
                .timeline_store
                .record_voice_state_update(old_payload, new_payload)
                .await
            {
                log(&format!(
                    "recording Discord voice state failed for user {user_id}: {error}"
                ));
            }
        }

        let mut clients = self.voice_clients_lock.lock().await;
        let Some(client) = clients.get_mut(bot_id) else {
            return;
        };
        if client.user_id.is_empty() || client.user_id != user_id {
            return;
        }
        client.current_guild_id = guild_id;
        client.current_channel_id = channel_id;
    }

    pub(super) async fn note_client_error(&self, bot_id: &str, error: &str) {
        {
            let mut clients = self.voice_clients_lock.lock().await;
            if let Some(client) = clients.get_mut(bot_id) {
                client.ready = false;
                client.last_error = error.to_string();
            }
        }
        log(&format!("bot {bot_id} error: {error}"));
    }

    pub(super) async fn handle_speaking_state(
        &self,
        session_id: &str,
        ssrc: u32,
        user_id: &str,
        active: bool,
    ) {
        if self.is_voice_bot_user(user_id).await {
            return;
        }
        let Some(user) = self.resolve_speaker_profile(session_id, user_id).await else {
            return;
        };
        let session = self.session(session_id).await;
        let Some(session) = session else {
            return;
        };
        let mut live_session = session.lock().await;
        live_session.note_speaking_state(ssrc, user, active);
    }

    pub(super) async fn handle_client_disconnect(&self, session_id: &str, user_id: &str) {
        if self.is_voice_bot_user(user_id).await {
            return;
        }
        let session = self.session(session_id).await;
        let Some(session) = session else {
            return;
        };
        let mut live_session = session.lock().await;
        for job in live_session.note_client_disconnect(user_id) {
            self.job_sink.submit_detached(job);
        }
    }

    pub(super) async fn handle_voice_tick(
        &self,
        session_id: &str,
        speaking: Vec<(u32, VoiceData)>,
        silent: Vec<u32>,
    ) {
        let session = self.session(session_id).await;
        let Some(session) = session else {
            return;
        };
        let mut live_session = session.lock().await;
        let jobs = live_session.write_voice_tick(speaking, silent);
        drop(live_session);
        for job in jobs {
            self.job_sink.submit_detached(job);
        }
    }

    async fn session(&self, session_id: &str) -> Option<LiveCaptureSessionLock> {
        self.capture_sessions_lock
            .lock()
            .await
            .get(session_id)
            .cloned()
    }

    async fn is_voice_bot_user(&self, user_id: &str) -> bool {
        self.voice_clients_lock.lock().await.values().any(|client| {
            let status = client.status();
            !status.user_id.is_empty() && status.user_id == user_id
        })
    }

    async fn resolve_speaker_profile(
        &self,
        session_id: &str,
        user_id: &str,
    ) -> Option<CaptureUser> {
        if let Some(profile) = self.cached_speaker_profile(user_id).await {
            return Some(profile);
        }

        let session = self.session(session_id).await?;
        let (client_id, guild_id) = {
            let live_session = session.lock().await;
            live_session.discord_lookup_context()
        };
        let http = {
            let clients = self.voice_clients_lock.lock().await;
            clients.get(&client_id).map(DiscordVoiceClient::http)
        }?;
        let guild_id = match parse_discord_id("guild_id", &guild_id) {
            Ok(value) => value,
            Err(error) => {
                log(&format!("speaker profile resolution skipped: {error}"));
                return None;
            }
        };
        let user_id_number = match parse_discord_id("user_id", user_id) {
            Ok(value) => value,
            Err(error) => {
                log(&format!("speaker profile resolution skipped: {error}"));
                return None;
            }
        };

        match GuildId::new(guild_id)
            .member(http, UserId::new(user_id_number))
            .await
        {
            Ok(member) => {
                let profile = capture_user_from_member(&member);
                self.cache_speaker_profile(profile.clone()).await;
                Some(profile)
            }
            Err(error) => {
                log(&format!(
                    "speaker profile resolution failed for {user_id} in guild {guild_id}: {error}"
                ));
                None
            }
        }
    }

    async fn cached_speaker_profile(&self, user_id: &str) -> Option<CaptureUser> {
        self.speaker_profiles_lock
            .lock()
            .await
            .get(user_id)
            .cloned()
    }

    async fn cache_speaker_profile(&self, profile: CaptureUser) {
        self.speaker_profiles_lock
            .lock()
            .await
            .insert(profile.id.clone(), profile);
    }
}

fn capture_user_from_member(member: &Member) -> CaptureUser {
    CaptureUser {
        id: member.user.id.get().to_string(),
        display_name: member.display_name().to_string(),
        global_name: member.user.global_name.clone().unwrap_or_default(),
        name: member.user.name.clone(),
    }
}

fn voice_state_payload(state: &VoiceState) -> Value {
    let guild_id = state
        .guild_id
        .map(|value| value.get().to_string())
        .unwrap_or_default();
    let channel_id = state
        .channel_id
        .map(|value| value.get().to_string())
        .unwrap_or_default();
    let user_id = state.user_id.get().to_string();
    let member = state.member.as_ref();
    let display_name = member
        .map(|member| member.display_name().to_string())
        .unwrap_or_else(|| user_id.to_string());
    let username = member
        .map(|member| member.user.name.clone())
        .unwrap_or_default();
    let global_name = member
        .and_then(|member| member.user.global_name.clone())
        .unwrap_or_default();
    let nick = member
        .and_then(|member| member.nick.clone())
        .unwrap_or_default();
    let request_to_speak_timestamp = state
        .request_to_speak_timestamp
        .map(|value| value.to_string())
        .unwrap_or_default();
    json!({
        "guild_id": guild_id,
        "guildId": guild_id,
        "voice_channel_id": channel_id,
        "voiceChannelId": channel_id,
        "channelId": channel_id,
        "user_id": user_id,
        "userId": user_id,
        "speaker_user_id": user_id,
        "username": username,
        "global_name": global_name,
        "globalName": global_name,
        "nick": if nick.is_empty() { Value::Null } else { Value::String(nick) },
        "display_name": display_name,
        "member_display_name": display_name,
        "deaf": state.deaf,
        "mute": state.mute,
        "self_deaf": state.self_deaf,
        "self_mute": state.self_mute,
        "self_stream": state.self_stream.unwrap_or(false),
        "self_video": state.self_video,
        "suppress": state.suppress,
        "voice_session_id": state.session_id,
        "request_to_speak_timestamp": if request_to_speak_timestamp.is_empty() {
            Value::Null
        } else {
            Value::String(request_to_speak_timestamp)
        },
        "updated_at": crate::runtime::timeline::isoformat_z(None),
    })
}

fn sound_asset_path(cue: crate::runtime::DiscordVoicePlaybackCue) -> PathBuf {
    crate::config::voice_sound_dir().join(cue.asset_file_name())
}

fn playback_timeout() -> Duration {
    Duration::from_millis(crate::config::voice_sound_timeout_ms())
}
