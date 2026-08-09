//! The single per-kind policy table.
//!
//! Everything the engine needs to know about a `JobKind` — which executor
//! runs it, which concurrency lane it occupies, how a waiting parent resolves
//! when its children finish, whether rows are ephemeral and when they are
//! garbage-collected, and how the dashboard categorizes it — lives in the one
//! exhaustive match in [`spec`]. Adding a kind without declaring its policy
//! is a compile error, and the scheduler iterates kinds that are actually due
//! in Postgres, so a queued row can never sit unschedulable in silence.

use crate::model::job::{Job, JobKind, JobPayload};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JobExecutor {
    /// Async tokio task handed a fresh `Ctx`.
    Async,
    /// `spawn_blocking` worker for kinds that do heavy synchronous work
    /// (wake detection, WAV handling, agent subprocesses).
    Blocking,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JobLane {
    GeneralAsync,
    VoiceControl,
    DiscordText,
    Wake,
    AudioSegment,
    TranscriptionMux,
    Agent,
    Maintenance,
}

impl JobLane {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::GeneralAsync => "general_async",
            Self::VoiceControl => "voice_control",
            Self::DiscordText => "discord_text",
            Self::Wake => "wake",
            Self::AudioSegment => "audio_segment",
            Self::TranscriptionMux => "transcription_mux",
            Self::Agent => "agent",
            Self::Maintenance => "maintenance",
        }
    }
}

/// What `resolve_waiting_jobs` does with a waiting parent once every child is
/// terminal: requeue it so its handler runs again and consumes child outputs,
/// or settle it directly from the children's states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResumePolicy {
    Resume,
    Settle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DashboardCategory {
    Conversation,
    VoiceDetail,
    Agent,
    MessagingControl,
    Background,
}

