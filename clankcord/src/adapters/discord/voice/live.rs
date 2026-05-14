use std::collections::BTreeMap;
use std::env;
use std::fmt;
use std::fs;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use anyhow::{Context as AnyhowContext, anyhow};
use serde_json::{Value, json};
use serenity::async_trait;
use serenity::builder::EditInteractionResponse;
use serenity::client::{Client, Context, EventHandler};
use serenity::gateway::ShardManager;
use serenity::model::application::{ComponentInteraction, Interaction};
use serenity::model::gateway::{GatewayIntents, Ready};
use serenity::model::id::{ChannelId, GuildId};
use serenity::model::voice::VoiceState;
use songbird::driver::{DecodeConfig, DecodeMode};
use songbird::events::{CoreEvent, Event, EventContext, EventHandler as VoiceEventHandler};
use songbird::serenity::SerenityInit;
use songbird::{Config as SongbirdConfig, Songbird};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use crate::Result;
use crate::adapters::discord::voice::artifacts::duration_ms_for_pcm;
use crate::adapters::discord::voice::capture::{
    CaptureUser, VoiceCaptureHandler, VoiceCaptureSink, VoiceData,
};
use crate::adapters::discord::voice::session::{
    AudioPipelineOutcome, SessionAudioPipeline, monotonic_seconds,
};
use crate::adapters::discord::voice::types::VoiceSession;
use crate::config::{local_tz, read_json, tokens_path};
use crate::errors::discord_tool_error;
use crate::runtime::core::execution::{
    JoinRoomEffectFuture, JoinRoomEffectRequest, JoinRoomEffectResult, LeaveRoomEffectFuture,
    LeaveRoomEffectRequest, LeaveRoomEffectResult, RuntimeEffects,
};
use crate::runtime::timeline::utc_now;
use crate::runtime::{Job, RuntimeBotStatus, RuntimeControlAction, RuntimeJobSink, log};

const DEFAULT_FLUSH_INTERVAL_SECONDS: f64 = 0.5;
const DEFAULT_SILENCE_MS: i64 = 1_000;
const DEFAULT_MAX_SEGMENT_MS: i64 = 8_000;

pub struct LiveVoiceAdapter {
    job_sink: RuntimeJobSink,
    bots: Mutex<BTreeMap<String, LiveBot>>,
    sessions: Mutex<BTreeMap<String, Arc<Mutex<LiveVoiceSession>>>>,
    voice_states: Mutex<BTreeMap<(String, String), String>>,
    flush_interval: Duration,
    silence_ms: i64,
    max_segment_ms: i64,
    minimum_utterance_ms: i64,
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
            .finish_non_exhaustive()
    }
}

impl RuntimeEffects for Arc<LiveVoiceAdapter> {
    fn join_room<'a>(&'a self, request: JoinRoomEffectRequest) -> JoinRoomEffectFuture<'a> {
        Box::pin(async move { LiveVoiceAdapter::join_assigned_room(self, request).await })
    }

    fn leave_room<'a>(&'a self, request: LeaveRoomEffectRequest) -> LeaveRoomEffectFuture<'a> {
        Box::pin(async move { LiveVoiceAdapter::finish_session(self, request).await })
    }
}

impl LiveVoiceAdapter {
    pub fn new(job_sink: RuntimeJobSink) -> Self {
        let payload = read_json(&crate::config::config_path(), json!({}));
        let transcription = payload
            .get("transcription")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let silence_ms = transcription
            .get("silenceMs")
            .and_then(Value::as_i64)
            .unwrap_or(DEFAULT_SILENCE_MS);
        let max_segment_ms = transcription
            .get("maxSegmentMs")
            .and_then(Value::as_i64)
            .unwrap_or(DEFAULT_MAX_SEGMENT_MS);
        let minimum_utterance_ms = transcription
            .get("minimumUtteranceMs")
            .and_then(Value::as_i64)
            .unwrap_or(350);
        Self {
            job_sink,
            bots: Mutex::new(BTreeMap::new()),
            sessions: Mutex::new(BTreeMap::new()),
            voice_states: Mutex::new(BTreeMap::new()),
            flush_interval: Duration::from_millis((DEFAULT_FLUSH_INTERVAL_SECONDS * 1000.0) as u64),
            silence_ms: silence_ms.max(0),
            max_segment_ms: max_segment_ms.max(250),
            minimum_utterance_ms: minimum_utterance_ms.max(0),
            no_token_warning_logged: AtomicBool::new(false),
        }
    }

