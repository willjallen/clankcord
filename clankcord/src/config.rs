use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::Context;
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

use crate::Result;
use crate::errors::discord_tool_error;
use crate::runtime::rooms::RoomConfig;

pub const CONFIG_PATH: &str = "config.toml";

static APP_CONFIG: OnceLock<AppConfig> = OnceLock::new();

#[derive(Debug, Clone, Deserialize)]
pub struct AppConfig {
    pub paths: PathsConfig,
    pub secrets: SecretsConfig,
    pub time: TimeConfig,
    pub api: ApiConfig,
    pub postgres: PostgresConfig,
    pub discord: DiscordConfig,
    pub codex: CodexConfig,
    pub agents: AgentsConfig,
    pub prompts: PromptsConfig,
    pub pool: PoolConfig,
    pub transcription: TranscriptionConfig,
    pub wake: WakeConfig,
    pub voice: VoiceConfig,
    pub jobs: JobsConfig,
    pub control: ControlConfig,
    pub guilds: Vec<GuildConfig>,
    pub rooms: Vec<ConfiguredRoom>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PathsConfig {
    pub state_dir: PathBuf,
    pub timeline_root: PathBuf,
    pub agent_workspaces_root: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SecretsConfig {
    pub root: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TimeConfig {
    pub timezone: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ApiConfig {
    pub host: String,
    pub port: u16,
    pub base_url: String,
    pub timeout_seconds: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PostgresConfig {
    pub host: String,
    pub port: u16,
    pub database: String,
    pub user: String,
    pub password_secret: String,
    pub schema: String,
    pub pool_size: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DiscordConfig {
    pub api_base: String,
    pub bot_token_secret: String,
    pub voice_bot_tokens_secret: String,
    pub member_cache_max_age_ms: i64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodexConfig {
    pub bin: String,
    pub home: PathBuf,
    pub workdir: PathBuf,
    pub model: String,
    pub reasoning_effort: CodexReasoningEffort,
    pub fast_mode: bool,
    pub sandbox: String,
    pub bypass_sandbox: bool,
    pub approval_policy: String,
    pub linear_mcp: CodexLinearMcpConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodexLinearMcpConfig {
    pub enabled: bool,
    pub url: String,
    pub api_key_secret: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CodexReasoningEffort {
    Low,
    Medium,
    High,
    #[serde(rename = "xhigh")]
    XHigh,
}

impl CodexReasoningEffort {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct AgentsConfig {
    pub thread_auto_archive_minutes: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PromptsConfig {
    pub dir: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PoolConfig {
    pub idle_channel_name: String,
    pub auto_join_enabled: bool,
    pub auto_join_min_participants: usize,
    pub auto_leave_empty_seconds: i64,
    pub auto_leave_single_deafened_seconds: i64,
    pub auto_rejoin_cooldown_seconds: i64,
    pub manual_override_seconds: i64,
    pub pause_release_seconds: i64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscriptionConfig {
    pub silence_ms: i64,
    pub max_segment_ms: i64,
    pub minimum_utterance_ms: i64,
    pub speech_rms_start_threshold: f64,
    pub speech_rms_continue_threshold: f64,
    pub speech_start_ms: i64,
    pub speech_soft_break_ms: i64,
    pub speech_end_silence_ms: i64,
    pub speech_preroll_ms: i64,
    pub active_source: String,
    pub mux_provider_streams: usize,
    pub mux_batch_delay_ms: i64,
    pub mux_max_slots: usize,
    pub mux_max_audio_ms: i64,
    pub mux_guard_ms: i64,
    pub mux_normal_latency_budget_ms: i64,
    pub mux_wake_latency_budget_ms: i64,
    pub mux_overflow_backlog_ms: i64,
    pub sources: BTreeMap<String, TranscriptionSourceConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscriptionSourceConfig {
    pub provider: TranscriptionProvider,
    pub base_url: String,
    pub model: String,
    pub language: String,
    pub response_format: String,
    pub timestamp_granularity: String,
    pub include_logprobs: bool,
    pub max_token_logprobs: usize,
    pub timeout_seconds: u64,
    pub retry_backoff_initial_seconds: i64,
    pub retry_backoff_max_seconds: i64,
    pub drop_no_speech_probability: f64,
    pub drop_avg_token_logprob: f64,
    pub api_key_secret: String,
    pub diarize: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptionProvider {
    OpenaiCompatible,
    Elevenlabs,
}

impl TranscriptionProvider {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenaiCompatible => "openai_compatible",
            Self::Elevenlabs => "elevenlabs",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct WakeConfig {
    pub base_url: String,
    pub timeout_seconds: u64,
    pub api_key_secret: String,
    pub probe_minimum_ms: i64,
    pub probe_window_ms: i64,
    pub probe_interval_ms: i64,
    pub probe_max_queue_age_seconds: i64,
    pub duplicate_overlap_grace_ms: i64,
    pub activation: WakeActivationConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WakeActivationConfig {
    pub lookback_seconds: i64,
    pub min_post_seconds: i64,
    pub speaker_idle_seconds: i64,
    pub stt_flush_grace_seconds: i64,
    pub max_window_seconds: i64,
    pub additive_preempt_seconds: i64,
    pub independent_after_seconds: i64,
    pub active_capture_poll_ms: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct VoiceConfig {
    pub sound: VoiceSoundConfig,
    pub capture: VoiceCaptureConfig,
    pub diagnostics: VoiceDiagnosticsConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct VoiceSoundConfig {
    pub dir: PathBuf,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct VoiceCaptureConfig {
    pub flush_interval_seconds: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct VoiceDiagnosticsConfig {
    pub enabled: bool,
    pub audio_stats: bool,
    pub receiver: bool,
    pub event_paths: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct JobsConfig {
    pub runtime_maintenance_interval_seconds: f64,
    pub intake_queue_depth: usize,
    pub ephemeral_gc_batch_limit: usize,
    pub failed_audio_segment_retry_batch_limit: usize,
    pub dispatch_drain_max_passes: usize,
    pub concurrency: JobLaneConfig,
    pub batch: JobLaneConfig,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobLaneConfig {
    pub wake: usize,
    pub audio_segment: usize,
    pub voice_control: usize,
    pub discord_text: usize,
    pub agent: usize,
    pub maintenance: usize,
    pub general_async: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct GuildConfig {
    #[serde(default, alias = "guildId")]
    pub guild_id: String,
    #[serde(default, alias = "guildSlug")]
    pub guild_slug: String,
    #[serde(default, alias = "idleChannelId")]
    pub idle_channel_id: String,
    #[serde(default, alias = "idleChannelName")]
    pub idle_channel_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct ControlConfig {
    #[serde(default, alias = "guildId")]
    pub guild_id: String,
    #[serde(default, alias = "guildSlug")]
    pub guild_slug: String,
    #[serde(default, alias = "defaultVoiceRoomId")]
    pub default_voice_room_id: String,
    #[serde(default, alias = "botsChannelId")]
    pub bots_channel_id: String,
    #[serde(default, alias = "agentThreadsChannelId")]
    pub agent_threads_channel_id: String,
    #[serde(default, alias = "transcriptsForumId")]
    pub transcripts_forum_id: String,
    #[serde(default, alias = "threadAutoArchiveMinutes")]
    pub thread_auto_archive_minutes: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ConfiguredRoom {
    #[serde(alias = "roomId")]
    pub id: String,
    #[serde(alias = "guildId")]
    pub guild_id: String,
    #[serde(alias = "guildSlug")]
    pub guild_slug: String,
    #[serde(alias = "channelId")]
    pub channel_id: String,
    #[serde(alias = "channelSlug")]
    pub channel_slug: String,
    #[serde(alias = "channelName")]
    pub channel_name: String,
    #[serde(alias = "autoJoin")]
    pub auto_join: bool,
}

pub fn app_config() -> &'static AppConfig {
    APP_CONFIG.get_or_init(|| {
        let path = config_file_path()
            .unwrap_or_else(|error| panic!("failed to locate {CONFIG_PATH}: {error:#}"));
        load_app_config(&path)
            .unwrap_or_else(|error| panic!("failed to load {}: {error:#}", path.display()))
    })
}

fn config_file_path() -> Result<PathBuf> {
    let mut dir = env::current_dir().context("resolving current directory")?;
    loop {
        let path = dir.join(CONFIG_PATH);
        if path.is_file() {
            return Ok(path);
        }
        if !dir.pop() {
            anyhow::bail!("{CONFIG_PATH} was not found in the current directory or its parents");
        }
    }
}

fn load_app_config(path: &Path) -> Result<AppConfig> {
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let mut config = toml::from_str::<AppConfig>(&text)
        .with_context(|| format!("parsing {}", path.display()))?;
    apply_env_overrides(&mut config)?;
    validate_config(&config)?;
    Ok(config)
}

fn apply_env_overrides(config: &mut AppConfig) -> Result<()> {
    if let Some(value) = env_bool("CLANKCORD_CODEX_BYPASS_SANDBOX")? {
        config.codex.bypass_sandbox = value;
    }
    Ok(())
}

fn env_bool(key: &str) -> Result<Option<bool>> {
    let Ok(value) = env::var(key) else {
        return Ok(None);
    };
    let normalized = value.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return Ok(None);
    }
    let parsed = match normalized.as_str() {
        "true" | "1" | "yes" | "on" => true,
        "false" | "0" | "no" | "off" => false,
        _ => {
            anyhow::bail!("invalid {key} `{value}`: expected true or false");
        }
    };
    Ok(Some(parsed))
}

fn validate_config(config: &AppConfig) -> Result<()> {
    if config.rooms.is_empty() {
        anyhow::bail!("config.toml must define at least one [[rooms]] entry");
    }
    if config.guilds.is_empty() {
        anyhow::bail!("config.toml must define at least one [[guilds]] entry");
    }
    if config.discord.bot_token_secret.trim().is_empty() {
        anyhow::bail!("config.toml discord.bot_token_secret is required");
    }
    if config.discord.voice_bot_tokens_secret.trim().is_empty() {
        anyhow::bail!("config.toml discord.voice_bot_tokens_secret is required");
    }
    if config.postgres.password_secret.trim().is_empty() {
        anyhow::bail!("config.toml postgres.password_secret is required");
    }
    validate_codex_config(&config.codex)?;
    validate_transcription_config(&config.transcription)?;
    if config.prompts.dir.as_os_str().is_empty() {
        anyhow::bail!("config.toml prompts.dir is required");
    }
    config
        .time
        .timezone
        .parse::<Tz>()
        .with_context(|| format!("invalid time.timezone `{}`", config.time.timezone))?;
    Ok(())
}

fn validate_codex_config(config: &CodexConfig) -> Result<()> {
    if config.linear_mcp.enabled {
        if config.linear_mcp.url.trim().is_empty() {
            anyhow::bail!("config.toml codex.linear_mcp.url is required when enabled");
        }
        if config.linear_mcp.api_key_secret.trim().is_empty() {
            anyhow::bail!("config.toml codex.linear_mcp.api_key_secret is required when enabled");
        }
    }
    Ok(())
}

fn validate_transcription_config(config: &TranscriptionConfig) -> Result<()> {
    let active_source = config.active_source.trim();
    if active_source.is_empty() {
        anyhow::bail!("config.toml transcription.active_source is required");
    }
    if !config.sources.contains_key(active_source) {
        anyhow::bail!(
            "config.toml transcription.active_source `{active_source}` is not defined under transcription.sources"
        );
    }
    if config.mux_provider_streams == 0 || config.mux_provider_streams > 2 {
        anyhow::bail!("config.toml transcription.mux_provider_streams must be 1 or 2");
    }
    if config.mux_batch_delay_ms < 0 {
        anyhow::bail!("config.toml transcription.mux_batch_delay_ms must be zero or positive");
    }
    if config.mux_max_slots == 0 {
        anyhow::bail!("config.toml transcription.mux_max_slots must be positive");
    }
    if config.mux_max_audio_ms <= 0 {
        anyhow::bail!("config.toml transcription.mux_max_audio_ms must be positive");
    }
    if config.mux_guard_ms < 0 {
        anyhow::bail!("config.toml transcription.mux_guard_ms must be zero or positive");
    }
    if config.mux_normal_latency_budget_ms <= 0 {
        anyhow::bail!("config.toml transcription.mux_normal_latency_budget_ms must be positive");
    }
    if config.mux_wake_latency_budget_ms <= 0 {
        anyhow::bail!("config.toml transcription.mux_wake_latency_budget_ms must be positive");
    }
    if config.mux_overflow_backlog_ms < 0 {
        anyhow::bail!("config.toml transcription.mux_overflow_backlog_ms must be zero or positive");
    }
    if config.speech_rms_start_threshold <= 0.0 || config.speech_rms_continue_threshold <= 0.0 {
        anyhow::bail!("config.toml transcription speech RMS thresholds must be positive");
    }
    if config.speech_rms_start_threshold < config.speech_rms_continue_threshold {
        anyhow::bail!(
            "config.toml transcription.speech_rms_start_threshold must be at least speech_rms_continue_threshold"
        );
    }
    if config.speech_start_ms <= 0 {
        anyhow::bail!("config.toml transcription.speech_start_ms must be positive");
    }
    if config.speech_soft_break_ms <= 0 {
        anyhow::bail!("config.toml transcription.speech_soft_break_ms must be positive");
    }
    if config.speech_end_silence_ms <= 0 {
        anyhow::bail!("config.toml transcription.speech_end_silence_ms must be positive");
    }
    if config.speech_soft_break_ms > config.speech_end_silence_ms {
        anyhow::bail!(
            "config.toml transcription.speech_soft_break_ms must be at most speech_end_silence_ms"
        );
    }
    if config.speech_preroll_ms < 0 {
        anyhow::bail!("config.toml transcription.speech_preroll_ms must be zero or positive");
    }
    for (source_id, source) in &config.sources {
        if source_id.trim().is_empty() {
            anyhow::bail!("config.toml transcription source ids cannot be empty");
        }
        if source.base_url.trim().is_empty() {
            anyhow::bail!("config.toml transcription source `{source_id}` base_url is required");
        }
        if source.model.trim().is_empty() {
            anyhow::bail!("config.toml transcription source `{source_id}` model is required");
        }
        if source.retry_backoff_initial_seconds <= 0 {
            anyhow::bail!(
                "config.toml transcription source `{source_id}` retry_backoff_initial_seconds must be positive"
            );
        }
        if source.retry_backoff_max_seconds < source.retry_backoff_initial_seconds {
            anyhow::bail!(
                "config.toml transcription source `{source_id}` retry_backoff_max_seconds must be at least retry_backoff_initial_seconds"
            );
        }
        match source.timestamp_granularity.trim() {
            "word" | "segment" => {}
            value => anyhow::bail!(
                "config.toml transcription source `{source_id}` timestamp_granularity must be word or segment, got `{value}`"
            ),
        }
    }
    Ok(())
}

pub fn state_dir() -> PathBuf {
    app_config().paths.state_dir.clone()
}

pub fn timeline_root() -> PathBuf {
    app_config().paths.timeline_root.clone()
}

pub fn agent_workspaces_root() -> PathBuf {
    app_config().paths.agent_workspaces_root.clone()
}

pub fn api_base_url() -> String {
    app_config().api.base_url.trim_end_matches('/').to_string()
}

pub fn api_timeout_seconds() -> u64 {
    app_config().api.timeout_seconds.max(5)
}

pub fn env_context_value(key: &str) -> Option<String> {
    env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub fn http_addr() -> Result<SocketAddr> {
    Ok(format!("{}:{}", app_config().api.host, app_config().api.port).parse()?)
}

pub fn local_tz() -> Tz {
    app_config()
        .time
        .timezone
        .parse::<Tz>()
        .expect("config timezone was validated")
}

pub fn database_url() -> Result<String> {
    let config = &app_config().postgres;
    let password = required_secret(&config.password_secret, "postgres password")?;
    Ok(format!(
        "postgres://{}:{}@{}:{}/{}",
        url_encode(&config.user),
        url_encode(&password),
        config.host,
        config.port,
        url_encode(&config.database)
    ))
}

pub fn database_schema() -> String {
    app_config().postgres.schema.clone()
}

pub fn database_pool_size() -> u32 {
    app_config().postgres.pool_size.clamp(4, 128)
}

pub fn discord_api_base() -> String {
    app_config()
        .discord
        .api_base
        .trim_end_matches('/')
        .to_string()
}

pub fn load_discord_bot_token() -> Result<String> {
    required_secret(
        &app_config().discord.bot_token_secret,
        "Discord control bot token",
    )
}

pub fn raw_voice_bot_token_lines() -> Result<Vec<String>> {
    Ok(secret_value(&app_config().discord.voice_bot_tokens_secret)?
        .lines()
        .map(str::to_string)
        .collect())
}

pub fn wake_url() -> Result<String> {
    let base_url = app_config().wake.base_url.trim().trim_end_matches('/');
    if base_url.is_empty() {
        return Err(discord_tool_error("config.toml wake.base_url is not set"));
    }
    if base_url.ends_with("/audio/wake") {
        Ok(base_url.to_string())
    } else {
        Ok(format!("{base_url}/audio/wake"))
    }
}

pub fn wake_timeout_seconds() -> u64 {
    app_config().wake.timeout_seconds.max(1)
}

pub fn wake_api_key() -> Result<String> {
    optional_secret(&app_config().wake.api_key_secret)
}

pub fn wake_probe_max_queue_age_seconds() -> i64 {
    app_config().wake.probe_max_queue_age_seconds.clamp(1, 60)
}

pub fn wake_duplicate_overlap_grace_ms() -> i64 {
    app_config().wake.duplicate_overlap_grace_ms.max(0)
}

pub fn wake_activation_config() -> WakeActivationConfig {
    app_config().wake.activation.clone()
}

pub fn codex_bin() -> String {
    app_config().codex.bin.clone()
}

pub fn codex_home() -> PathBuf {
    app_config().codex.home.clone()
}

pub fn codex_workdir() -> PathBuf {
    app_config().codex.workdir.clone()
}

pub fn codex_model() -> Option<String> {
    non_empty_option(&app_config().codex.model)
}

pub fn codex_reasoning_effort() -> CodexReasoningEffort {
    app_config().codex.reasoning_effort
}

pub fn codex_fast_mode() -> bool {
    app_config().codex.fast_mode
}

pub fn codex_sandbox() -> Option<String> {
    non_empty_option(&app_config().codex.sandbox)
}

pub fn codex_bypass_sandbox() -> bool {
    app_config().codex.bypass_sandbox
}

pub fn codex_approval_policy() -> String {
    app_config().codex.approval_policy.clone()
}

pub const CODEX_LINEAR_MCP_TOKEN_ENV: &str = "LINEAR_API_KEY";

pub fn codex_linear_mcp_config() -> CodexLinearMcpConfig {
    app_config().codex.linear_mcp.clone()
}

pub fn codex_linear_mcp_enabled() -> bool {
    app_config().codex.linear_mcp.enabled
}

pub fn codex_linear_mcp_api_key() -> Result<String> {
    let config = &app_config().codex.linear_mcp;
    if !config.enabled {
        return Ok(String::new());
    }
    required_secret(&config.api_key_secret, "Linear MCP API key")
}

pub fn agent_session_max_active_seconds() -> i64 {
    8 * 60 * 60
}

pub fn agent_thread_auto_archive_minutes() -> i64 {
    app_config()
        .agents
        .thread_auto_archive_minutes
        .clamp(60, 10080)
}

pub fn prompt_templates_dir() -> PathBuf {
    app_config().prompts.dir.clone()
}

pub fn runtime_pool_config() -> PoolConfig {
    app_config().pool.clone()
}

pub fn transcription_config() -> TranscriptionConfig {
    app_config().transcription.clone()
}

pub fn active_transcription_source_id() -> String {
    app_config().transcription.active_source.trim().to_string()
}

pub fn active_transcription_source() -> Result<NamedTranscriptionSourceConfig> {
    transcription_source(&active_transcription_source_id())
}

pub fn transcription_source(source_id: &str) -> Result<NamedTranscriptionSourceConfig> {
    let source_id = source_id.trim();
    let Some(source) = app_config().transcription.sources.get(source_id) else {
        return Err(discord_tool_error(format!(
            "config.toml transcription source `{source_id}` is not defined"
        )));
    };
    Ok(NamedTranscriptionSourceConfig {
        id: source_id.to_string(),
        config: source.clone(),
    })
}

#[derive(Debug, Clone)]
pub struct NamedTranscriptionSourceConfig {
    pub id: String,
    pub config: TranscriptionSourceConfig,
}

pub fn transcription_source_api_key(source: &TranscriptionSourceConfig) -> Result<String> {
    optional_secret(&source.api_key_secret)
}

pub fn transcription_mux_provider_streams() -> usize {
    app_config().transcription.mux_provider_streams.clamp(1, 2)
}

pub fn transcription_mux_batch_delay_ms() -> i64 {
    app_config()
        .transcription
        .mux_batch_delay_ms
        .clamp(0, 10_000)
}

pub fn transcription_mux_max_slots() -> usize {
    app_config().transcription.mux_max_slots.clamp(1, 128)
}

pub fn transcription_mux_max_audio_ms() -> i64 {
    app_config()
        .transcription
        .mux_max_audio_ms
        .clamp(1_000, 300_000)
}

pub fn transcription_mux_guard_ms() -> i64 {
    app_config().transcription.mux_guard_ms.clamp(0, 5_000)
}

pub fn transcription_mux_normal_latency_budget_ms() -> i64 {
    app_config()
        .transcription
        .mux_normal_latency_budget_ms
        .clamp(1_000, 300_000)
}

pub fn transcription_mux_wake_latency_budget_ms() -> i64 {
    app_config()
        .transcription
        .mux_wake_latency_budget_ms
        .clamp(500, 120_000)
}

pub fn transcription_mux_overflow_backlog_ms() -> i64 {
    app_config()
        .transcription
        .mux_overflow_backlog_ms
        .clamp(0, 120_000)
}

pub fn voice_sound_dir() -> PathBuf {
    app_config().voice.sound.dir.clone()
}

pub fn voice_sound_timeout_ms() -> u64 {
    app_config().voice.sound.timeout_ms
}

pub fn voice_flush_interval_seconds() -> f64 {
    app_config().voice.capture.flush_interval_seconds.max(0.001)
}

pub fn voice_diagnostics_config() -> VoiceDiagnosticsConfig {
    app_config().voice.diagnostics.clone()
}

pub fn discord_member_cache_max_age_ms() -> i64 {
    app_config().discord.member_cache_max_age_ms.max(0)
}

pub fn intake_queue_depth() -> usize {
    app_config().jobs.intake_queue_depth.max(1)
}

pub fn runtime_maintenance_interval_ms() -> i64 {
    let seconds = app_config()
        .jobs
        .runtime_maintenance_interval_seconds
        .max(0.001);
    (seconds * 1000.0).round() as i64
}

pub fn ephemeral_job_gc_batch_limit() -> usize {
    app_config().jobs.ephemeral_gc_batch_limit.clamp(1, 1000)
}

pub fn failed_audio_segment_retry_batch_limit() -> usize {
    app_config()
        .jobs
        .failed_audio_segment_retry_batch_limit
        .clamp(1, 1000)
}

pub fn dispatch_drain_max_passes() -> usize {
    app_config().jobs.dispatch_drain_max_passes.clamp(1, 512)
}

pub fn job_concurrency() -> JobLaneConfig {
    app_config().jobs.concurrency
}

pub fn job_batch_limits() -> JobLaneConfig {
    app_config().jobs.batch
}

pub fn guild_configs() -> Vec<GuildConfig> {
    app_config().guilds.clone()
}

pub fn control_config() -> ControlConfig {
    app_config().control.clone()
}

pub fn room_configs() -> Vec<RoomConfig> {
    app_config()
        .rooms
        .iter()
        .map(|room| RoomConfig {
            room_id: room.id.clone(),
            guild_id: room.guild_id.clone(),
            guild_slug: room.guild_slug.clone(),
            channel_id: room.channel_id.clone(),
            channel_slug: room.channel_slug.clone(),
            channel_name: room.channel_name.clone(),
            auto_join: room.auto_join,
        })
        .collect()
}

pub fn configured_guild_ids() -> Vec<String> {
    let mut guild_ids = BTreeMap::new();
    for guild in &app_config().guilds {
        insert_non_empty_key(&mut guild_ids, &guild.guild_id);
    }
    insert_non_empty_key(&mut guild_ids, &app_config().control.guild_id);
    for room in &app_config().rooms {
        insert_non_empty_key(&mut guild_ids, &room.guild_id);
    }
    guild_ids.into_keys().collect()
}

fn insert_non_empty_key(map: &mut BTreeMap<String, ()>, value: &str) {
    let value = value.trim();
    if !value.is_empty() {
        map.insert(value.to_string(), ());
    }
}

fn secret_value(secret_name: &str) -> Result<String> {
    let secret_name = secret_name.trim();
    if secret_name.is_empty() {
        return Ok(String::new());
    }
    let path = app_config().secrets.root.join(secret_name);
    let value = fs::read_to_string(&path)
        .with_context(|| format!("reading secret {}", path.display()))?
        .trim()
        .to_string();
    Ok(value)
}

fn required_secret(secret_name: &str, label: &str) -> Result<String> {
    let value = secret_value(secret_name)?;
    if value.is_empty() {
        anyhow::bail!("{label} secret `{secret_name}` is empty");
    }
    Ok(value)
}

fn optional_secret(secret_name: &str) -> Result<String> {
    secret_value(secret_name)
}

fn non_empty_option(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

fn url_encode(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}