impl DashboardCategory {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Conversation => "conversation",
            Self::VoiceDetail => "voice_detail",
            Self::Agent => "agent",
            Self::MessagingControl => "messaging_control",
            Self::Background => "background",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct JobSpec {
    pub executor: JobExecutor,
    pub lane: JobLane,
    pub resume: ResumePolicy,
    pub ephemeral: bool,
    pub gc_ok_seconds: i64,
    pub gc_failed_seconds: i64,
    pub dashboard: DashboardCategory,
}

const fn durable(
    executor: JobExecutor,
    lane: JobLane,
    resume: ResumePolicy,
    dashboard: DashboardCategory,
) -> JobSpec {
    JobSpec {
        executor,
        lane,
        resume,
        ephemeral: false,
        gc_ok_seconds: 0,
        gc_failed_seconds: 0,
        dashboard,
    }
}

const fn ephemeral(
    executor: JobExecutor,
    lane: JobLane,
    resume: ResumePolicy,
    gc_ok_seconds: i64,
    gc_failed_seconds: i64,
    dashboard: DashboardCategory,
) -> JobSpec {
    JobSpec {
        executor,
        lane,
        resume,
        ephemeral: true,
        gc_ok_seconds,
        gc_failed_seconds,
        dashboard,
    }
}

pub(crate) const fn spec(kind: JobKind) -> JobSpec {
    use DashboardCategory as Cat;
    use JobExecutor as Exec;
    use JobLane as Lane;
    use ResumePolicy as Resume;
    match kind {
        JobKind::RuntimeControl => durable(
            Exec::Async,
            Lane::GeneralAsync,
            Resume::Settle,
            Cat::MessagingControl,
        ),
        JobKind::RuntimeMaintenance => ephemeral(
            Exec::Async,
            Lane::Maintenance,
            Resume::Settle,
            300,
            300,
            Cat::Background,
        ),
        JobKind::VoiceStatusSync => ephemeral(
            Exec::Async,
            Lane::Maintenance,
            Resume::Resume,
            300,
            300,
            Cat::Background,
        ),
        JobKind::DiscordVoiceStatusSnapshot => ephemeral(
            Exec::Async,
            Lane::Maintenance,
            Resume::Settle,
            300,
            300,
            Cat::Background,
        ),
        JobKind::AutomationEvaluation => ephemeral(
            Exec::Async,
            Lane::Maintenance,
            Resume::Settle,
            300,
            300,
            Cat::Background,
        ),
        JobKind::AgentSessionRetirement => ephemeral(
            Exec::Async,
            Lane::Maintenance,
            Resume::Settle,
            300,
            300,
            Cat::Background,
        ),
        JobKind::StaleWakeProbeSweep => ephemeral(
            Exec::Async,
            Lane::Maintenance,
            Resume::Settle,
            300,
            300,
            Cat::Background,
        ),
        JobKind::EphemeralJobGc => ephemeral(
            Exec::Async,
            Lane::Maintenance,
            Resume::Settle,
            300,
            300,
            Cat::Background,
        ),
        JobKind::DiscordVoiceJoin
        | JobKind::DiscordVoiceLeave
        | JobKind::DiscordVoiceMute
        | JobKind::DiscordVoiceDeafen
        | JobKind::DiscordVoicePlayAudio => durable(
            Exec::Async,
            Lane::VoiceControl,
            Resume::Settle,
            Cat::MessagingControl,
        ),
        JobKind::DiscordVoicePlayback => durable(
            Exec::Async,
            Lane::VoiceControl,
            Resume::Resume,
            Cat::MessagingControl,
        ),
        JobKind::WakeProbe => ephemeral(
            Exec::Blocking,
            Lane::Wake,
            Resume::Settle,
            60,
            300,
            Cat::Background,
        ),
        JobKind::AudioSegment => ephemeral(
            Exec::Blocking,
            Lane::AudioSegment,
            Resume::Settle,
            300,
            1800,
            Cat::VoiceDetail,
        ),
        JobKind::TranscriptionMuxPlan => ephemeral(
            Exec::Async,
            Lane::GeneralAsync,
            Resume::Settle,
            300,
            1800,
            Cat::Background,
        ),
        JobKind::TranscriptionMux => ephemeral(
            Exec::Blocking,
            Lane::TranscriptionMux,
            Resume::Settle,
            300,
            1800,
            Cat::Background,
        ),
        JobKind::WakeActivation => durable(
            Exec::Async,
            Lane::GeneralAsync,
            Resume::Settle,
            Cat::Conversation,
        ),
        JobKind::RoomAgentPlacement => durable(
            Exec::Async,
            Lane::GeneralAsync,
            Resume::Resume,
            Cat::MessagingControl,
        ),
        JobKind::Command => durable(
            Exec::Async,
            Lane::GeneralAsync,
            Resume::Settle,
            Cat::MessagingControl,
        ),
        JobKind::DiscordTextMessage | JobKind::DiscordSlashCommand => durable(
            Exec::Async,
            Lane::GeneralAsync,
            Resume::Settle,
            Cat::MessagingControl,
        ),
        JobKind::TextDelivery => durable(
            Exec::Async,
            Lane::GeneralAsync,
            Resume::Resume,
            Cat::MessagingControl,
        ),
        JobKind::ConfirmationRequired => durable(
            Exec::Async,
            Lane::GeneralAsync,
            Resume::Resume,
            Cat::MessagingControl,
        ),
        JobKind::AgentSessionStart | JobKind::AgentSessionResume => {
            durable(Exec::Async, Lane::GeneralAsync, Resume::Resume, Cat::Agent)
        }
        JobKind::AgentSessionSunset => {
            durable(Exec::Async, Lane::GeneralAsync, Resume::Settle, Cat::Agent)
        }
        JobKind::TranscriptPublication => durable(
            Exec::Async,
            Lane::GeneralAsync,
            Resume::Resume,
            Cat::Conversation,
        ),
        JobKind::DiscordTextSend
        | JobKind::DiscordForumThreadCreate
        | JobKind::DiscordForumThreadRename => durable(
            Exec::Async,
            Lane::DiscordText,
            Resume::Settle,
            Cat::MessagingControl,
        ),
        JobKind::DiscordTypingIndicator => durable(
            Exec::Async,
            Lane::DiscordText,
            Resume::Resume,
            Cat::Background,
        ),
        JobKind::AgentTask => durable(Exec::Blocking, Lane::Agent, Resume::Resume, Cat::Agent),
        JobKind::MemberSync => ephemeral(
            Exec::Async,
            Lane::GeneralAsync,
            Resume::Settle,
            300,
            300,
            Cat::Background,
        ),
        JobKind::AgentThreadTitleRefresh => ephemeral(
            Exec::Blocking,
            Lane::Agent,
            Resume::Resume,
            300,
            300,
            Cat::Background,
        ),
    }
}

/// Jobs sharing a non-empty ordering key are mutually exclusive: the claim
/// SQL skips a due job while another job holding the same key is active.
pub(crate) fn ordering_key(job: &Job) -> String {
    match &job.payload {
        JobPayload::WakeProbe(payload) => {
            format!("wake:stream:{}", payload.stream_id)
        }
        JobPayload::MemberSync(payload) => {
            format!("discord:members:{}", normalize_key_part(&payload.guild_id))
        }
        JobPayload::TranscriptionMuxPlan(payload) => {
            format!(
                "transcription:mux_plan:{}",
                normalize_key_part(&payload.transcription_source_id)
            )
        }
        JobPayload::AgentTask(payload) => {
            format!(
                "agent:session:{}",
                normalize_key_part(&payload.agent_session_id)
            )
        }
        JobPayload::WakeActivation(payload) => {
            voice_agent_route_ordering_key(&payload.guild_id, &payload.voice_channel_id)
        }
        JobPayload::Command(payload)
            if payload.command.command_kind == crate::model::job::CommandKind::AgentTask =>
        {
            voice_agent_route_ordering_key(&payload.command.guild_id, &payload.command.scope_id)
        }
        JobPayload::DiscordTextMessage(payload) => {
            if payload.guild_id.trim().is_empty() {
                format!(
                    "agent:route:{}",
                    crate::runtime::agents::dm_route_key(&payload.author_user_id)
                )
            } else {
                format!("discord:text:{}", normalize_key_part(&payload.channel_id))
            }
        }
        JobPayload::DiscordSlashCommand(payload) => {
            if payload.guild_id.trim().is_empty() {
                format!("discord:slash:dm:{}", normalize_key_part(&payload.user_id))
            } else {
                format!(
                    "discord:slash:{}:{}",
                    normalize_key_part(&payload.guild_id),
                    normalize_key_part(&payload.channel_id)
                )
            }
        }
        JobPayload::TextDelivery(payload) => {
            if payload.target.kind == crate::model::job::TextTargetKind::AgentSession {
                return format!(
                    "text:session_route:{}:{}",
                    normalize_key_part(&job.guild_id),
                    normalize_key_part(&job.scope_id)
                );
            }
            let target_id = if payload.target.kind == crate::model::job::TextTargetKind::Dm {
                payload.target.user_id.as_str()
            } else {
                payload.target.channel_id.as_str()
            };
            if payload.source_job_id.trim().is_empty() {
                format!(
                    "text:target:{}:{}",
                    payload.target.kind.as_str(),
                    normalize_key_part(target_id),
                )
            } else {
                format!("text:source:{}", normalize_key_part(&payload.source_job_id))
            }
        }
        JobPayload::DiscordTextSend(payload) => {
            let target_id = if payload.target.kind == crate::model::job::TextTargetKind::Dm {
                payload.target.user_id.as_str()
            } else {
                payload.target.channel_id.as_str()
            };
            format!(
                "discord:text:{}:{}",
                payload.target.kind.as_str(),
                normalize_key_part(target_id)
            )
        }
        JobPayload::DiscordForumThreadCreate(payload) => {
            format!(
                "discord:forum_thread:{}",
                normalize_key_part(&payload.parent_channel_id)
            )
        }
        JobPayload::DiscordForumThreadRename(payload) => {
            format!("discord:thread:{}", normalize_key_part(&payload.thread_id))
        }
        JobPayload::DiscordTypingIndicator(payload) => {
            if payload.target.kind == crate::model::job::TextTargetKind::AgentSession {
                return format!(
                    "discord:typing:source:{}",
                    normalize_key_part(&payload.source_job_id)
                );
            }
            let target_id = if payload.target.kind == crate::model::job::TextTargetKind::Dm {
                payload.target.user_id.as_str()
            } else {
                payload.target.channel_id.as_str()
            };
            format!(
                "discord:typing:{}:{}",
                payload.target.kind.as_str(),
                normalize_key_part(target_id)
            )
        }
        JobPayload::ConfirmationRequired(payload) => {
            if payload.confirmation.delivery == "dm" {
                format!(
                    "discord:confirmation:dm:{}",
                    normalize_key_part(&payload.command.requested_by_user_id)
                )
            } else {
                format!(
                    "discord:confirmation:channel:{}",
                    normalize_key_part(&job.scope_id)
                )
            }
        }
        JobPayload::AgentSessionStart(payload) => {
            voice_agent_route_ordering_key(&payload.guild_id, &payload.voice_channel_id)
        }
        JobPayload::AgentSessionSunset(payload) => {
            format!(
                "agent:session:{}",
                normalize_key_part(&payload.agent_session_id)
            )
        }
        JobPayload::AgentSessionResume(payload) => {
            if payload.route_kind == "dm" {
                format!(
                    "agent:route:{}",
                    crate::runtime::agents::dm_route_key(&payload.dm_user_id)
                )
            } else {
                voice_agent_route_ordering_key(&payload.guild_id, &payload.voice_channel_id)
            }
        }
        JobPayload::AgentThreadTitleRefresh(payload) => {
            format!(
                "agent:session:{}",
                normalize_key_part(&payload.agent_session_id)
            )
        }
        JobPayload::TranscriptPublication(payload) => {
            format!(
                "publication:{}",
                normalize_key_part(&payload.publication_id)
            )
        }
        JobPayload::RoomAgentPlacement(payload) => {
            let room_key = if payload.room_id.trim().is_empty() {
                job.scope_id.as_str()
            } else {
                payload.room_id.as_str()
            };
            format!(
                "room:placement:{}:{}",
                normalize_key_part(&job.guild_id),
                normalize_key_part(room_key)
            )
        }
        JobPayload::DiscordVoiceJoin(payload) => {
            format!("voice:bot:{}", payload.bot_id)
        }
        JobPayload::DiscordVoiceLeave(payload) => {
            format!("voice:session:{}", payload.session_id)
        }
        JobPayload::DiscordVoicePlayback(payload) => {
            format!("voice:session:{}", payload.session_id)
        }
        JobPayload::DiscordVoiceMute(payload) => {
            format!("voice:session:{}", payload.session_id)
        }
        JobPayload::DiscordVoiceDeafen(payload) => {
            format!("voice:session:{}", payload.session_id)
        }
        JobPayload::DiscordVoicePlayAudio(payload) => {
            format!("voice:session:{}", payload.session_id)
        }
        JobPayload::RuntimeMaintenance(_)
        | JobPayload::VoiceStatusSync(_)
        | JobPayload::DiscordVoiceStatusSnapshot(_)
        | JobPayload::AutomationEvaluation(_)
        | JobPayload::AgentSessionRetirement(_)
        | JobPayload::StaleWakeProbeSweep(_)
        | JobPayload::EphemeralJobGc(_) => "runtime:maintenance".to_string(),
        _ => String::new(),
    }
}

fn voice_agent_route_ordering_key(guild_id: &str, voice_channel_id: &str) -> String {
    format!(
        "agent:route:{}",
        crate::runtime::agents::voice_route_key(guild_id, voice_channel_id)
    )
}

pub(crate) fn normalize_key_part(value: &str) -> String {
    let normalized = value
        .trim()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    if normalized.is_empty() {
        "unknown".to_string()
    } else {
        normalized
    }
}
