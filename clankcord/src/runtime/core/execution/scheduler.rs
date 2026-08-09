use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::{Map, Value, json};
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore};

use crate::Result;
use crate::config;
use crate::model::job::spec::{JobExecutor, JobLane, spec};
use crate::model::job::{Job, JobKind};
use crate::ports::discord::DiscordApi;
use crate::runtime::Ctx;
use crate::runtime::timeline::TimelineStore;
use crate::runtime::util::log;

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
    wake: Arc<Semaphore>,
    audio_segment: Arc<Semaphore>,
    transcription_mux: Arc<Semaphore>,
    voice_control: Arc<Semaphore>,
    discord_text: Arc<Semaphore>,
    agent: Arc<Semaphore>,
    maintenance: Arc<Semaphore>,
    async_jobs: Arc<Semaphore>,
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
    pub(crate) async fn schedule_due_jobs(&self) -> Result<Value> {
        let mut scheduled = Map::new();
        let due_kinds = self.timeline_store.due_job_kinds().await?;
        for kind in due_kinds {
            scheduled.insert(kind.as_str().to_string(), self.schedule_kind(kind).await?);
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
        let mut total_timed_out_running = 0usize;
        let mut total_resolved = 0usize;
        let mut total_scheduled = 0usize;
        let mut exhausted = false;

        for pass in 0..max_passes {
            let timed_out_running_jobs =
                crate::runtime::domain::maintenance::execution::recover_stale_running_jobs_for_maintenance_pass(
                    &Ctx::new(self.timeline_store.clone()),
                )
                .await?;
            let schedule_submissions = crate::engine::schedules::run_due_schedules(
                &self.timeline_store,
                &crate::engine::JobBus::new(self.timeline_store.clone()),
            )
            .await?;
            let resolved_waiting = self.timeline_store.resolve_waiting_jobs().await?;
            let scheduled = self.schedule_due_jobs().await?;
            let scheduled_count = scheduled_job_count(&scheduled);
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
                "scheduled": scheduled,
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

        Ok(json!({
            "ok": true,
            "passes": passes,
            "totalTimedOutRunningJobs": total_timed_out_running,
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

    async fn schedule_kind(&self, kind: JobKind) -> Result<Value> {
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
        Ok(json!({
            "scheduled": count,
            "availablePermits": lane.available_permits(),
            "activeOrderingKeys": blocked_keys.len(),
            "lane": job_spec.lane.as_str(),
        }))
    }

    fn spawn_runtime_job(&self, job: Job, permit: OwnedSemaphorePermit) {
        let timeline_store = self.timeline_store.clone();
        let external_api = self.external_api.clone();
        let notify = self.notify.clone();
        tokio::spawn(async move {
            let job_id = job.id.clone();
            let kind = job.kind;
            let ctx = Ctx::new(timeline_store);
            let result = crate::runtime::core::execution::dispatcher::dispatch_claimed_runtime_job(
                &ctx,
                &external_api,
                job,
            )
            .await;
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
        let notify = self.notify.clone();
        let runtime_handle = tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || {
            let job_id = job.id.clone();
            let kind = job.kind;
            let result = runtime_handle.block_on(async move {
                let ctx = Ctx::new(timeline_store);
                crate::runtime::core::execution::dispatcher::dispatch_claimed_blocking_job(
                    &ctx, job,
                )
                .await
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
            audio_segment: Arc::new(Semaphore::new(concurrency.audio_segment.clamp(1, 128))),
            transcription_mux: Arc::new(Semaphore::new(
                config::transcription_mux_provider_streams(),
            )),
            voice_control: Arc::new(Semaphore::new(concurrency.voice_control.clamp(1, 128))),
            discord_text: Arc::new(Semaphore::new(concurrency.discord_text.clamp(1, 64))),
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
            JobLane::AudioSegment => self.audio_segment.clone(),
            JobLane::TranscriptionMux => self.transcription_mux.clone(),
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
            JobLane::AudioSegment => (
                "audio_segment",
                self.audio_segment.clone(),
                concurrency.audio_segment.clamp(1, 128),
            ),
            JobLane::TranscriptionMux => (
                "transcription_mux",
                self.transcription_mux.clone(),
                config::transcription_mux_provider_streams(),
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

fn error_chain(error: &anyhow::Error) -> String {
    error
        .chain()
        .map(|cause| cause.to_string())
        .collect::<Vec<_>>()
        .join(": ")
}
