use std::collections::BTreeSet;
use std::sync::{Arc, Mutex as StdMutex};

use anyhow::Context;
use serde_json::{Map, Value, json};
use tokio::sync::{Mutex, Notify, OwnedSemaphorePermit, Semaphore};

use crate::Result;
use crate::runtime::core::execution::RuntimeAdapterJobs;
use crate::runtime::timeline::TimelineStore;
use crate::runtime::{AgentRuntime, Job, JobKind, Runtime, log};

#[derive(Clone)]
pub(crate) struct RuntimeExecutor<E>
where
    E: RuntimeAdapterJobs + Clone + Send + Sync + 'static,
{
    runtime: Arc<Mutex<Runtime>>,
    adapter_jobs: E,
    timeline_store: TimelineStore,
    lanes: Arc<JobLanes>,
    notify: Arc<Notify>,
}

struct JobLanes {
    audio: Arc<Semaphore>,
    response: Arc<Semaphore>,
    refinement: Arc<Semaphore>,
    agent: Arc<Semaphore>,
    async_jobs: Arc<Semaphore>,
    active_agent_sessions: StdMutex<BTreeSet<String>>,
}

impl<E> RuntimeExecutor<E>
where
    E: RuntimeAdapterJobs + Clone + Send + Sync + 'static,
{
    pub(crate) fn new(
        runtime: Arc<Mutex<Runtime>>,
        adapter_jobs: E,
        timeline_store: TimelineStore,
    ) -> Self {
        Self {
            runtime,
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

    pub(crate) fn schedule_due_jobs(&self) -> Result<Value> {
        let mut scheduled = Map::new();
        scheduled.insert(
            JobKind::RuntimeControl.as_str().to_string(),
            self.schedule_async_kind(JobKind::RuntimeControl)?,
        );
        scheduled.insert(
            JobKind::AudioSegment.as_str().to_string(),
            self.schedule_blocking_kind(JobKind::AudioSegment, &self.lanes.audio)?,
        );
        scheduled.insert(
            JobKind::WakeActivation.as_str().to_string(),
            self.schedule_async_kind(JobKind::WakeActivation)?,
        );
        scheduled.insert(
            JobKind::Command.as_str().to_string(),
            self.schedule_async_kind(JobKind::Command)?,
        );
        scheduled.insert(
            JobKind::RoomAgentPlacement.as_str().to_string(),
            self.schedule_async_kind(JobKind::RoomAgentPlacement)?,
        );
        scheduled.insert(
            JobKind::DiscordVoiceJoin.as_str().to_string(),
            self.schedule_adapter_kind(JobKind::DiscordVoiceJoin)?,
        );
        scheduled.insert(
            JobKind::DiscordVoiceLeave.as_str().to_string(),
            self.schedule_adapter_kind(JobKind::DiscordVoiceLeave)?,
        );
        scheduled.insert(
            JobKind::Response.as_str().to_string(),
            self.schedule_blocking_kind(JobKind::Response, &self.lanes.response)?,
        );
        scheduled.insert(
            JobKind::RefineTranscript.as_str().to_string(),
            self.schedule_blocking_kind(JobKind::RefineTranscript, &self.lanes.refinement)?,
        );
        scheduled.insert(
            JobKind::AgentTask.as_str().to_string(),
            self.schedule_agent_tasks()?,
        );
        Ok(Value::Object(scheduled))
    }

    pub(crate) async fn run_maintenance(&self) -> Result<Value> {
        let snapshot = {
            let runtime = self.runtime.lock().await;
            runtime.clone()
        };
        tokio::task::spawn_blocking(move || snapshot.run_blocking_maintenance())
            .await
            .context("joining blocking maintenance task")?
    }

    fn schedule_async_kind(&self, kind: JobKind) -> Result<Value> {
        let permits = take_permits(&self.lanes.async_jobs, async_dispatch_batch_limit());
        let permit_count = permits.len();
        let jobs = self
            .timeline_store
            .claim_due_jobs(kind, permit_count, |_| false)?;
        let count = jobs.len();
        for (permit, job) in permits.into_iter().zip(jobs) {
            self.spawn_async_job(job, permit);
        }
        Ok(json!({
            "scheduled": count,
            "availablePermits": self.lanes.async_jobs.available_permits(),
        }))
    }

    fn schedule_adapter_kind(&self, kind: JobKind) -> Result<Value> {
        let permits = take_permits(&self.lanes.async_jobs, async_dispatch_batch_limit());
        let permit_count = permits.len();
        let jobs = self
            .timeline_store
            .claim_due_jobs(kind, permit_count, |_| false)?;
        let count = jobs.len();
        for (permit, job) in permits.into_iter().zip(jobs) {
            self.spawn_adapter_job(job, permit);
        }
        Ok(json!({
            "scheduled": count,
            "availablePermits": self.lanes.async_jobs.available_permits(),
        }))
    }

    fn schedule_blocking_kind(&self, kind: JobKind, lane: &Arc<Semaphore>) -> Result<Value> {
        let permits = take_permits(lane, blocking_dispatch_batch_limit(kind));
        let permit_count = permits.len();
        let jobs = self
            .timeline_store
            .claim_due_jobs(kind, permit_count, |_| false)?;
        let count = jobs.len();
        for (permit, job) in permits.into_iter().zip(jobs) {
            self.spawn_blocking_job(job, permit, None);
        }
        Ok(json!({
            "scheduled": count,
            "availablePermits": lane.available_permits(),
        }))
    }

    fn schedule_agent_tasks(&self) -> Result<Value> {
        let permits = take_permits(&self.lanes.agent, agent_dispatch_batch_limit());
        let permit_count = permits.len();
        let mut blocked_sessions = self.lanes.active_agent_sessions();
        let jobs = self
            .timeline_store
            .claim_due_jobs(JobKind::AgentTask, permit_count, |job| {
                let key = agent_session_key(job);
                if blocked_sessions.contains(&key) {
                    true
                } else {
                    blocked_sessions.insert(key);
                    false
                }
            })?;
        let count = jobs.len();
        for (permit, job) in permits.into_iter().zip(jobs) {
            let key = agent_session_key(&job);
            self.lanes.mark_agent_session_active(&key);
            self.spawn_blocking_job(job, permit, Some(key));
        }
        Ok(json!({
            "scheduled": count,
            "availablePermits": self.lanes.agent.available_permits(),
            "activeSessions": self.lanes.active_agent_sessions().len(),
        }))
    }

    fn spawn_async_job(&self, job: Job, permit: OwnedSemaphorePermit) {
        let runtime = self.runtime.clone();
        let notify = self.notify.clone();
        tokio::spawn(async move {
            let job_id = job.id.clone();
            let kind = job.kind;
            let result = {
                let mut runtime = runtime.lock().await;
                runtime.dispatch_claimed_runtime_job(job)
            };
            if let Err(error) = result {
                log(&format!(
                    "async job worker failed {job_id} ({kind}): {}",
                    error_chain(&error)
                ));
            }
            drop(permit);
            notify.notify_one();
        });
    }

    fn spawn_adapter_job(&self, job: Job, permit: OwnedSemaphorePermit) {
        let runtime = self.runtime.clone();
        let adapter = self.adapter_jobs.clone();
        let notify = self.notify.clone();
        tokio::spawn(async move {
            let job_id = job.id.clone();
            let kind = job.kind;
            let result = adapter.execute_adapter_job(job.clone()).await;
            let runtime = runtime.lock().await;
            let update = match result {
                Ok(output) => runtime.complete_dispatched_job(&job_id, job, output),
                Err(error) => runtime.fail_dispatched_job(&job_id, job, error),
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

    fn spawn_blocking_job(
        &self,
        job: Job,
        permit: OwnedSemaphorePermit,
        agent_session_key: Option<String>,
    ) {
        let runtime = self.runtime.clone();
        let lanes = self.lanes.clone();
        let notify = self.notify.clone();
        tokio::spawn(async move {
            let job_id = job.id.clone();
            let kind = job.kind;
            let result = {
                let snapshot = {
                    let runtime = runtime.lock().await;
                    runtime.clone()
                };
                tokio::task::spawn_blocking(move || snapshot.dispatch_claimed_blocking_job(job))
                    .await
            };
            match result {
                Ok(Ok(_)) => {}
                Ok(Err(error)) => log(&format!(
                    "blocking job worker failed {job_id} ({kind}): {}",
                    error_chain(&error)
                )),
                Err(error) => log(&format!(
                    "blocking job worker panicked {job_id} ({kind}): {error}"
                )),
            }
            if let Some(key) = agent_session_key {
                lanes.mark_agent_session_inactive(&key);
            }
            drop(permit);
            notify.notify_one();
        });
    }
}

impl JobLanes {
    fn from_env() -> Self {
        Self {
            audio: Arc::new(Semaphore::new(env_usize(
                "CLANKCORD_AUDIO_JOB_CONCURRENCY",
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
            active_agent_sessions: StdMutex::new(BTreeSet::new()),
        }
    }

    fn active_agent_sessions(&self) -> BTreeSet<String> {
        self.active_agent_sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn mark_agent_session_active(&self, key: &str) {
        self.active_agent_sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(key.to_string());
    }

    fn mark_agent_session_inactive(&self, key: &str) {
        self.active_agent_sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(key);
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

fn blocking_dispatch_batch_limit(kind: JobKind) -> usize {
    match kind {
        JobKind::AudioSegment => env_usize("CLANKCORD_AUDIO_JOB_BATCH_LIMIT", 32, 128),
        JobKind::Response => env_usize("CLANKCORD_RESPONSE_JOB_BATCH_LIMIT", 12, 64),
        JobKind::RefineTranscript => env_usize("CLANKCORD_REFINEMENT_JOB_BATCH_LIMIT", 4, 32),
        _ => 1,
    }
}

fn async_dispatch_batch_limit() -> usize {
    env_usize("CLANKCORD_ASYNC_JOB_BATCH_LIMIT", 16, 128)
}

fn agent_dispatch_batch_limit() -> usize {
    env_usize("CLANKCORD_AGENT_JOB_BATCH_LIMIT", 4, 32)
}

fn env_usize(key: &str, default: usize, max: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(default)
        .clamp(1, max)
}

fn agent_session_key(job: &Job) -> String {
    AgentRuntime::task_session_key(&job.guild_id, &job.voice_channel_id)
}

fn error_chain(error: &anyhow::Error) -> String {
    error
        .chain()
        .map(|cause| cause.to_string())
        .collect::<Vec<_>>()
        .join(": ")
}
