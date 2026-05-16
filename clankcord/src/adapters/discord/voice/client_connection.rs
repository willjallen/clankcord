use std::collections::BTreeSet;
use std::io::Cursor;
use std::path::Path;
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use anyhow::{Context as AnyhowContext, anyhow};
use serenity::async_trait;
use serenity::client::{Client, Context, EventHandler};
use serenity::gateway::ShardManager;
use serenity::http::Http;
use serenity::model::application::Interaction;
use serenity::model::gateway::{GatewayIntents, Ready};
use serenity::model::id::{ChannelId, GuildId};
use serenity::model::voice::VoiceState;
use songbird::driver::{DecodeConfig, DecodeMode};
use songbird::events::{CoreEvent, Event, EventContext, EventHandler as VoiceEventHandler};
use songbird::input::RawAdapter;
use songbird::serenity::SerenityInit;
use songbird::tracks::PlayMode;
use songbird::{Config as SongbirdConfig, Songbird};
use tokio::task::JoinHandle;

use crate::Result;
use crate::adapters::discord::gateway::components;
use crate::adapters::discord::voice::capture::VoiceData;
use crate::adapters::discord::voice::live::LiveVoiceAdapter;
use crate::config;
use crate::runtime::VoiceBotStatus;

pub(super) struct DiscordVoiceClient {
    pub(super) bot_id: String,
    pub(super) ready: bool,
    pub(super) joining_live_session_id: Option<String>,
    pub(super) active_live_session_id: Option<String>,
    pub(super) current_guild_id: String,
    pub(super) current_channel_id: String,
    pub(super) last_error: String,
    pub(super) user_id: String,
    pub(super) username: String,
    http: Arc<Http>,
    voice: Arc<Songbird>,
    shard_manager: Arc<ShardManager>,
    client_task: Option<JoinHandle<()>>,
}

impl DiscordVoiceClient {
    pub(super) async fn start(
        adapter: &Arc<LiveVoiceAdapter>,
        bot_id: String,
        token: String,
    ) -> Result<Self> {
        let voice = Songbird::serenity_from_config(
            SongbirdConfig::default().decode_mode(DecodeMode::Decode(DecodeConfig::default())),
        );
        let handler = DiscordGatewayHandler {
            adapter: Arc::downgrade(adapter),
            bot_id: bot_id.clone(),
        };
        let intents = GatewayIntents::GUILDS
            | GatewayIntents::GUILD_VOICE_STATES
            | GatewayIntents::GUILD_MEMBERS
            | GatewayIntents::DIRECT_MESSAGES;
        let client = Client::builder(&token, intents)
            .event_handler(handler)
            .register_songbird_with(voice.clone())
            .await
            .with_context(|| format!("failed to build Discord voice client for {bot_id}"))?;
        let http = client.http.clone();
        let shard_manager = client.shard_manager.clone();
        let client_task = spawn_gateway_task(Arc::downgrade(adapter), bot_id.clone(), client);

        Ok(Self {
            bot_id,
            ready: false,
            joining_live_session_id: None,
            active_live_session_id: None,
            current_guild_id: String::new(),
            current_channel_id: String::new(),
            last_error: String::new(),
            user_id: String::new(),
            username: String::new(),
            http,
            voice,
            shard_manager,
            client_task: Some(client_task),
        })
    }

    pub(super) fn discord_user_id(&self) -> Result<String> {
        if self.user_id.trim().is_empty() {
            anyhow::bail!("voice client {} is not ready", self.bot_id);
        }
        Ok(self.user_id.clone())
    }

    pub(super) fn voice(&self) -> Arc<Songbird> {
        self.voice.clone()
    }

    pub(super) fn http(&self) -> Arc<Http> {
        self.http.clone()
    }

    pub(super) async fn shutdown(&self) {
        self.shard_manager.shutdown_all().await;
    }