    pub fn flush_interval(&self) -> Duration {
        self.flush_interval
    }

    pub async fn start_missing_bots(self: &Arc<Self>) -> Result<()> {
        let specs = load_bot_token_specs()?;
        if specs.is_empty() {
            let bots = self.bots.lock().await;
            if bots.is_empty() && !self.no_token_warning_logged.swap(true, Ordering::Relaxed) {
                log("no dedicated voice bot tokens configured; discord voice bots are disabled");
            }
            return Ok(());
        }
        self.no_token_warning_logged.store(false, Ordering::Relaxed);

        for (bot_id, token) in specs {
            if self.bots.lock().await.contains_key(&bot_id) {
                continue;
            }
            self.start_bot(bot_id, token).await?;
        }
        Ok(())
    }

    async fn start_bot(self: &Arc<Self>, bot_id: String, token: String) -> Result<()> {
        let voice = Songbird::serenity_from_config(
            SongbirdConfig::default().decode_mode(DecodeMode::Decode(DecodeConfig::default())),
        );
        let handler = LiveDiscordHandler {
            adapter: Arc::downgrade(self),
            bot_id: bot_id.clone(),
        };
        let intents = GatewayIntents::GUILDS
            | GatewayIntents::GUILD_VOICE_STATES
            | GatewayIntents::DIRECT_MESSAGES;
        let mut client = Client::builder(&token, intents)
            .event_handler(handler)
            .register_songbird_with(voice.clone())
            .await
            .with_context(|| format!("failed to build Discord voice client for {bot_id}"))?;
        let shard_manager = client.shard_manager.clone();

        {
            let mut bots = self.bots.lock().await;
            if bots.contains_key(&bot_id) {
                return Ok(());
            }
            bots.insert(
                bot_id.clone(),
                LiveBot {
                    bot_id: bot_id.clone(),
                    ready: false,
                    joining_session_id: None,
                    assigned_session_id: None,
                    current_guild_id: String::new(),
                    current_channel_id: String::new(),
                    last_error: String::new(),
                    user_id: String::new(),
                    username: String::new(),
                    voice,
                    shard_manager,
                    client_task: None,
                },
            );
        }
        self.sync_bot_status(&bot_id).await;

        let adapter = Arc::downgrade(self);
        let task_bot_id = bot_id.clone();
        let task = tokio::spawn(async move {
            loop {
                match client.start().await {
                    Ok(()) => {
                        if let Some(adapter) = adapter.upgrade() {
                            adapter
                                .note_bot_error(&task_bot_id, "gateway client stopped")
                                .await;
                        }
                        break;
                    }
                    Err(error) => {
                        if let Some(adapter) = adapter.upgrade() {
                            adapter
                                .note_bot_error(&task_bot_id, &error.to_string())
                                .await;
                        } else {
                            break;
                        }
                        tokio::time::sleep(Duration::from_secs(10)).await;
                    }
                }
            }
        });

        {
            let mut bots = self.bots.lock().await;
            if let Some(bot) = bots.get_mut(&bot_id) {
                bot.client_task = Some(task);
            }
        }
        log(&format!("starting bot {bot_id}"));
        self.sync_bot_status(&bot_id).await;
        Ok(())
    }

    pub async fn shutdown(&self) {
        let managers = {
            let bots = self.bots.lock().await;
            bots.values()
                .map(|bot| bot.shard_manager.clone())
                .collect::<Vec<_>>()
        };
        for runtime in managers {
            runtime.shutdown_all().await;
        }
    }

