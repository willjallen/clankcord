use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::{Map, Value, json};
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore};

use crate::Result;
use crate::config;
use crate::domain::Ctx;
use crate::domain::maintenance::execution;
use crate::engine::dispatcher;
use crate::engine::schedules;
use crate::model::job::spec::{JobExecutor, JobLane, spec};
use crate::model::job::{Job, JobKind};
use crate::ports::discord::DiscordApi;
use crate::store::TimelineStore;
use crate::util::log;

#[derive(Clone)]
pub(crate) struct RuntimeExecutor<E>
where
    E: DiscordApi + Clone + Send + Sync + 'static,
{
    external_api: E,
    timeline_store: TimelineStore,
    lanes: Arc<JobLanes>,
    notify: Arc<Notify>,
}

struct JobLanes {
    wake: Lane,
    audio_segment: Lane,
    transcription_mux: Lane,
    voice_control: Lane,
    discord_text: Lane,
    agent: Lane,
    maintenance: Lane,
    async_jobs: Lane,
}

/// A lane's semaphore and the capacity it was built with. Capacity lives
/// beside the semaphore so idle-waits acquire exactly the permits that
/// exist, whatever the config says by the time they run.
struct Lane {
    semaphore: Arc<Semaphore>,
    capacity: usize,
}

impl Lane {
    fn new(capacity: usize) -> Self {
        Self {
            semaphore: Arc::new(Semaphore::new(capacity)),
            capacity,
        }
    }
}

/// One drain cycle over the dispatch passes: typed control flow for the
/// dispatch loop, serialized only at the HTTP edge.
pub struct DrainReport {
    pub exhausted: bool,
    total_timed_out_running: usize,
    total_resolved: usize,
    total_scheduled: usize,
    passes: Vec<Value>,
}

impl DrainReport {
    pub fn to_json(&self) -> Value {
        json!({
            "ok": true,
            "passes": self.passes,
            "totalTimedOutRunningJobs": self.total_timed_out_running,
            "totalResolvedWaiting": self.total_resolved,
            "totalScheduled": self.total_scheduled,
            "exhausted": self.exhausted,
        })
    }
}

struct ScheduleRound {
    per_kind: Vec<(JobKind, KindSchedule)>,
}

struct KindSchedule {
    scheduled: usize,
    available_permits: usize,
    active_ordering_keys: usize,
    lane: &'static str,
}

impl ScheduleRound {
    fn total_scheduled(&self) -> usize {
        self.per_kind
            .iter()
            .map(|(_, schedule)| schedule.scheduled)
            .sum()
    }

    fn to_json(&self) -> Value {
        let mut object = Map::new();
        for (kind, schedule) in &self.per_kind {
            object.insert(
                kind.as_str().to_string(),
                json!({
                    "scheduled": schedule.scheduled,
                    "availablePermits": schedule.available_permits,
                    "activeOrderingKeys": schedule.active_ordering_keys,
                    "lane": schedule.lane,
                }),
            );
        }
        object.insert("totalScheduled".to_string(), json!(self.total_scheduled()));
        Value::Object(object)
    }
}