    pub(super) fn status(&self) -> VoiceBotStatus {
        VoiceBotStatus {
            bot_id: self.bot_id.clone(),
            ready: self.ready,
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

pub(super) async fn join_voice_channel(
    adapter: &Arc<LiveVoiceAdapter>,
    voice: Arc<Songbird>,
    session_id: &str,
    guild_id: u64,
    channel_id: u64,
) -> Result<()> {
    let call = voice.get_or_insert(GuildId::new(guild_id));
    let join = {
        let mut call = call.lock().await;
        call.remove_all_global_events();
        call.add_global_event(
            Event::Core(CoreEvent::SpeakingStateUpdate),
            VoiceReceiveHandler {
                adapter: Arc::downgrade(adapter),
                session_id: session_id.to_string(),
            },
        );
        call.add_global_event(
            Event::Core(CoreEvent::VoiceTick),
            VoiceReceiveHandler {
                adapter: Arc::downgrade(adapter),
                session_id: session_id.to_string(),
            },
        );
        call.add_global_event(
            Event::Core(CoreEvent::ClientDisconnect),
            VoiceReceiveHandler {
                adapter: Arc::downgrade(adapter),
                session_id: session_id.to_string(),
            },
        );
        call.join(ChannelId::new(channel_id)).await
    };
    let join_result = match join {
        Ok(join) => join.await,
        Err(error) => Err(error),
    };
    if let Err(error) = join_result {
        {
            let mut call = call.lock().await;
            call.remove_all_global_events();
        }
        return Err(anyhow::Error::new(error).context("failed to join voice channel"));
    }
    Ok(())
}

pub(super) async fn leave_voice_channel(voice: Arc<Songbird>, guild_id: u64) {
    let _ = voice.remove(GuildId::new(guild_id)).await;
}

pub(super) async fn set_voice_mute(voice: Arc<Songbird>, guild_id: u64, muted: bool) -> Result<()> {
    let call = voice
        .get(GuildId::new(guild_id))
        .ok_or_else(|| anyhow!("voice call for guild {guild_id} is not active"))?;
    let mut call = call.lock().await;
    call.mute(muted)
        .await
        .map_err(|error| anyhow!("failed to set voice mute={muted}: {error}"))
}

pub(super) async fn play_voice_file(
    voice: Arc<Songbird>,
    guild_id: u64,
    path: &Path,
    timeout: Duration,
) -> Result<Duration> {
    let (input, audio_duration) = raw_input_from_wav(path)?;
    let call = voice
        .get(GuildId::new(guild_id))
        .ok_or_else(|| anyhow!("voice call for guild {guild_id} is not active"))?;
    let handle = {
        let mut call = call.lock().await;
        call.play_input(input)
    };
    handle
        .make_playable_async()
        .await
        .map_err(|error| anyhow!("failed to prepare playback source: {error}"))?;
    wait_for_track_end(&handle, timeout).await?;
    Ok(audio_duration)
}

pub(super) fn load_client_token_specs() -> Result<Vec<(String, String)>> {
    parse_client_token_specs(raw_client_token_lines()?)
}

pub(super) fn parse_discord_id(label: &str, value: &str) -> Result<u64> {
    value
        .trim()
        .parse::<u64>()
        .map_err(|_| anyhow!("invalid Discord {label}: {value:?}"))
}

pub(super) fn describe_error(error: &anyhow::Error) -> String {
    error
        .chain()
        .map(ToString::to_string)
        .filter(|part| !part.trim().is_empty())
        .collect::<Vec<_>>()
        .join(": ")
}

fn spawn_gateway_task(
    adapter: Weak<LiveVoiceAdapter>,
    bot_id: String,
    mut client: Client,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match client.start().await {
                Ok(()) => {
                    if let Some(adapter) = adapter.upgrade() {
                        adapter
                            .note_client_error(&bot_id, "gateway client stopped")
                            .await;
                    }
                    break;
                }
                Err(error) => {
                    if let Some(adapter) = adapter.upgrade() {
                        adapter.note_client_error(&bot_id, &error.to_string()).await;
                    } else {
                        break;
                    }
                    tokio::time::sleep(Duration::from_secs(10)).await;
                }
            }
        }
    })
}

fn raw_client_token_lines() -> Result<Vec<String>> {
    config::raw_voice_bot_token_lines()
}