    async fn join_assigned_room(
        self: &Arc<Self>,
        request: JoinRoomEffectRequest,
    ) -> Result<JoinRoomEffectResult> {
        let room = request.room.clone();
        let session_id = request.capture_run_id.clone();
        let (voice, bot_user_id) = {
            let mut bots = self.bots.lock().await;
            let bot = bots.get_mut(&request.bot_id).ok_or_else(|| {
                discord_tool_error(format!("voice bot {} is not running", request.bot_id))
            })?;
            bot.joining_session_id = Some(session_id.clone());
            bot.last_error.clear();
            (
                bot.voice.clone(),
                first_non_empty([bot.user_id.clone(), request.bot_user_id.clone()]),
            )
        };

        let guild_id = parse_discord_id("guild_id", &room.guild_id)?;
        let channel_id = parse_discord_id("channel_id", &room.channel_id)?;
        fs::create_dir_all(request.session_dir.join("minutes"))?;
        let session = VoiceSession {
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
        self.sessions.lock().await.insert(
            session_id.clone(),
            Arc::new(Mutex::new(LiveVoiceSession {
                session,
                pipeline: SessionAudioPipeline::new()
                    .with_minimum_utterance_ms(self.minimum_utterance_ms),
                capture_sink: VoiceCaptureSink::new(&session_id),
                ssrc_users: BTreeMap::new(),
            })),
        );

        let call = voice.get_or_insert(GuildId::new(guild_id));
        let join = {
            let mut call = call.lock().await;
            call.remove_all_global_events();
            call.add_global_event(
                Event::Core(CoreEvent::SpeakingStateUpdate),
                LiveVoiceEventHandler {
                    adapter: Arc::downgrade(self),
                    session_id: session_id.clone(),
                },
            );
            call.add_global_event(
                Event::Core(CoreEvent::VoiceTick),
                LiveVoiceEventHandler {
                    adapter: Arc::downgrade(self),
                    session_id: session_id.clone(),
                },
            );
            call.add_global_event(
                Event::Core(CoreEvent::ClientDisconnect),
                LiveVoiceEventHandler {
                    adapter: Arc::downgrade(self),
                    session_id: session_id.clone(),
                },
            );
            call.join(ChannelId::new(channel_id)).await
        };
        let join_result = match join {
            Ok(join) => join.await,
            Err(error) => Err(error),
        };
        if let Err(error) = join_result {
            self.sessions.lock().await.remove(&session_id);
            {
                let mut call = call.lock().await;
                call.remove_all_global_events();
            }
            let error_text = error_chain(&error);
            let status = self.mark_join_failed(&request.bot_id, &error_text).await;
            return Err(discord_tool_error(format!(
                "failed to join {} with {}: {error_text}",
                room.channel_name, request.bot_id
            ))
            .context(format!("bot status after failure: {status:?}")));
        }

        let bot_status = {
            let mut bots = self.bots.lock().await;
            if let Some(bot) = bots.get_mut(&request.bot_id) {
                bot.joining_session_id = None;
                bot.assigned_session_id = Some(session_id.clone());
                bot.current_guild_id = room.guild_id.clone();
                bot.current_channel_id = room.channel_id.clone();
                bot.last_error.clear();
                Some(bot.status())
            } else {
                None
            }
        };
        Ok(JoinRoomEffectResult {
            status: "assigned".to_string(),
            session: Some(session_metadata),
            bot_status,
            message: String::new(),
        })
    }

    async fn mark_join_failed(&self, bot_id: &str, error: &str) -> Option<RuntimeBotStatus> {
        let mut bots = self.bots.lock().await;
        let bot = bots.get_mut(bot_id)?;
        bot.joining_session_id = None;
        bot.last_error = error.to_string();
        Some(bot.status())
    }

    async fn finish_session(
        self: &Arc<Self>,
        request: LeaveRoomEffectRequest,
    ) -> Result<LeaveRoomEffectResult> {
        let session_id = request.session_id;
        let live_session = self.sessions.lock().await.remove(&session_id);
        let Some(live_session) = live_session else {
            return Ok(LeaveRoomEffectResult {
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

        let (metadata, bot_id, guild_id, channel_id, capture_run_id, audio_jobs) = {
            let mut live_session = live_session.lock().await;
            live_session.session.finalizing = true;
            let user_ids = live_session
                .session
                .buffers
                .keys()
                .cloned()
                .collect::<Vec<_>>();
            let pipeline = live_session.pipeline.clone();
            let mut audio_jobs = Vec::new();
            for user_id in user_ids {
                match pipeline.flush_speaker(&mut live_session.session, &user_id) {
                    Ok(outcome) => collect_audio_job(outcome, &mut audio_jobs),
                    Err(error) => log(&format!("voice final flush failed: {error}")),
                }
            }
            let ended_at = utc_now();
            live_session.session.ended_at = Some(ended_at);
            live_session.session.finalizing = false;
            live_session
                .session
                .debug_notes
                .insert("leaveReason".to_string(), request.reason);
            let metadata = live_session.session.metadata(local_tz());
            (
                metadata,
                live_session.session.bot_id.clone(),
                live_session.session.room.guild_id.clone(),
                live_session.session.room.channel_id.clone(),
                first_non_empty([
                    live_session.session.capture_run_id.clone(),
                    live_session.session.session_id.clone(),
                ]),
                audio_jobs,
            )
        };

        let voice = {
            let mut bots = self.bots.lock().await;
            let voice = bots.get(&bot_id).map(|bot| bot.voice.clone());
            if let Some(bot) = bots.get_mut(&bot_id) {
                bot.assigned_session_id = None;
                bot.joining_session_id = None;
                bot.current_guild_id.clear();
                bot.current_channel_id.clear();
            }
            voice
        };
        if let (Some(voice), Ok(guild_id_raw)) = (voice, guild_id.parse::<u64>()) {
            let _ = voice.remove(GuildId::new(guild_id_raw)).await;
        }
        let bot_status = {
            let bots = self.bots.lock().await;
            bots.get(&bot_id).map(LiveBot::status)
        };
        Ok(LeaveRoomEffectResult {
            session_id,
            status: "ended".to_string(),
            session: Some(metadata),
            bot_status,
            guild_id,
            voice_channel_id: channel_id,
            capture_run_id,
            audio_jobs,
        })
    }

    pub async fn flush_ready_buffers(&self) -> Result<()> {
        let sessions = {
            let sessions = self.sessions.lock().await;
            sessions
                .iter()
                .map(|(id, session)| (id.clone(), session.clone()))
                .collect::<Vec<_>>()
        };
        let now = monotonic_seconds();
        let mut audio_jobs = Vec::new();
        for (_session_id, session) in sessions {
            let mut live_session = session.lock().await;
            if live_session.session.ended_at.is_some() || live_session.session.finalizing {
                continue;
            }
            let user_ids = live_session
                .session
                .buffers
                .iter()
                .filter_map(|(user_id, speaker)| {
                    if speaker.pcm.is_empty() || speaker.flush_in_flight {
                        return None;
                    }
                    let buffered_duration_ms = duration_ms_for_pcm(&speaker.pcm);
                    if buffered_duration_ms >= self.max_segment_ms
                        || now - speaker.last_packet_monotonic >= self.silence_ms as f64 / 1000.0
                    {
                        Some(user_id.clone())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>();
            if user_ids.is_empty() {
                continue;
            }
            let pipeline = live_session.pipeline.clone();
            for user_id in user_ids {
                match pipeline.flush_speaker(&mut live_session.session, &user_id) {
                    Ok(outcome) => collect_audio_job(outcome, &mut audio_jobs),
                    Err(error) => log(&format!("voice buffer flush failed: {error}")),
                }
            }
        }
        for job in audio_jobs {
            self.job_sink.submit_detached(job);
        }
        Ok(())
    }

    pub async fn bot_statuses(&self) -> Vec<RuntimeBotStatus> {
        let bots = self.bots.lock().await;
        bots.values().map(LiveBot::status).collect()
    }

    pub async fn session_statuses(&self) -> Vec<crate::runtime::RuntimeSessionStatus> {
        let sessions = {
            let sessions = self.sessions.lock().await;
            sessions.values().cloned().collect::<Vec<_>>()
        };
        let mut statuses = Vec::new();
        for session in sessions {
            statuses.push(session.lock().await.session.metadata(local_tz()));
        }
        statuses
    }

    async fn on_bot_ready(&self, bot_id: &str, ready: Ready) {
        {
            let mut bots = self.bots.lock().await;
            if let Some(bot) = bots.get_mut(bot_id) {
                bot.ready = true;
                bot.user_id = ready.user.id.get().to_string();
                bot.username = ready.user.name.clone();
                bot.last_error.clear();
            }
        }
        log(&format!("bot {bot_id} ready as {}", ready.user.name));
        self.sync_bot_status(bot_id).await;
    }

    async fn on_voice_state_update(&self, bot_id: &str, state: &VoiceState) {
        if let Some(guild_id) = state.guild_id {
            let key = (guild_id.get().to_string(), state.user_id.get().to_string());
            let mut voice_states = self.voice_states.lock().await;
            if let Some(channel_id) = state.channel_id {
                voice_states.insert(key, channel_id.get().to_string());
            } else {
                voice_states.remove(&key);
            }
        }

        let should_sync = {
            let mut bots = self.bots.lock().await;
            let Some(bot) = bots.get_mut(bot_id) else {
                return;
            };
            if bot.user_id.is_empty() || bot.user_id != state.user_id.get().to_string() {
                return;
            }
            bot.current_guild_id = state
                .guild_id
                .map(|value| value.get().to_string())
                .unwrap_or_default();
            bot.current_channel_id = state
                .channel_id
                .map(|value| value.get().to_string())
                .unwrap_or_default();
            true
        };
        if should_sync {
            self.sync_bot_status(bot_id).await;
        }
    }

    async fn note_bot_error(&self, bot_id: &str, error: &str) {
        {
            let mut bots = self.bots.lock().await;
            if let Some(bot) = bots.get_mut(bot_id) {
                bot.ready = false;
                bot.last_error = error.to_string();
            }
        }
        log(&format!("bot {bot_id} error: {error}"));
        self.sync_bot_status(bot_id).await;
    }

    async fn sync_bot_status(&self, bot_id: &str) {
        let _ = bot_id;
    }

    async fn handle_component_interaction(
        self: &Arc<Self>,
        ctx: Context,
        component: ComponentInteraction,
    ) {
        let custom_id = component.data.custom_id.trim().to_string();
        let action = if let Some(job_id) = custom_id.strip_prefix("clawcord_voice_confirm:") {
            ("approve", job_id.trim().to_string())
        } else if let Some(job_id) = custom_id.strip_prefix("clawcord_voice_cancel:") {
            ("cancel", job_id.trim().to_string())
        } else {
            return;
        };
        let actor_user_id = component.user.id.get().to_string();
        if let Err(error) = component.defer(&ctx.http).await {
            log(&format!("confirmation interaction defer failed: {error}"));
        }
        let control_action = if action.0 == "approve" {
            RuntimeControlAction::ApproveConfirmation
        } else {
            RuntimeControlAction::CancelConfirmation
        };
        let result = self
            .job_sink
            .submit_runtime_control_for_target(&action.1, control_action, actor_user_id)
            .await;
        let content = match result {
            Ok(_) if action.0 == "approve" => {
                format!("Clanky voice confirmation `{}` approval queued.", action.1)
            }
            Ok(_) => format!(
                "Clanky voice confirmation `{}` cancellation queued.",
                action.1
            ),
            Err(error) => format!(
                "Could not complete Clanky voice confirmation `{}`: {}",
                action.1, error
            ),
        };
        if let Err(error) = component
            .edit_response(
                &ctx.http,
                EditInteractionResponse::new()
                    .content(clipped_text(&content, 1900))
                    .components(Vec::new()),
            )
            .await
        {
            log(&format!("confirmation interaction finish failed: {error}"));
        }
    }

    async fn handle_speaking_state(
        &self,
        session_id: &str,
        ssrc: u32,
        user_id: &str,
        active: bool,
    ) {
        let session = self.session(session_id).await;
        let Some(session) = session else {
            return;
        };
        let mut live_session = session.lock().await;
        let pipeline = live_session.pipeline.clone();
        let user = CaptureUser {
            id: user_id.to_string(),
            display_name: user_id.to_string(),
            global_name: String::new(),
            name: user_id.to_string(),
        };
        live_session.ssrc_users.insert(ssrc, user.clone());
        let mut handler = SessionCaptureHandler {
            pipeline,
            session: &mut live_session.session,
        };
        handler.handle_speaking_state(session_id, &user.id, &user.display_name, &user.name, active);
    }

    async fn handle_client_disconnect(&self, session_id: &str, user_id: &str) {
        let session = self.session(session_id).await;
        let Some(session) = session else {
            return;
        };
        let mut live_session = session.lock().await;
        live_session
            .ssrc_users
            .retain(|_, user| user.id.as_str() != user_id);
        let pipeline = live_session.pipeline.clone();
        {
            let mut handler = SessionCaptureHandler {
                pipeline: pipeline.clone(),
                session: &mut live_session.session,
            };
            handler.handle_speaking_state(session_id, user_id, user_id, user_id, false);
        }
        match pipeline.flush_speaker(&mut live_session.session, user_id) {
            Ok(outcome) => {
                if let Some(job) = audio_job_from_outcome(outcome) {
                    self.job_sink.submit_detached(job);
                }
            }
            Err(error) => log(&format!("voice disconnect flush failed: {error}")),
        }
    }

    async fn handle_voice_tick(
        &self,
        session_id: &str,
        speaking: Vec<(u32, VoiceData)>,
        silent: Vec<u32>,
    ) {
        let session = self.session(session_id).await;
        let Some(session) = session else {
            return;
        };
        let (guild_id, channel_id, bot_user_id) = {
            let live_session = session.lock().await;
            (
                live_session.session.room.guild_id.clone(),
                live_session.session.room.channel_id.clone(),
                live_session.session.bot_user_id.clone(),
            )
        };
        let channel_users = self
            .voice_users_for_channel(&guild_id, &channel_id, &bot_user_id)
            .await;
        let mut live_session = session.lock().await;
        for (ssrc, data) in speaking {
            let user = live_session
                .ssrc_users
                .get(&ssrc)
                .cloned()
                .or_else(|| fallback_user_for_ssrc(ssrc, &channel_users));
            if let Some(user) = user.as_ref() {
                live_session.ssrc_users.insert(ssrc, user.clone());
            }
            live_session.write_voice_data(user, data);
        }
        for ssrc in silent {
            let Some(user) = live_session.ssrc_users.get(&ssrc).cloned() else {
                continue;
            };
            live_session.write_voice_data(
                Some(user),
                VoiceData {
                    user: None,
                    pcm: Vec::new(),
                    has_packet: true,
                    is_silence: true,
                },
            );
        }
    }

    async fn voice_users_for_channel(
        &self,
        guild_id: &str,
        channel_id: &str,
        bot_user_id: &str,
    ) -> Vec<CaptureUser> {
        let voice_states = self.voice_states.lock().await;
        voice_states
            .iter()
            .filter_map(|((state_guild_id, user_id), state_channel_id)| {
                if state_guild_id != guild_id
                    || state_channel_id != channel_id
                    || user_id == bot_user_id
                {
                    return None;
                }
                Some(CaptureUser {
                    id: user_id.clone(),
                    display_name: user_id.clone(),
                    global_name: String::new(),
                    name: user_id.clone(),
                })
            })
            .collect()
    }

    async fn session(&self, session_id: &str) -> Option<Arc<Mutex<LiveVoiceSession>>> {
        self.sessions.lock().await.get(session_id).cloned()
    }
}

struct LiveBot {
    bot_id: String,
    ready: bool,
    joining_session_id: Option<String>,
    assigned_session_id: Option<String>,
    current_guild_id: String,
    current_channel_id: String,
    last_error: String,
    user_id: String,
    username: String,
    voice: Arc<Songbird>,
    shard_manager: Arc<ShardManager>,
    client_task: Option<JoinHandle<()>>,
}

impl LiveBot {
    fn status(&self) -> RuntimeBotStatus {
        RuntimeBotStatus {
            bot_id: self.bot_id.clone(),
            ready: self.ready,
            joining_session_id: self.joining_session_id.clone().unwrap_or_default(),
            assigned_session_id: self.assigned_session_id.clone().unwrap_or_default(),
            current_guild_id: self.current_guild_id.clone(),
            current_channel_id: self.current_channel_id.clone(),
            last_error: self.last_error.clone(),
            pending_disconnect_events: 0,
            pending_disconnect_until: 0,
            user_id: self.user_id.clone(),
            username: self.username.clone(),
            gateway_running: self
                .client_task
                .as_ref()
                .is_some_and(|task| !task.is_finished()),
            receive_backend: "songbird".to_string(),
        }
    }
}

struct LiveVoiceSession {
    session: VoiceSession,
    pipeline: SessionAudioPipeline,
    capture_sink: VoiceCaptureSink,
    ssrc_users: BTreeMap<u32, CaptureUser>,
}

impl LiveVoiceSession {
    fn write_voice_data(&mut self, user: Option<CaptureUser>, mut data: VoiceData) {
        if data.user.is_none() {
            data.user = user;
        }
        let mut sink = std::mem::replace(
            &mut self.capture_sink,
            VoiceCaptureSink::new(&self.session.session_id),
        );
        let mut handler = SessionCaptureHandler {
            pipeline: self.pipeline.clone(),
            session: &mut self.session,
        };
        sink.write(&mut handler, data);
        self.capture_sink = sink;
    }
}

fn fallback_user_for_ssrc(ssrc: u32, channel_users: &[CaptureUser]) -> Option<CaptureUser> {
    if channel_users.len() == 1 {
        return channel_users.first().cloned();
    }
    Some(CaptureUser {
        id: format!("ssrc:{ssrc}"),
        display_name: format!("unknown-ssrc-{ssrc}"),
        global_name: String::new(),
        name: format!("unknown-ssrc-{ssrc}"),
    })
}

struct SessionCaptureHandler<'a> {
    pipeline: SessionAudioPipeline,
    session: &'a mut VoiceSession,
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

struct LiveDiscordHandler {
    adapter: Weak<LiveVoiceAdapter>,
    bot_id: String,
}

#[async_trait]
impl EventHandler for LiveDiscordHandler {
    async fn ready(&self, _ctx: Context, ready: Ready) {
        if let Some(adapter) = self.adapter.upgrade() {
            adapter.on_bot_ready(&self.bot_id, ready).await;
        }
    }

    async fn voice_state_update(&self, _ctx: Context, _old: Option<VoiceState>, new: VoiceState) {
        if let Some(adapter) = self.adapter.upgrade() {
            adapter.on_voice_state_update(&self.bot_id, &new).await;
        }
    }

    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        let Interaction::Component(component) = interaction else {
            return;
        };
        if let Some(adapter) = self.adapter.upgrade() {
            adapter.handle_component_interaction(ctx, component).await;
        }
    }
}

struct LiveVoiceEventHandler {
    adapter: Weak<LiveVoiceAdapter>,
    session_id: String,
}

#[async_trait]
impl VoiceEventHandler for LiveVoiceEventHandler {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
        let Some(adapter) = self.adapter.upgrade() else {
            return None;
        };
        match ctx {
            EventContext::SpeakingStateUpdate(speaking) => {
                if let Some(user_id) = speaking.user_id {
                    adapter
                        .handle_speaking_state(
                            &self.session_id,
                            speaking.ssrc,
                            &user_id.0.to_string(),
                            speaking.speaking.microphone() || speaking.speaking.soundshare(),
                        )
                        .await;
                }
            }
            EventContext::VoiceTick(tick) => {
                let speaking = tick
                    .speaking
                    .iter()
                    .map(|(ssrc, data)| {
                        (
                            *ssrc,
                            VoiceData {
                                user: None,
                                pcm: data
                                    .decoded_voice
                                    .as_ref()
                                    .map(|samples| pcm_i16_to_le_bytes(samples))
                                    .unwrap_or_default(),
                                has_packet: data.packet.is_some(),
                                is_silence: false,
                            },
                        )
                    })
                    .collect::<Vec<_>>();
                let silent = tick.silent.iter().copied().collect::<Vec<_>>();
                adapter
                    .handle_voice_tick(&self.session_id, speaking, silent)
                    .await;
            }
            EventContext::ClientDisconnect(disconnect) => {
                adapter
                    .handle_client_disconnect(&self.session_id, &disconnect.user_id.0.to_string())
                    .await;
            }
            _ => {}
        }
        None
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

fn parse_discord_id(label: &str, value: &str) -> Result<u64> {
    value
        .trim()
        .parse::<u64>()
        .map_err(|_| anyhow!("invalid Discord {label}: {value:?}"))
}

fn pcm_i16_to_le_bytes(samples: &[i16]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(samples.len() * 2);
    for sample in samples {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    bytes
}

fn load_bot_token_specs() -> Result<Vec<(String, String)>> {
    parse_bot_token_specs(raw_bot_token_lines()?)
}

fn raw_bot_token_lines() -> Result<Vec<String>> {
    let mut lines = Vec::new();
    if let Ok(value) = env::var("CLAWCORD_BOT_TOKENS") {
        lines.extend(value.lines().map(str::to_string));
    }
    let path = tokens_path();
    if path.is_file() {
        lines.extend(
            fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))?
                .lines()
                .map(str::to_string),
        );
    }
    Ok(lines)
}

fn parse_bot_token_specs(lines: Vec<String>) -> Result<Vec<(String, String)>> {
    let mut specs = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    let mut auto_index = 1;
    for raw_line in lines {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (mut bot_id, token) = if let Some((left, right)) = line.split_once('=') {
            (left.trim().to_string(), right.trim().to_string())
        } else if !line.starts_with("mfa.") {
            if let Some((left, right)) = line.split_once(':') {
                (left.trim().to_string(), right.trim().to_string())
            } else {
                (String::new(), line.to_string())
            }
        } else {
            (String::new(), line.to_string())
        };
        if token.is_empty() {
            continue;
        }
        if bot_id.is_empty() {
            bot_id = format!("voice-{auto_index}");
            auto_index += 1;
        }
        if seen.insert(bot_id.clone()) {
            specs.push((bot_id, token));
        }
    }
    Ok(specs)
}

fn clipped_text(content: &str, limit: usize) -> String {
    let mut clipped = content.chars().take(limit).collect::<String>();
    if clipped.len() < content.len() {
        clipped.push_str("...");
    }
    clipped
}

fn error_chain(error: &(dyn std::error::Error + 'static)) -> String {
    let mut parts = vec![error.to_string()];
    let mut source = error.source();
    while let Some(error) = source {
        let text = error.to_string();
        if !text.trim().is_empty() {
            parts.push(text);
        }
        source = error.source();
    }
    parts.join(": ")
}

fn first_non_empty(values: impl IntoIterator<Item = String>) -> String {
    values
        .into_iter()
        .find(|value| !value.trim().is_empty())
        .unwrap_or_default()
}