impl<E> RuntimeExecutor<E>
where
    E: DiscordApi + Clone + Send + Sync + 'static,
{
    pub(crate) fn new(external_api: E, timeline_store: TimelineStore) -> Self {
        Self {
            external_api,
            notify: timeline_store.dispatch_notify(),
            timeline_store,
            lanes: Arc::new(JobLanes::from_config()),
        }
    }

    pub(crate) fn notify_handle(&self) -> Arc<Notify> {
        self.notify.clone()
    }

    pub(crate) fn wake(&self) {
        self.notify.notify_one();
    }

    /// Schedules every kind with a due queued row. Iterating kinds reported
    /// by Postgres (instead of a hardcoded table) means a queued job of any
    /// kind is always considered — a kind cannot be silently unschedulable.
    async fn schedule_due_jobs(&self) -> Result<ScheduleRound> {
        let mut per_kind = Vec::new();
        let due_kinds = self.timeline_store.due_job_kinds().await?;
        for kind in due_kinds {
            per_kind.push((kind, self.schedule_kind(kind).await?));
        }
        Ok(ScheduleRound { per_kind })
    }

    pub(crate) async fn next_queued_job_ready_at(&self) -> Result<Option<DateTime<Utc>>> {
        self.timeline_store.next_queued_job_ready_at().await
    }

    pub(crate) async fn drain_ready_jobs(&self) -> Result<DrainReport> {
        let max_passes = dispatch_drain_max_passes();
        let mut passes = Vec::new();
        let mut total_timed_out_running = 0usize;
        let mut total_resolved = 0usize;
        let mut total_scheduled = 0usize;
        let mut exhausted = false;

        for pass in 0..max_passes {
            let timed_out_running_jobs =
                execution::recover_stale_running_jobs_for_maintenance_pass(&Ctx::new(
                    self.timeline_store.clone(),
                ))
                .await?;
            let schedule_submissions = schedules::run_due_schedules(
                &self.timeline_store,
                &crate::engine::JobBus::new(self.timeline_store.clone()),
            )
            .await?;
            let resolved_waiting = self.timeline_store.resolve_waiting_jobs().await?;
            let scheduled = self.schedule_due_jobs().await?;
            let scheduled_count = scheduled.total_scheduled();
            let timed_out_count = timed_out_running_jobs.len();
            let schedule_count = schedule_submissions.len();
            let resolved_count = resolved_waiting.len();
            total_timed_out_running += timed_out_count;
            total_resolved += resolved_count;
            total_scheduled += scheduled_count;
            passes.push(json!({
                "pass": pass + 1,
                "timedOutRunningJobs": timed_out_running_jobs,
                "scheduleSubmissions": schedule_submissions,
                "resolvedWaiting": resolved_waiting,
                "scheduled": scheduled.to_json(),
            }));
            if timed_out_count == 0
                && resolved_count == 0
                && scheduled_count == 0
                && schedule_count == 0
            {
                exhausted = true;
                break;
            }
            tokio::task::yield_now().await;
        }

        Ok(DrainReport {
            exhausted,
            total_timed_out_running,
            total_resolved,
            total_scheduled,
            passes,
        })
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
                JobLane::AudioSegment,
                JobLane::TranscriptionMux,
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
            let entry = self.lanes.lane(*lane);
            let name = lane.as_str();
            let capacity = entry.capacity;
            let semaphore = entry.semaphore.clone();
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

    async fn schedule_kind(&self, kind: JobKind) -> Result<KindSchedule> {
        let job_spec = spec(kind);
        let lane = self.lanes.semaphore(job_spec.lane);
        let permits = take_permits(&lane, dispatch_batch_limit(job_spec.lane));
        let permit_count = permits.len();
        let mut blocked_keys = self.timeline_store.active_ordering_keys().await?;
        let jobs = self
            .timeline_store
            .claim_due_jobs(kind, permit_count, &mut blocked_keys)
            .await?;
        let count = jobs.len();
        for (permit, job) in permits.into_iter().zip(jobs) {
            match job_spec.executor {
                JobExecutor::Async => self.spawn_runtime_job(job, permit),
                JobExecutor::Blocking => self.spawn_blocking_job(job, permit),
            }
        }
        Ok(KindSchedule {
            scheduled: count,
            available_permits: lane.available_permits(),
            active_ordering_keys: blocked_keys.len(),
            lane: job_spec.lane.as_str(),
        })
    }

    fn spawn_runtime_job(&self, job: Job, permit: OwnedSemaphorePermit) {
        let timeline_store = self.timeline_store.clone();
        let external_api = self.external_api.clone();
        let notify = self.notify.clone();
        tokio::spawn(async move {
            let job_id = job.id.clone();
            let kind = job.kind;
            let ctx = Ctx::new(timeline_store);
            let result = dispatcher::dispatch_claimed_job(&ctx, &external_api, job).await;
            if let Err(error) = result {
                log(&format!(
                    "runtime job worker failed {job_id} ({kind}): {}",
                    error_chain(&error)
                ));
            }
            drop(permit);
            notify.notify_one();
        });
    }

    fn spawn_blocking_job(&self, job: Job, permit: OwnedSemaphorePermit) {
        let timeline_store = self.timeline_store.clone();
        let external_api = self.external_api.clone();
        let notify = self.notify.clone();
        let runtime_handle = tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || {
            let job_id = job.id.clone();
            let kind = job.kind;
            let result = runtime_handle.block_on(async move {
                let ctx = Ctx::new(timeline_store);
                dispatcher::dispatch_claimed_job(&ctx, &external_api, job).await
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
            wake: Lane::new(concurrency.wake.clamp(1, 32)),
            audio_segment: Lane::new(concurrency.audio_segment.clamp(1, 128)),
            transcription_mux: Lane::new(config::transcription_mux_provider_streams()),
            voice_control: Lane::new(concurrency.voice_control.clamp(1, 128)),
            discord_text: Lane::new(concurrency.discord_text.clamp(1, 64)),
            agent: Lane::new(concurrency.agent.clamp(1, 32)),
            maintenance: Lane::new(concurrency.maintenance.clamp(1, 1)),
            async_jobs: Lane::new(concurrency.general_async.clamp(1, 128)),
        }
    }

    fn lane(&self, lane: JobLane) -> &Lane {
        match lane {
            JobLane::GeneralAsync => &self.async_jobs,
            JobLane::VoiceControl => &self.voice_control,
            JobLane::DiscordText => &self.discord_text,
            JobLane::Wake => &self.wake,
            JobLane::AudioSegment => &self.audio_segment,
            JobLane::TranscriptionMux => &self.transcription_mux,
            JobLane::Agent => &self.agent,
            JobLane::Maintenance => &self.maintenance,
        }
    }

    fn semaphore(&self, lane: JobLane) -> Arc<Semaphore> {
        self.lane(lane).semaphore.clone()
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

fn dispatch_batch_limit(lane: JobLane) -> usize {
    let batch = config::job_batch_limits();
    match lane {
        JobLane::Wake => batch.wake.clamp(1, 64),
        JobLane::AudioSegment => batch.audio_segment.clamp(1, 128),
        JobLane::TranscriptionMux => config::transcription_mux_provider_streams(),
        JobLane::VoiceControl => batch.voice_control.clamp(1, 128),
        JobLane::DiscordText => batch.discord_text.clamp(1, 64),
        JobLane::Agent => batch.agent.clamp(1, 32),
        JobLane::Maintenance => batch.maintenance.clamp(1, 1),
        JobLane::GeneralAsync => batch.general_async.clamp(1, 128),
    }
}

fn dispatch_drain_max_passes() -> usize {
    config::dispatch_drain_max_passes()
}

fn error_chain(error: &anyhow::Error) -> String {
    error
        .chain()
        .map(|cause| cause.to_string())
        .collect::<Vec<_>>()
        .join(": ")
}