fn parse_client_token_specs(lines: Vec<String>) -> Result<Vec<(String, String)>> {
    let mut specs = Vec::new();
    let mut seen = BTreeSet::new();
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

fn raw_input_from_wav(path: &Path) -> Result<(songbird::input::Input, Duration)> {
    let mut reader = hound::WavReader::open(path)
        .with_context(|| format!("failed to open voice cue {}", path.display()))?;
    let spec = reader.spec();
    let channels = u32::from(spec.channels);
    if channels == 0 || channels > 2 {
        anyhow::bail!(
            "voice cue {} must be mono or stereo, found {} channels",
            path.display(),
            channels
        );
    }
    if spec.sample_rate == 0 {
        anyhow::bail!("voice cue {} has no sample rate", path.display());
    }

    let mut pcm = Vec::new();
    match spec.sample_format {
        hound::SampleFormat::Float => {
            for sample in reader.samples::<f32>() {
                push_f32_sample(&mut pcm, sample?);
            }
        }
        hound::SampleFormat::Int => match spec.bits_per_sample {
            8 => {
                for sample in reader.samples::<i8>() {
                    push_scaled_sample(&mut pcm, i32::from(sample?), 7);
                }
            }
            16 => {
                for sample in reader.samples::<i16>() {
                    push_scaled_sample(&mut pcm, i32::from(sample?), 15);
                }
            }
            24 => {
                for sample in reader.samples::<i32>() {
                    push_scaled_sample(&mut pcm, sample?, 23);
                }
            }
            32 => {
                for sample in reader.samples::<i32>() {
                    push_scaled_sample(&mut pcm, sample?, 31);
                }
            }
            bits => anyhow::bail!(
                "voice cue {} uses unsupported PCM depth {}",
                path.display(),
                bits
            ),
        },
    }

    let sample_count = pcm.len() / std::mem::size_of::<f32>();
    let frames = sample_count as f64 / f64::from(channels);
    let seconds = frames / f64::from(spec.sample_rate);
    let input = RawAdapter::new(Cursor::new(pcm), spec.sample_rate, channels).into();
    Ok((input, Duration::from_secs_f64(seconds.max(0.0))))
}

fn push_scaled_sample(pcm: &mut Vec<u8>, sample: i32, scale_bits: u32) {
    let scale = (1_i64 << scale_bits) as f32;
    push_f32_sample(pcm, sample as f32 / scale);
}

fn push_f32_sample(pcm: &mut Vec<u8>, sample: f32) {
    pcm.extend_from_slice(&sample.clamp(-1.0, 1.0).to_le_bytes());
}

async fn wait_for_track_end(
    handle: &songbird::tracks::TrackHandle,
    timeout: Duration,
) -> Result<()> {
    let started = Instant::now();
    loop {
        if started.elapsed() > timeout {
            let _ = handle.stop();
            anyhow::bail!("voice cue playback exceeded {} ms", timeout.as_millis());
        }
        match handle.get_info().await {
            Ok(state) if state.playing.is_done() => {
                if matches!(state.playing, PlayMode::Errored(_)) {
                    anyhow::bail!("voice cue playback errored");
                }
                return Ok(());
            }
            Ok(_) => tokio::time::sleep(Duration::from_millis(20)).await,
            Err(_) => return Ok(()),
        }
    }
}

struct DiscordGatewayHandler {
    adapter: Weak<LiveVoiceAdapter>,
    bot_id: String,
}

#[async_trait]
impl EventHandler for DiscordGatewayHandler {
    async fn ready(&self, _ctx: Context, ready: Ready) {
        if let Some(adapter) = self.adapter.upgrade() {
            adapter.mark_client_ready(&self.bot_id, ready).await;
        }
    }

    async fn voice_state_update(&self, _ctx: Context, old: Option<VoiceState>, new: VoiceState) {
        if let Some(adapter) = self.adapter.upgrade() {
            adapter.note_voice_state(&self.bot_id, old, new).await;
        }
    }

    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        let Interaction::Component(component) = interaction else {
            return;
        };
        if let Some(adapter) = self.adapter.upgrade() {
            components::handle_component_interaction(adapter.job_sink(), ctx, component).await;
        }
    }
}

struct VoiceReceiveHandler {
    adapter: Weak<LiveVoiceAdapter>,
    session_id: String,
}

#[async_trait]
impl VoiceEventHandler for VoiceReceiveHandler {
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

fn pcm_i16_to_le_bytes(samples: &[i16]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(samples.len() * 2);
    for sample in samples {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    bytes
}
