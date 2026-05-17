use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::{Map, Value, json};
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore};

use crate::Result;
use crate::config;
use crate::runtime::domain::external::RuntimeExternalApi;
use crate::runtime::timeline::TimelineStore;
use crate::runtime::{Job, JobKind, Runtime, log};

const JOB_EXECUTION_POLICIES: [JobExecutionPolicy; 34] = [
    JobExecutionPolicy::runtime_exclusive(
        JobKind::RuntimeControl,
        JobLane::GeneralAsync,
        JobOrdering::None,
    ),
    JobExecutionPolicy::runtime_snapshot(
        JobKind::RuntimeMaintenance,
        JobLane::Maintenance,
        JobOrdering::RuntimeMaintenance,
    ),
    JobExecutionPolicy::runtime_snapshot(
        JobKind::VoiceStatusSync,
        JobLane::Maintenance,
        JobOrdering::RuntimeMaintenance,
    ),
    JobExecutionPolicy::runtime_snapshot(
        JobKind::DiscordVoiceStatusSnapshot,
        JobLane::Maintenance,
        JobOrdering::RuntimeMaintenance,
    ),
    JobExecutionPolicy::runtime_snapshot(
        JobKind::AutomationEvaluation,
        JobLane::Maintenance,
        JobOrdering::RuntimeMaintenance,
    ),
    JobExecutionPolicy::runtime_snapshot(
        JobKind::AgentSessionRetirement,
        JobLane::Maintenance,
        JobOrdering::RuntimeMaintenance,
    ),
    JobExecutionPolicy::runtime_snapshot(
        JobKind::StaleWakeProbeSweep,
        JobLane::Maintenance,
        JobOrdering::RuntimeMaintenance,
    ),
    JobExecutionPolicy::runtime_snapshot(
        JobKind::StaleRunningJobSweep,
        JobLane::Maintenance,
        JobOrdering::RuntimeMaintenance,
    ),
    JobExecutionPolicy::runtime_snapshot(
        JobKind::EphemeralJobGc,
        JobLane::Maintenance,
        JobOrdering::RuntimeMaintenance,
    ),
    JobExecutionPolicy::runtime_snapshot(
        JobKind::DiscordVoiceMute,
        JobLane::VoiceControl,
        JobOrdering::VoiceTarget,
    ),
    JobExecutionPolicy::runtime_snapshot(
        JobKind::DiscordVoiceDeafen,
        JobLane::VoiceControl,
        JobOrdering::VoiceTarget,
    ),
    JobExecutionPolicy::runtime_snapshot(
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
        JobOrdering::IngressRoute,
    ),
    JobExecutionPolicy::runtime_exclusive(
        JobKind::RoomAgentPlacement,
        JobLane::GeneralAsync,
        JobOrdering::VoiceTarget,
    ),
    JobExecutionPolicy::runtime_snapshot(
        JobKind::DiscordVoiceJoin,
        JobLane::VoiceControl,
        JobOrdering::VoiceTarget,
    ),
    JobExecutionPolicy::runtime_snapshot(
        JobKind::DiscordVoiceLeave,
        JobLane::VoiceControl,
        JobOrdering::VoiceTarget,
    ),
    JobExecutionPolicy::runtime_exclusive(
        JobKind::Command,
        JobLane::GeneralAsync,
        JobOrdering::IngressRoute,
    ),
    JobExecutionPolicy::runtime_exclusive(
        JobKind::DiscordTextMessage,
        JobLane::GeneralAsync,
        JobOrdering::IngressRoute,
    ),
    JobExecutionPolicy::runtime_exclusive(
        JobKind::DiscordSlashCommand,
        JobLane::GeneralAsync,
        JobOrdering::IngressRoute,
    ),
    JobExecutionPolicy::runtime_exclusive(
        JobKind::TextDelivery,
        JobLane::GeneralAsync,
        JobOrdering::TextTarget,
    ),
    JobExecutionPolicy::runtime_exclusive(
        JobKind::ConfirmationRequired,
        JobLane::GeneralAsync,
        JobOrdering::TextTarget,
    ),
    JobExecutionPolicy::runtime_exclusive(
        JobKind::AgentSessionStart,
        JobLane::GeneralAsync,
        JobOrdering::AgentSession,
    ),
    JobExecutionPolicy::runtime_exclusive(
        JobKind::AgentSessionSunset,
        JobLane::GeneralAsync,
        JobOrdering::AgentSession,
    ),
    JobExecutionPolicy::runtime_exclusive(
        JobKind::AgentSessionResume,
        JobLane::GeneralAsync,
        JobOrdering::AgentSession,
    ),
    JobExecutionPolicy::runtime_exclusive(
        JobKind::TranscriptPublication,
        JobLane::GeneralAsync,
        JobOrdering::TextTarget,
    ),
    JobExecutionPolicy::runtime_snapshot(
        JobKind::DiscordTextSend,
        JobLane::DiscordText,
        JobOrdering::TextTarget,
    ),
    JobExecutionPolicy::runtime_snapshot(
        JobKind::DiscordForumThreadCreate,
        JobLane::DiscordText,
        JobOrdering::TextTarget,
    ),
    JobExecutionPolicy::runtime_snapshot(
        JobKind::DiscordForumThreadRename,
        JobLane::DiscordText,
        JobOrdering::TextTarget,
    ),
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
    JobExecutionPolicy::blocking_snapshot(
        JobKind::AgentThreadTitleRefresh,
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
    BlockingSnapshot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JobLane {
    GeneralAsync,
    VoiceControl,
    DiscordText,
    Wake,
    Audio,
    Refinement,
    Agent,
    Maintenance,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JobOrdering {
    None,
    VoiceTarget,
    WakeStream,
    IngressRoute,
    TextTarget,
    AgentSession,
    RuntimeMaintenance,
}

#[derive(Clone)]
pub(crate) struct RuntimeExecutor<E>
where
    E: RuntimeExternalApi + Clone + Send + Sync + 'static,
{
    external_api: E,
    timeline_store: TimelineStore,
    lanes: Arc<JobLanes>,
    notify: Arc<Notify>,
}

struct JobLanes {
    wake: Arc<Semaphore>,
    audio: Arc<Semaphore>,
    voice_control: Arc<Semaphore>,
    discord_text: Arc<Semaphore>,
    refinement: Arc<Semaphore>,
    agent: Arc<Semaphore>,
    maintenance: Arc<Semaphore>,
    async_jobs: Arc<Semaphore>,
}

impl<E> RuntimeExecutor<E>
where
    E: RuntimeExternalApi + Clone + Send + Sync + 'static,
{
    pub(crate) fn new(external_api: E, timeline_store: TimelineStore) -> Self {
        Self {
            external_api,
            timeline_store,
            lanes: Arc::new(JobLanes::from_config()),
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

    pub(crate) async fn next_queued_job_ready_at(&self) -> Result<Option<DateTime<Utc>>> {
        self.timeline_store.next_queued_job_ready_at().await
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

    pub(crate) async fn wait_for_voice_idle(&self, timeout: Duration) -> Value {
        self.wait_for_lanes(timeout, &[JobLane::VoiceControl]).await
    }

    pub(crate) async fn wait_for_idle(&self, timeout: Duration) -> Value {
        self.wait_for_lanes(
            timeout,
            &[
                JobLane::VoiceControl,
                JobLane::GeneralAsync,
                JobLane::DiscordText,
                JobLane::Maintenance,
                JobLane::Wake,
                JobLane::Audio,
                JobLane::Refinement,
                JobLane::Agent,
            ],
        )
        .await
    }

    async fn wait_for_lanes(&self, timeout: Duration, lanes: &[JobLane]) -> Value {
        let deadline = tokio::time::Instant::now() + timeout;
        let mut reports = Vec::new();
        let mut idle = true;
        for lane in lanes {
            let Some((name, semaphore, capacity)) = self.lanes.lane_entry(*lane) else {
                continue;
            };
            let active_before = capacity.saturating_sub(semaphore.available_permits());
            if active_before == 0 {
                reports.push(json!({
                    "lane": name,
                    "status": "idle",
                    "activeBefore": active_before,
                }));
                continue;
            }
            let now = tokio::time::Instant::now();
            let remaining = deadline.saturating_duration_since(now);
            if remaining.is_zero() {
                idle = false;
                reports.push(json!({
                    "lane": name,
                    "status": "timeout",
                    "activeBefore": active_before,
                    "activeAfter": capacity.saturating_sub(semaphore.available_permits()),
                }));
                continue;
            }
            match tokio::time::timeout(
                remaining,
                semaphore.clone().acquire_many_owned(capacity as u32),
            )
            .await
            {
                Ok(Ok(permit)) => {
                    drop(permit);
                    reports.push(json!({
                        "lane": name,
                        "status": "idle",
                        "activeBefore": active_before,
                    }));
                }
                Ok(Err(error)) => {
                    idle = false;
                    reports.push(json!({
                        "lane": name,
                        "status": "closed",
                        "activeBefore": active_before,
                        "error": error.to_string(),
                    }));
                }
                Err(_) => {
                    idle = false;
                    reports.push(json!({
                        "lane": name,
                        "status": "timeout",
                        "activeBefore": active_before,
                        "activeAfter": capacity.saturating_sub(semaphore.available_permits()),
                    }));
                }
            }
        }
        json!({
            "idle": idle,
            "timeoutMs": timeout.as_millis().min(u128::from(u64::MAX)) as u64,
            "lanes": reports,
        })
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
        let external_api = self.external_api.clone();
        let notify = self.notify.clone();
        tokio::spawn(async move {
            let job_id = job.id.clone();
            let kind = job.kind;
            let result = {
                match Runtime::from_store(timeline_store) {
                    Ok(mut runtime) => {
                        runtime
                            .dispatch_claimed_runtime_job_with_external_api(job, &external_api)
                            .await
                    }
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
        let external_api = self.external_api.clone();
        let notify = self.notify.clone();
        tokio::spawn(async move {
            let job_id = job.id.clone();
            let kind = job.kind;
            let result = {
                match Runtime::from_store(timeline_store) {
                    Ok(mut runtime) => {
                        runtime
                            .dispatch_claimed_runtime_job_with_external_api(job, &external_api)
                            .await
                    }
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

    fn spawn_blocking_snapshot_job(&self, job: Job, permit: OwnedSemaphorePermit) {
        let timeline_store = self.timeline_store.clone();
        let notify = self.notify.clone();
        let runtime_handle = tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || {
            let job_id = job.id.clone();
            let kind = job.kind;
            let result = runtime_handle.block_on(async move {
                match Runtime::from_store(timeline_store) {
                    Ok(snapshot) => snapshot.dispatch_claimed_blocking_job(job).await,
                    Err(error) => Err(error),
                }
            });
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
    fn from_config() -> Self {
        let concurrency = config::job_concurrency();
        Self {
            wake: Arc::new(Semaphore::new(concurrency.wake.clamp(1, 32))),
            audio: Arc::new(Semaphore::new(concurrency.audio.clamp(1, 128))),
            voice_control: Arc::new(Semaphore::new(concurrency.voice_control.clamp(1, 128))),
            discord_text: Arc::new(Semaphore::new(concurrency.discord_text.clamp(1, 64))),
            refinement: Arc::new(Semaphore::new(concurrency.refinement.clamp(1, 32))),
            agent: Arc::new(Semaphore::new(concurrency.agent.clamp(1, 32))),
            maintenance: Arc::new(Semaphore::new(concurrency.maintenance.clamp(1, 1))),
            async_jobs: Arc::new(Semaphore::new(concurrency.general_async.clamp(1, 128))),
        }
    }

    fn semaphore(&self, lane: JobLane) -> Arc<Semaphore> {
        match lane {
            JobLane::GeneralAsync => self.async_jobs.clone(),
            JobLane::VoiceControl => self.voice_control.clone(),
            JobLane::DiscordText => self.discord_text.clone(),
            JobLane::Wake => self.wake.clone(),
            JobLane::Audio => self.audio.clone(),
            JobLane::Refinement => self.refinement.clone(),
            JobLane::Agent => self.agent.clone(),
            JobLane::Maintenance => self.maintenance.clone(),
        }
    }

    fn lane_entry(&self, lane: JobLane) -> Option<(&'static str, Arc<Semaphore>, usize)> {
        let concurrency = config::job_concurrency();
        Some(match lane {
            JobLane::GeneralAsync => (
                "general_async",
                self.async_jobs.clone(),
                concurrency.general_async.clamp(1, 128),
            ),
            JobLane::VoiceControl => (
                "voice_control",
                self.voice_control.clone(),
                concurrency.voice_control.clamp(1, 128),
            ),
            JobLane::DiscordText => (
                "discord_text",
                self.discord_text.clone(),
                concurrency.discord_text.clamp(1, 64),
            ),
            JobLane::Wake => ("wake", self.wake.clone(), concurrency.wake.clamp(1, 32)),
            JobLane::Audio => ("audio", self.audio.clone(), concurrency.audio.clamp(1, 128)),
            JobLane::Refinement => (
                "refinement",
                self.refinement.clone(),
                concurrency.refinement.clamp(1, 32),
            ),
            JobLane::Agent => ("agent", self.agent.clone(), concurrency.agent.clamp(1, 32)),
            JobLane::Maintenance => (
                "maintenance",
                self.maintenance.clone(),
                concurrency.maintenance.clamp(1, 1),
            ),
        })
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
    let batch = config::job_batch_limits();
    match policy.lane {
        JobLane::Wake => batch.wake.clamp(1, 64),
        JobLane::Audio => batch.audio.clamp(1, 128),
        JobLane::VoiceControl => batch.voice_control.clamp(1, 128),
        JobLane::DiscordText => batch.discord_text.clamp(1, 64),
        JobLane::Refinement => batch.refinement.clamp(1, 32),
        JobLane::Agent => batch.agent.clamp(1, 32),
        JobLane::Maintenance => batch.maintenance.clamp(1, 1),
        JobLane::GeneralAsync => batch.general_async.clamp(1, 128),
    }
}

fn dispatch_drain_max_passes() -> usize {
    config::dispatch_drain_max_passes()
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
