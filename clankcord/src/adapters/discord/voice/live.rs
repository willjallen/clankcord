use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use serenity::Error as SerenityError;
use serenity::http::HttpError;
use serenity::model::gateway::Ready;
use serenity::model::guild::Member;
use serenity::model::id::{GuildId, UserId};
use serenity::model::voice::VoiceState;
use tokio::sync::Mutex;

use crate::Result;
use crate::adapters::discord::voice::capture::{CaptureUser, LiveCaptureSession, VoiceData};
use crate::adapters::discord::voice::client_connection::{
    DiscordVoiceClient, describe_error, join_voice_channel, leave_voice_channel,
    load_client_token_specs, parse_discord_id, play_voice_file, set_voice_deafen, set_voice_mute,
};
use crate::adapters::discord::voice::session::WakeProbeConfig;
use crate::adapters::discord::voice::types::LiveVoiceSession;
use crate::config::{local_tz, transcription_config};
use crate::errors::discord_tool_error;
use crate::runtime::timeline::{TimelineStore, isoformat_z, utc_now};
use crate::runtime::{
    DiscordVoiceDeafenOutput, DiscordVoiceDeafenPayload, DiscordVoiceJoinOutput,
    DiscordVoiceJoinPayload, DiscordVoiceLeaveOutput, DiscordVoiceLeavePayload,
    DiscordVoiceMuteOutput, DiscordVoiceMutePayload, DiscordVoicePlayAudioOutput,
    DiscordVoicePlayAudioPayload, DiscordVoiceStatusSnapshotOutput, RuntimeJobSink, VoiceBotStatus,
    log,
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

    pub async fn shutdown_gracefully(self: &Arc<Self>) -> Result<Value> {
        log("live voice adapter shutdown started");
        let sessions = {
            let mut sessions = self.capture_sessions_lock.lock().await;
            std::mem::take(&mut *sessions)
        };
        let mut finished_sessions = Vec::new();
        let mut created_audio_jobs = Vec::new();
        for (_, live_session) in sessions {
            let finished = {
                let mut live_session = live_session.lock().await;
                live_session.finish("runtime_shutdown".to_string(), local_tz())
            };
            for job in &finished.audio_jobs {
                let created = self.timeline_store.create_job(job.clone()).await?;
                created_audio_jobs.push(created.id);
            }
            self.persist_capture_session_status(&finished.metadata)
                .await;
            if !finished.capture_run_id.trim().is_empty() {
                self.timeline_store
                    .close_capture_run(
                        &finished.guild_id,
                        &finished.voice_channel_id,
                        &finished.capture_run_id,
                        None,
                        "runtime_shutdown",
                        "ended",
                    )
                    .await?;
            }
            finished_sessions.push(finished);
        }

        let mut leave_requests = Vec::new();
        let mut bot_statuses = Vec::new();
        let mut shard_managers = Vec::new();
        let mut client_tasks = Vec::new();
        let mut seen_leave_requests = BTreeSet::new();
        {
            let mut clients = self.voice_clients_lock.lock().await;
            for finished in &finished_sessions {
                let Some(client) = clients.get_mut(&finished.bot_id) else {
                    continue;
                };
                if seen_leave_requests.insert((finished.bot_id.clone(), finished.guild_id.clone()))
                {
                    leave_requests.push((
                        finished.bot_id.clone(),
                        finished.guild_id.clone(),
                        client.voice(),
                    ));
                }
            }
            for client in clients.values_mut() {
                if !client.current_guild_id.trim().is_empty()
                    && seen_leave_requests
                        .insert((client.bot_id.clone(), client.current_guild_id.clone()))
                {
                    leave_requests.push((
                        client.bot_id.clone(),
                        client.current_guild_id.clone(),
                        client.voice(),
                    ));
                }
                client.joining_live_session_id = None;
                client.active_live_session_id = None;
                client.current_guild_id.clear();
                client.current_channel_id.clear();
                client.ready = false;
                client.last_error = "runtime shutdown".to_string();
                bot_statuses.push(client.status());
                shard_managers.push(client.shard_manager());
                if let Some(task) = client.take_client_task() {
                    client_tasks.push((client.bot_id.clone(), task));
                }
            }
        }

        let mut leave_results = Vec::new();
        for (bot_id, guild_id, voice) in leave_requests {
            let result = match parse_discord_id("guild_id", &guild_id) {
                Ok(guild_id) => {
                    leave_voice_channel(voice, guild_id).await;
                    json!({"botId": bot_id, "guildId": guild_id.to_string(), "status": "left"})
                }
                Err(error) => {
                    json!({"botId": bot_id, "guildId": guild_id, "status": "invalid_guild", "error": error.to_string()})
                }
            };
            leave_results.push(result);
        }
        for status in &bot_statuses {
            self.persist_bot_status(status).await;
        }
        for shard_manager in shard_managers {
            shard_manager.shutdown_all().await;
        }

        let mut gateway_results = Vec::new();
        for (bot_id, mut task) in client_tasks {
            match tokio::time::timeout(Duration::from_secs(5), &mut task).await {
                Ok(Ok(())) => {
                    gateway_results.push(json!({"botId": bot_id, "status": "stopped"}));
                }
                Ok(Err(error)) => {
                    gateway_results.push(json!({"botId": bot_id, "status": "join_error", "error": error.to_string()}));
                }
                Err(_) => {
                    task.abort();
                    let _ = task.await;
                    gateway_results.push(json!({"botId": bot_id, "status": "aborted"}));
                }
            }
        }

        let report = json!({
            "finishedSessions": finished_sessions.len(),
            "audioJobs": created_audio_jobs,
            "voiceLeaves": leave_results,
            "gatewayTasks": gateway_results,
            "botStatuses": bot_statuses.len(),
        });
        log(&format!("live voice adapter shutdown complete: {report}"));
        Ok(report)
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
            client.joining_live_session_id = Some(session_id.clone());
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
        self.capture_sessions_lock.lock().await.insert(
            session_id.clone(),
            Arc::new(Mutex::new(LiveCaptureSession::new(
                session,
                self.minimum_utterance_ms,
                self.wake_probe,
            ))),
        );

        let join_started_at = utc_now();
        let join_started = Instant::now();
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
        let join_completed_at = utc_now();
        let join_total_ms = elapsed_ms(join_started.elapsed());

        let bot_status = {
            let mut clients = self.voice_clients_lock.lock().await;
            let client = clients.get_mut(&request.bot_id).ok_or_else(|| {
                discord_tool_error(format!("voice bot {} is not running", request.bot_id))
            })?;
            client.joining_live_session_id = None;
            client.active_live_session_id = Some(session_id.clone());
            client.current_guild_id = room.guild_id.clone();
            client.current_channel_id = room.channel_id.clone();
            client.last_error.clear();
            Some(client.status())
        };
        if let Some(status) = &bot_status {
            self.persist_bot_status(status).await;
        }
        let Some(session) = self.session(&session_id).await else {
            anyhow::bail!("live capture session {session_id} missing after successful voice join");
        };
        let (session_metadata, debug_notes) = {
            let mut live_session = session.lock().await;
            live_session.set_debug_note("joinStartedAt", isoformat_z(Some(join_started_at)));
            live_session.set_debug_note(
                "joinStartedAtMs",
                join_started_at.timestamp_millis().to_string(),
            );
            live_session.set_debug_note("joinReadyAt", isoformat_z(Some(join_completed_at)));
            live_session.set_debug_note(
                "joinReadyAtMs",
                join_completed_at.timestamp_millis().to_string(),
            );
            live_session.set_debug_note("joinTotalMs", join_total_ms.to_string());
            if let Some(bot_voice_state_at_ms) = live_session
                .debug_note("botVoiceStateAtMs")
                .and_then(|value| value.parse::<i64>().ok())
            {
                live_session.set_debug_note(
                    "botVoiceStateToJoinReadyMs",
                    (join_completed_at.timestamp_millis() - bot_voice_state_at_ms).to_string(),
                );
            }
            (
                live_session.metadata(local_tz()),
                live_session.debug_notes(),
            )
        };
        self.persist_capture_session_status(&session_metadata).await;
        self.persist_capture_session_debug_notes(&session_id, &debug_notes)
            .await;
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
        client.joining_live_session_id = None;
        client.last_error = error.to_string();
        let status = client.status();
        drop(clients);
        self.persist_bot_status(&status).await;
        Some(status)
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
            client.active_live_session_id = None;
            client.joining_live_session_id = None;
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
        if let Some(status) = &bot_status {
            self.persist_bot_status(status).await;
        }
        self.persist_capture_session_status(&finished.metadata)
            .await;
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

    pub(crate) async fn set_session_deafen(
        self: &Arc<Self>,
        request: DiscordVoiceDeafenPayload,
    ) -> Result<DiscordVoiceDeafenOutput> {
        let session_id = request.session_id.clone();
        let Some(live_session) = self.session(&session_id).await else {
            return Ok(DiscordVoiceDeafenOutput {
                session_id,
                status: "missing_session".to_string(),
                deafened: request.deafened,
                guild_id: String::new(),
                voice_channel_id: String::new(),
                message: "Voice session is not active.".to_string(),
            });
        };

        let session = {
            let mut live_session = live_session.lock().await;
            if request.deafened {
                live_session.set_deafened(true);
            }
            live_session.metadata(local_tz())
        };
        if request.deafened {
            self.persist_capture_session_status(&session).await;
        }

        let voice = {
            let clients = self.voice_clients_lock.lock().await;
            let client = clients.get(&session.bot_id).ok_or_else(|| {
                discord_tool_error(format!("voice bot {} is not running", session.bot_id))
            })?;
            client.voice()
        };
        let guild_id = parse_discord_id("guild_id", &session.guild_id)?;
        set_voice_deafen(voice, guild_id, request.deafened).await?;

        let session = {
            let mut live_session = live_session.lock().await;
            if !request.deafened {
                live_session.set_deafened(false);
            }
            live_session.metadata(local_tz())
        };
        self.persist_capture_session_status(&session).await;

        Ok(DiscordVoiceDeafenOutput {
            session_id,
            deafened: request.deafened,
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
        let mut statuses = Vec::new();
        for (_session_id, session) in sessions {
            let mut live_session = session.lock().await;
            audio_jobs
                .extend(live_session.flush_ready_buffers(self.max_segment_ms, self.silence_ms));
            statuses.push(live_session.metadata(local_tz()));
        }
        for job in audio_jobs {
            self.submit_capture_job(job).await;
        }
        for status in statuses {
            self.persist_capture_session_status(&status).await;
        }
        Ok(())
    }

    pub async fn bot_statuses(&self) -> Vec<VoiceBotStatus> {
        let clients = self.voice_clients_lock.lock().await;
        clients.values().map(DiscordVoiceClient::status).collect()
    }

    pub(crate) async fn voice_status_snapshot(&self) -> Result<DiscordVoiceStatusSnapshotOutput> {
        self.reconcile_voice_client_presence().await?;
        Ok(DiscordVoiceStatusSnapshotOutput {
            bots: self.bot_statuses().await,
            sessions: self.session_statuses().await,
        })
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

    async fn persist_bot_status(&self, status: &VoiceBotStatus) {
        if let Err(error) = self.timeline_store.upsert_voice_bot_state(status).await {
            log(&format!(
                "persisting voice bot status {} failed: {error}",
                status.bot_id
            ));
        }
    }

    async fn persist_capture_session_status(
        &self,
        status: &crate::runtime::VoiceCaptureSessionStatus,
    ) {
        if let Err(error) = self
            .timeline_store
            .upsert_capture_session_status(status)
            .await
        {
            log(&format!(
                "persisting capture session status {} failed: {error}",
                status.session_id
            ));
        }
    }

    async fn persist_capture_session_debug_notes(
        &self,
        session_id: &str,
        notes: &BTreeMap<String, String>,
    ) {
        if let Err(error) = self
            .timeline_store
            .set_capture_session_debug_notes(session_id, notes)
            .await
        {
            log(&format!(
                "persisting capture session debug notes {session_id} failed: {error}"
            ));
        }
    }

    async fn submit_capture_job(&self, job: crate::runtime::Job) {
        let job_id = job.id.clone();
        if let Err(error) = self.job_sink.submit(job).await {
            log(&format!("capture job submission failed {job_id}: {error}"));
        }
    }

    async fn reconcile_voice_client_presence(&self) -> Result<()> {
        let configured_guild_ids = configured_voice_guild_ids();
        let probes = {
            let clients = self.voice_clients_lock.lock().await;
            clients
                .values()
                .filter(|client| {
                    client.ready
                        && client.joining_live_session_id.is_none()
                        && !client.user_id.trim().is_empty()
                })
                .map(|client| {
                    let guild_ids = if client.current_guild_id.trim().is_empty() {
                        configured_guild_ids.clone()
                    } else {
                        vec![client.current_guild_id.clone()]
                    };
                    (
                        client.bot_id.clone(),
                        client.user_id.clone(),
                        client.http(),
                        guild_ids,
                    )
                })
                .filter(|(_, _, _, guild_ids)| !guild_ids.is_empty())
                .collect::<Vec<_>>()
        };

        for (bot_id, user_id, http, guild_ids) in probes {
            let discord_user_id = UserId::new(parse_discord_id("user_id", &user_id)?);
            let mut found_voice_state = false;
            let mut checked_guild = false;
            for guild_id in guild_ids {
                checked_guild = true;
                let discord_guild_id = GuildId::new(parse_discord_id("guild_id", &guild_id)?);
                match http
                    .get_user_voice_state(discord_guild_id, discord_user_id)
                    .await
                {
                    Ok(state) => {
                        let actual_guild_id = state
                            .guild_id
                            .map(|value| value.get().to_string())
                            .unwrap_or_else(|| guild_id.clone());
                        let actual_channel_id = state
                            .channel_id
                            .map(|value| value.get().to_string())
                            .unwrap_or_default();
                        self.apply_authoritative_voice_presence(
                            &bot_id,
                            &actual_guild_id,
                            &actual_channel_id,
                            "discord_voice_state",
                        )
                        .await;
                        found_voice_state = true;
                        break;
                    }
                    Err(error) if unknown_voice_state(&error) => {}
                    Err(error) => {
                        anyhow::bail!(
                            "Discord voice state lookup failed for {bot_id} in guild {guild_id}: {error}"
                        );
                    }
                }
            }
            if checked_guild && !found_voice_state {
                self.apply_authoritative_voice_presence(
                    &bot_id,
                    "",
                    "",
                    "discord_voice_state_absent",
                )
                .await;
            }
        }

        Ok(())
    }

    async fn apply_authoritative_voice_presence(
        &self,
        bot_id: &str,
        guild_id: &str,
        channel_id: &str,
        reason: &str,
    ) {
        let status = {
            let mut clients = self.voice_clients_lock.lock().await;
            let Some(client) = clients.get_mut(bot_id) else {
                return;
            };
            client.current_guild_id = guild_id.to_string();
            client.current_channel_id = channel_id.to_string();
            Some(client.status())
        };
        if let Some(status) = status {
            self.persist_bot_status(&status).await;
        }

        let stale_session_ids = self
            .live_session_ids_for_bot_outside(bot_id, guild_id, channel_id)
            .await;
        if stale_session_ids.is_empty() {
            return;
        }

        let stale_session_ids_set = stale_session_ids.iter().cloned().collect::<BTreeSet<_>>();
        let status = {
            let mut clients = self.voice_clients_lock.lock().await;
            clients.get_mut(bot_id).map(|client| {
                if client
                    .active_live_session_id
                    .as_ref()
                    .is_some_and(|session_id| stale_session_ids_set.contains(session_id))
                {
                    client.active_live_session_id = None;
                }
                if client
                    .joining_live_session_id
                    .as_ref()
                    .is_some_and(|session_id| stale_session_ids_set.contains(session_id))
                {
                    client.joining_live_session_id = None;
                }
                client.status()
            })
        };
        if let Some(status) = status {
            self.persist_bot_status(&status).await;
        }

        for session_id in stale_session_ids {
            self.finish_reconciled_session(&session_id, reason).await;
        }
    }

    async fn live_session_ids_for_bot_outside(
        &self,
        bot_id: &str,
        guild_id: &str,
        channel_id: &str,
    ) -> Vec<String> {
        let sessions = {
            let sessions = self.capture_sessions_lock.lock().await;
            sessions
                .iter()
                .map(|(session_id, session)| (session_id.clone(), session.clone()))
                .collect::<Vec<_>>()
        };
        let mut stale_session_ids = Vec::new();
        for (session_id, session) in sessions {
            let metadata = session.lock().await.metadata(local_tz());
            if metadata.bot_id != bot_id {
                continue;
            }
            if channel_id.trim().is_empty()
                || metadata.guild_id != guild_id
                || metadata.voice_channel_id != channel_id
            {
                stale_session_ids.push(session_id);
            }
        }
        stale_session_ids
    }

    async fn finish_reconciled_session(&self, session_id: &str, reason: &str) {
        let live_session = self.capture_sessions_lock.lock().await.remove(session_id);
        let Some(live_session) = live_session else {
            return;
        };
        let finished = {
            let mut live_session = live_session.lock().await;
            live_session.finish(reason.to_string(), local_tz())
        };
        for job in finished.audio_jobs {
            self.submit_capture_job(job).await;
        }
        self.persist_capture_session_status(&finished.metadata)
            .await;
        log(&format!(
            "finished voice session {} for {} after {reason}",
            finished.session_id, finished.bot_id
        ));
    }

    pub(super) async fn mark_client_ready(&self, bot_id: &str, ready: Ready) {
        let status = {
            let mut clients = self.voice_clients_lock.lock().await;
            if let Some(client) = clients.get_mut(bot_id) {
                client.ready = true;
                client.user_id = ready.user.id.get().to_string();
                client.username = ready.user.name.clone();
                client.last_error.clear();
                Some(client.status())
            } else {
                None
            }
        };
        if let Some(status) = status {
            self.persist_bot_status(&status).await;
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

        let (status, joining_live_session_id) = {
            let mut clients = self.voice_clients_lock.lock().await;
            let Some(client) = clients.get_mut(bot_id) else {
                return;
            };
            if client.user_id.is_empty() || client.user_id != user_id {
                return;
            }
            client.current_guild_id = guild_id.clone();
            client.current_channel_id = channel_id.clone();
            let joining_live_session_id = if client.joining_live_session_id.is_some()
                && !client.current_channel_id.is_empty()
            {
                client.joining_live_session_id.clone()
            } else {
                None
            };
            (Some(client.status()), joining_live_session_id)
        };
        if let Some(session_id) = joining_live_session_id {
            self.note_bot_join_voice_state(&session_id, &channel_id)
                .await;
        }
        if let Some(status) = status {
            self.persist_bot_status(&status).await;
        }
    }

    async fn note_bot_join_voice_state(&self, session_id: &str, channel_id: &str) {
        let Some(session) = self.session(session_id).await else {
            return;
        };
        let now = utc_now();
        let (status, debug_notes) = {
            let mut live_session = session.lock().await;
            live_session.set_debug_note("botVoiceStateAt", isoformat_z(Some(now)));
            live_session.set_debug_note("botVoiceStateAtMs", now.timestamp_millis().to_string());
            live_session.set_debug_note("botVoiceStateChannelId", channel_id.to_string());
            (
                live_session.metadata(local_tz()),
                live_session.debug_notes(),
            )
        };
        self.persist_capture_session_status(&status).await;
        self.persist_capture_session_debug_notes(session_id, &debug_notes)
            .await;
    }

    pub(super) async fn note_client_error(&self, bot_id: &str, error: &str) {
        let status = {
            let mut clients = self.voice_clients_lock.lock().await;
            if let Some(client) = clients.get_mut(bot_id) {
                client.ready = false;
                client.last_error = error.to_string();
                Some(client.status())
            } else {
                None
            }
        };
        if let Some(status) = status {
            self.persist_bot_status(&status).await;
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
        let (jobs, status) = {
            let mut live_session = session.lock().await;
            let jobs = live_session.note_client_disconnect(user_id);
            let status = live_session.metadata(local_tz());
            (jobs, status)
        };
        for job in jobs {
            self.submit_capture_job(job).await;
        }
        self.persist_capture_session_status(&status).await;
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

fn elapsed_ms(duration: Duration) -> i64 {
    duration.as_millis().min(i64::MAX as u128) as i64
}

fn unknown_voice_state(error: &SerenityError) -> bool {
    matches!(
        error,
        SerenityError::Http(HttpError::UnsuccessfulRequest(response))
            if response.status_code.as_u16() == 404 && response.error.code == 10065
    )
}

fn configured_voice_guild_ids() -> Vec<String> {
    let mut guild_ids = crate::config::app_config()
        .guilds
        .iter()
        .map(|guild| guild.guild_id.trim().to_string())
        .filter(|guild_id| !guild_id.is_empty())
        .collect::<BTreeSet<_>>();
    let control_guild_id = crate::config::app_config()
        .control
        .guild_id
        .trim()
        .to_string();
    if !control_guild_id.is_empty() {
        guild_ids.insert(control_guild_id);
    }
    guild_ids.into_iter().collect()
}
