use std::sync::Arc;

use serde_json::{Map, Value, json};
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore};

use crate::Result;
#[cfg(test)]
use crate::runtime::AgentRuntime;
#[cfg(test)]
use crate::runtime::JobPayload;
use crate::runtime::core::execution::RuntimeAdapterJobs;
use crate::runtime::timeline::TimelineStore;
use crate::runtime::{Job, JobKind, Runtime, log};

const DEFAULT_DISPATCH_DRAIN_MAX_PASSES: usize = 64;

const JOB_EXECUTION_POLICIES: [JobExecutionPolicy; 14] = [
    JobExecutionPolicy::runtime_exclusive(
        JobKind::RuntimeControl,
        JobLane::GeneralAsync,
        JobOrdering::None,
    ),
    JobExecutionPolicy::adapter(
        JobKind::DiscordVoiceMute,
        JobLane::VoiceControl,
        JobOrdering::VoiceTarget,
    ),
    JobExecutionPolicy::adapter(
        JobKind::DiscordVoicePlayAudio,
        JobLane::VoiceControl,
        JobOrdering::VoiceTarget,
    ),
    JobExecutionPolicy::runtime_snapshot(
        JobKind::DiscordVoicePlayback,
        JobLane::VoiceControl,
        JobOrdering::VoiceTarget,
    ),
    JobExecutionPolicy::blocking_snapshot(
        JobKind::WakeProbe,
        JobLane::Wake,
        JobOrdering::WakeStream,
    ),
    JobExecutionPolicy::blocking_snapshot(JobKind::AudioSegment, JobLane::Audio, JobOrdering::None),
    JobExecutionPolicy::runtime_snapshot(
        JobKind::WakeActivation,
        JobLane::GeneralAsync,
        JobOrdering::None,
    ),
    JobExecutionPolicy::runtime_exclusive(
        JobKind::RoomAgentPlacement,
        JobLane::GeneralAsync,
        JobOrdering::VoiceTarget,
    ),
    JobExecutionPolicy::adapter(
        JobKind::DiscordVoiceJoin,
        JobLane::VoiceControl,
        JobOrdering::VoiceTarget,
    ),
    JobExecutionPolicy::adapter(
        JobKind::DiscordVoiceLeave,
        JobLane::VoiceControl,
        JobOrdering::VoiceTarget,
    ),
    JobExecutionPolicy::runtime_exclusive(
        JobKind::Command,
        JobLane::GeneralAsync,
        JobOrdering::None,
    ),
    JobExecutionPolicy::blocking_snapshot(JobKind::Response, JobLane::Response, JobOrdering::None),
    JobExecutionPolicy::blocking_snapshot(
        JobKind::RefineTranscript,
        JobLane::Refinement,
        JobOrdering::None,
    ),
    JobExecutionPolicy::blocking_snapshot(
        JobKind::AgentTask,
        JobLane::Agent,
        JobOrdering::AgentSession,
    ),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct JobExecutionPolicy {
    kind: JobKind,
    executor: JobExecutor,
    lane: JobLane,
    ordering: JobOrdering,
}

impl JobExecutionPolicy {
    const fn runtime_exclusive(kind: JobKind, lane: JobLane, ordering: JobOrdering) -> Self {
        Self {
            kind,
            executor: JobExecutor::RuntimeExclusive,
            lane,
            ordering,
        }
    }

    const fn runtime_snapshot(kind: JobKind, lane: JobLane, ordering: JobOrdering) -> Self {
        Self {
            kind,
            executor: JobExecutor::RuntimeSnapshot,
            lane,
            ordering,
        }
    }

    const fn adapter(kind: JobKind, lane: JobLane, ordering: JobOrdering) -> Self {
        Self {
            kind,
            executor: JobExecutor::AdapterAsync,
            lane,
            ordering,
        }
    }

    const fn blocking_snapshot(kind: JobKind, lane: JobLane, ordering: JobOrdering) -> Self {
        Self {
            kind,
            executor: JobExecutor::BlockingSnapshot,
            lane,
            ordering,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JobExecutor {
    RuntimeExclusive,
    RuntimeSnapshot,
    AdapterAsync,
    BlockingSnapshot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JobLane {
    GeneralAsync,
    VoiceControl,
    Wake,
    Audio,
    Response,
    Refinement,
    Agent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JobOrdering {
    None,
    VoiceTarget,
    WakeStream,
    AgentSession,
}

#[derive(Clone)]
pub(crate) struct RuntimeExecutor<E>
where
    E: RuntimeAdapterJobs + Clone + Send + Sync + 'static,
{
    adapter_jobs: E,
    timeline_store: TimelineStore,
    lanes: Arc<JobLanes>,
    notify: Arc<Notify>,
}

struct JobLanes {
    wake: Arc<Semaphore>,
    audio: Arc<Semaphore>,
    voice_control: Arc<Semaphore>,
    response: Arc<Semaphore>,
    refinement: Arc<Semaphore>,
    agent: Arc<Semaphore>,
    async_jobs: Arc<Semaphore>,
}

impl<E> RuntimeExecutor<E>
where
    E: RuntimeAdapterJobs + Clone + Send + Sync + 'static,
{
    pub(crate) fn new(adapter_jobs: E, timeline_store: TimelineStore) -> Self {
        Self {
            adapter_jobs,
            timeline_store,
            lanes: Arc::new(JobLanes::from_env()),
            notify: Arc::new(Notify::new()),
        }
    }

    pub(crate) fn notify_handle(&self) -> Arc<Notify> {
        self.notify.clone()
    }

    pub(crate) fn wake(&self) {
        self.notify.notify_one();
    }

    pub(crate) async fn schedule_due_jobs(&self) -> Result<Value> {
        let mut scheduled = Map::new();
        let due_kinds = self.timeline_store.due_job_kinds().await?;
        for policy in JOB_EXECUTION_POLICIES {
            if !due_kinds.contains(&policy.kind) {
                scheduled.insert(policy.kind.as_str().to_string(), idle_policy_report(policy));
                continue;
            }
            scheduled.insert(
                policy.kind.as_str().to_string(),
                self.schedule_policy(policy).await?,
            );
        }
        let total_scheduled = scheduled
            .values()
            .map(scheduled_count_for_kind)
            .sum::<usize>();
        scheduled.insert("totalScheduled".to_string(), json!(total_scheduled));
        Ok(Value::Object(scheduled))
    }

    pub(crate) async fn drain_ready_jobs(&self) -> Result<Value> {
        let max_passes = dispatch_drain_max_passes();
        let mut passes = Vec::new();
        let mut total_resolved = 0usize;
        let mut total_scheduled = 0usize;
        let mut exhausted = false;

        for pass in 0..max_passes {
            let resolved_waiting = self.timeline_store.resolve_waiting_jobs().await?;
            let scheduled = self.schedule_due_jobs().await?;
            let scheduled_count = scheduled_job_count(&scheduled);
            let resolved_count = resolved_waiting.len();
            total_resolved += resolved_count;
            total_scheduled += scheduled_count;
            passes.push(json!({
                "pass": pass + 1,
                "resolvedWaiting": resolved_waiting,
                "scheduled": scheduled,
            }));
            if resolved_count == 0 && scheduled_count == 0 {
                exhausted = true;
                break;
            }
            tokio::task::yield_now().await;
        }

        Ok(json!({
            "ok": true,
            "passes": passes,
            "totalResolvedWaiting": total_resolved,
            "totalScheduled": total_scheduled,
            "exhausted": exhausted,
        }))
    }

    pub(crate) async fn run_maintenance(&self) -> Result<Value> {
        let snapshot = Runtime::from_store(self.timeline_store.clone())?;
        snapshot.run_blocking_maintenance().await
    }

    async fn schedule_policy(&self, policy: JobExecutionPolicy) -> Result<Value> {
        let lane = self.lanes.semaphore(policy.lane);
        let permits = take_permits(&lane, dispatch_batch_limit(policy));
        let permit_count = permits.len();
        let mut blocked_keys = self.timeline_store.active_ordering_keys().await?;
        let jobs = self
            .timeline_store
            .claim_due_jobs(policy.kind, permit_count, &mut blocked_keys)
            .await?;
        let count = jobs.len();
        for (permit, job) in permits.into_iter().zip(jobs) {
            match policy.executor {
                JobExecutor::RuntimeExclusive => self.spawn_runtime_exclusive_job(job, permit),
                JobExecutor::RuntimeSnapshot => self.spawn_runtime_snapshot_job(job, permit),
                JobExecutor::AdapterAsync => self.spawn_adapter_job(job, permit),
                JobExecutor::BlockingSnapshot => self.spawn_blocking_snapshot_job(job, permit),
            }
        }
        Ok(json!({
            "scheduled": count,
            "availablePermits": lane.available_permits(),
            "activeOrderingKeys": blocked_keys.len(),
            "ordering": format!("{:?}", policy.ordering),
        }))
    }

    fn spawn_runtime_exclusive_job(&self, job: Job, permit: OwnedSemaphorePermit) {
        let timeline_store = self.timeline_store.clone();
        let notify = self.notify.clone();
        tokio::spawn(async move {
            let job_id = job.id.clone();
            let kind = job.kind;
            let result = {
                match Runtime::from_store(timeline_store) {
                    Ok(mut runtime) => runtime.dispatch_claimed_runtime_job(job).await,
                    Err(error) => Err(error),
                }
            };
            if let Err(error) = result {
                log(&format!(
                    "runtime-exclusive job worker failed {job_id} ({kind}): {}",
                    error_chain(&error)
                ));
            }
            drop(permit);
            notify.notify_one();
        });
    }

    fn spawn_runtime_snapshot_job(&self, job: Job, permit: OwnedSemaphorePermit) {
        let timeline_store = self.timeline_store.clone();
        let notify = self.notify.clone();
        tokio::spawn(async move {
            let job_id = job.id.clone();
            let kind = job.kind;
            let result = {
                match Runtime::from_store(timeline_store) {
                    Ok(mut runtime) => runtime.dispatch_claimed_runtime_job(job).await,
                    Err(error) => Err(error),
                }
            };
            if let Err(error) = result {
                log(&format!(
                    "runtime-snapshot job worker failed {job_id} ({kind}): {}",
                    error_chain(&error)
                ));
            }
            drop(permit);
            notify.notify_one();
        });
    }

    fn spawn_adapter_job(&self, job: Job, permit: OwnedSemaphorePermit) {
        let timeline_store = self.timeline_store.clone();
        let adapter = self.adapter_jobs.clone();
        let notify = self.notify.clone();
        tokio::spawn(async move {
            let job_id = job.id.clone();
            let kind = job.kind;
            let result = adapter.execute_adapter_job(job.clone()).await;
            let update = match result {
                Ok(output) => match Runtime::from_store(timeline_store.clone()) {
                    Ok(runtime) => runtime.complete_dispatched_job(&job_id, job, output).await,
                    Err(error) => Err(error),
                },
                Err(error) => match Runtime::from_store(timeline_store.clone()) {
                    Ok(runtime) => runtime.fail_dispatched_job(&job_id, job, error).await,
                    Err(error) => Err(error),
                },
            };
            if let Err(error) = update {
                log(&format!(
                    "adapter job worker failed {job_id} ({kind}): {}",
                    error_chain(&error)
                ));
            }
            drop(permit);
            notify.notify_one();
        });
    }

    fn spawn_blocking_snapshot_job(&self, job: Job, permit: OwnedSemaphorePermit) {
        let timeline_store = self.timeline_store.clone();
        let notify = self.notify.clone();
        tokio::spawn(async move {
            let job_id = job.id.clone();
            let kind = job.kind;
            let result = match Runtime::from_store(timeline_store) {
                Ok(snapshot) => snapshot.dispatch_claimed_blocking_job(job).await,
                Err(error) => {
                    log(&format!(
                        "blocking job worker failed {job_id} ({kind}) before dispatch: {}",
                        error_chain(&error)
                    ));
                    drop(permit);
                    notify.notify_one();
                    return;
                }
            };
            match result {
                Ok(_) => {}
                Err(error) => log(&format!(
                    "blocking job worker failed {job_id} ({kind}): {}",
                    error_chain(&error)
                )),
            }
            drop(permit);
            notify.notify_one();
        });
    }
}

impl JobLanes {
    fn from_env() -> Self {
        Self {
            wake: Arc::new(Semaphore::new(env_usize(
                "CLANKCORD_WAKE_JOB_CONCURRENCY",
                4,
                32,
            ))),
            audio: Arc::new(Semaphore::new(env_usize(
                "CLANKCORD_AUDIO_JOB_CONCURRENCY",
                32,
                128,
            ))),
            voice_control: Arc::new(Semaphore::new(env_usize(
                "CLANKCORD_VOICE_CONTROL_JOB_CONCURRENCY",
                32,
                128,
            ))),
            response: Arc::new(Semaphore::new(env_usize(
                "CLANKCORD_RESPONSE_JOB_CONCURRENCY",
                12,
                64,
            ))),
            refinement: Arc::new(Semaphore::new(env_usize(
                "CLANKCORD_REFINEMENT_JOB_CONCURRENCY",
                4,
                32,
            ))),
            agent: Arc::new(Semaphore::new(env_usize(
                "CLANKCORD_AGENT_JOB_CONCURRENCY",
                4,
                32,
            ))),
            async_jobs: Arc::new(Semaphore::new(env_usize(
                "CLANKCORD_ASYNC_JOB_CONCURRENCY",
                16,
                128,
            ))),
        }
    }

    fn semaphore(&self, lane: JobLane) -> Arc<Semaphore> {
        match lane {
            JobLane::GeneralAsync => self.async_jobs.clone(),
            JobLane::VoiceControl => self.voice_control.clone(),
            JobLane::Wake => self.wake.clone(),
            JobLane::Audio => self.audio.clone(),
            JobLane::Response => self.response.clone(),
            JobLane::Refinement => self.refinement.clone(),
            JobLane::Agent => self.agent.clone(),
        }
    }
}

fn take_permits(semaphore: &Arc<Semaphore>, max: usize) -> Vec<OwnedSemaphorePermit> {
    let mut permits = Vec::new();
    for _ in 0..max {
        match semaphore.clone().try_acquire_owned() {
            Ok(permit) => permits.push(permit),
            Err(_) => break,
        }
    }
    permits
}

fn dispatch_batch_limit(policy: JobExecutionPolicy) -> usize {
    match policy.lane {
        JobLane::Wake => env_usize("CLANKCORD_WAKE_JOB_BATCH_LIMIT", 8, 64),
        JobLane::Audio => env_usize("CLANKCORD_AUDIO_JOB_BATCH_LIMIT", 32, 128),
        JobLane::VoiceControl => env_usize("CLANKCORD_VOICE_CONTROL_JOB_BATCH_LIMIT", 32, 128),
        JobLane::Response => env_usize("CLANKCORD_RESPONSE_JOB_BATCH_LIMIT", 12, 64),
        JobLane::Refinement => env_usize("CLANKCORD_REFINEMENT_JOB_BATCH_LIMIT", 4, 32),
        JobLane::Agent => env_usize("CLANKCORD_AGENT_JOB_BATCH_LIMIT", 4, 32),
        JobLane::GeneralAsync => env_usize("CLANKCORD_ASYNC_JOB_BATCH_LIMIT", 16, 128),
    }
}

fn dispatch_drain_max_passes() -> usize {
    env_usize(
        "CLANKCORD_DISPATCH_DRAIN_MAX_PASSES",
        DEFAULT_DISPATCH_DRAIN_MAX_PASSES,
        512,
    )
}

fn env_usize(key: &str, default: usize, max: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(default)
        .clamp(1, max)
}

#[cfg(test)]
fn agent_session_key(job: &Job) -> String {
    AgentRuntime::task_session_key(&job.guild_id, &job.voice_channel_id)
}

#[cfg(test)]
fn ordering_key(ordering: JobOrdering, job: &Job) -> Option<String> {
    match ordering {
        JobOrdering::None => None,
        JobOrdering::VoiceTarget => voice_target_key(job),
        JobOrdering::WakeStream => wake_stream_key(job),
        JobOrdering::AgentSession => Some(format!("agent:{}", agent_session_key(job))),
    }
}

#[cfg(test)]
fn wake_stream_key(job: &Job) -> Option<String> {
    match &job.payload {
        JobPayload::WakeProbe(payload) => Some(format!("wake:stream:{}", payload.stream_id)),
        _ => None,
    }
}

#[cfg(test)]
fn voice_target_key(job: &Job) -> Option<String> {
    match &job.payload {
        JobPayload::RoomAgentPlacement(payload) => {
            let room_key = if payload.room_id.trim().is_empty() {
                job.voice_channel_id.as_str()
            } else {
                payload.room_id.as_str()
            };
            Some(format!(
                "room:placement:{}:{}",
                ordering_key_part(&job.guild_id),
                ordering_key_part(room_key)
            ))
        }
        JobPayload::DiscordVoiceJoin(payload) => Some(format!("voice:bot:{}", payload.bot_id)),
        JobPayload::DiscordVoiceLeave(payload) => {
            Some(format!("voice:session:{}", payload.session_id))
        }
        JobPayload::DiscordVoicePlayback(payload) => {
            Some(format!("voice:session:{}", payload.session_id))
        }
        JobPayload::DiscordVoiceMute(payload) => {
            Some(format!("voice:session:{}", payload.session_id))
        }
        JobPayload::DiscordVoicePlayAudio(payload) => {
            Some(format!("voice:session:{}", payload.session_id))
        }
        _ => None,
    }
}

#[cfg(test)]
fn ordering_key_part(value: &str) -> String {
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

fn scheduled_job_count(report: &Value) -> usize {
    report
        .get("totalScheduled")
        .and_then(Value::as_u64)
        .map(|value| value as usize)
        .unwrap_or_else(|| {
            report
                .as_object()
                .map(|object| object.values().map(scheduled_count_for_kind).sum())
                .unwrap_or(0)
        })
}

fn scheduled_count_for_kind(value: &Value) -> usize {
    value
        .get("scheduled")
        .and_then(Value::as_u64)
        .map(|value| value as usize)
        .unwrap_or(0)
}

fn idle_policy_report(policy: JobExecutionPolicy) -> Value {
    json!({
        "scheduled": 0,
        "availablePermits": 0,
        "activeOrderingKeys": 0,
        "skipped": "no_due_jobs",
        "lane": format!("{:?}", policy.lane),
    })
}

fn error_chain(error: &anyhow::Error) -> String {
    error
        .chain()
        .map(|cause| cause.to_string())
        .collect::<Vec<_>>()
        .join(": ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{DiscordVoiceMutePayload, DiscordVoicePlaybackCue};

    #[test]
    fn execution_policy_prioritizes_urgent_voice_and_wake_before_bulk_audio() {
        let position = |kind| {
            JOB_EXECUTION_POLICIES
                .iter()
                .position(|policy| policy.kind == kind)
                .expect("job kind should have a policy")
        };

        assert!(position(JobKind::DiscordVoiceMute) < position(JobKind::AudioSegment));
        assert!(position(JobKind::DiscordVoicePlayAudio) < position(JobKind::AudioSegment));
        assert!(position(JobKind::DiscordVoicePlayback) < position(JobKind::AudioSegment));
        assert!(position(JobKind::WakeProbe) < position(JobKind::AudioSegment));
        assert!(position(JobKind::AudioSegment) < position(JobKind::WakeActivation));
    }

    #[test]
    fn store_owned_orchestration_uses_snapshot_execution() {
        let policy = |kind| {
            JOB_EXECUTION_POLICIES
                .iter()
                .find(|policy| policy.kind == kind)
                .copied()
                .expect("job kind should have a policy")
        };

        assert_eq!(
            policy(JobKind::DiscordVoicePlayback).executor,
            JobExecutor::RuntimeSnapshot
        );
        assert_eq!(
            policy(JobKind::WakeActivation).executor,
            JobExecutor::RuntimeSnapshot
        );
        assert_eq!(
            policy(JobKind::RoomAgentPlacement).executor,
            JobExecutor::RuntimeExclusive
        );
        assert_eq!(
            policy(JobKind::WakeProbe).executor,
            JobExecutor::BlockingSnapshot
        );
    }

    #[test]
    fn voice_target_ordering_uses_session_key_for_playback_control() {
        let job = Job::discord_voice_mute(
            "guild",
            "code",
            "user-a",
            DiscordVoiceMutePayload {
                session_id: "cap_test".to_string(),
                muted: false,
                source_job_id: "source".to_string(),
                reason: DiscordVoicePlaybackCue::Wake.as_str().to_string(),
            },
        );

        assert_eq!(
            ordering_key(JobOrdering::VoiceTarget, &job).as_deref(),
            Some("voice:session:cap_test")
        );
    }
}
