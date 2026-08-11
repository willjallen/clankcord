use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use serde_json::{Value, json};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::Result;
use crate::adapters::discord::gateway::client::DiscordTextAdapter;
use crate::adapters::discord::runtime_api::DiscordRuntimeApi;
use crate::adapters::discord::voice::live::LiveVoiceAdapter;
use crate::config;
use crate::domain::Ctx;
use crate::domain::interactions::commands;
use crate::domain::interactions::tasks;
use crate::engine::DrainReport;
use crate::engine::JobBus;
use crate::engine::RuntimeExecutor;
use crate::engine::schedules;
use crate::model::job::{CommandRequest, Job, RuntimeControlAction};
use crate::store::{TimelineStore, utc_now};
use crate::util::log;

type ServiceRuntimeExecutor = RuntimeExecutor<DiscordRuntimeApi>;
/// A job can be due but unclaimable while its ordering key is held by a
/// running job. The holder's completion notify re-drains immediately; this
/// poll interval is only the fallback that keeps the loop from spinning hot
/// on a blocked backlog.
const DISPATCH_BLOCKED_BACKLOG_POLL_MS: u64 = 25;
const SERVICE_SHUTDOWN_TASK_TIMEOUT: Duration = Duration::from_secs(5);
const SERVICE_SHUTDOWN_VOICE_IDLE_TIMEOUT: Duration = Duration::from_secs(10);
const SERVICE_SHUTDOWN_WORKER_IDLE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone)]
pub struct RuntimeHandle {
    live_voice: Arc<LiveVoiceAdapter>,
    timeline_store: TimelineStore,
    executor: ServiceRuntimeExecutor,
    bus: JobBus,
}

impl RuntimeHandle {
    pub(crate) fn runtime_context(&self) -> Ctx {
        Ctx::new(self.timeline_store.clone())
    }

    pub fn bus(&self) -> JobBus {
        self.bus.clone()
    }

    pub async fn submit_command(&self, command: CommandRequest) -> Result<Value> {
        let runtime = self.runtime_context();
        commands::create_command_job(&runtime, command, None).await
    }

    pub async fn submit_job(&self, job: Job) -> Result<Value> {
        self.bus.submit(job).await
    }

    pub async fn retry_job(&self, job_id: String) -> Result<Value> {
        self.submit_runtime_control(job_id, RuntimeControlAction::RetryJob, String::new())
            .await
    }

    pub async fn approve_confirmation(
        &self,
        job_id: String,
        approved_by_user_id: String,
    ) -> Result<Value> {
        self.submit_runtime_control(
            job_id,
            RuntimeControlAction::ApproveConfirmation,
            approved_by_user_id,
        )
        .await
    }

    pub async fn cancel_confirmation(
        &self,
        job_id: String,
        cancelled_by_user_id: String,
    ) -> Result<Value> {
        self.submit_runtime_control(
            job_id,
            RuntimeControlAction::CancelConfirmation,
            cancelled_by_user_id,
        )
        .await
    }

    async fn submit_runtime_control(
        &self,
        target_job_id: String,
        action: RuntimeControlAction,
        actor_user_id: String,
    ) -> Result<Value> {
        self.bus
            .submit_runtime_control_for_target(&target_job_id, action, actor_user_id)
            .await
    }

    pub async fn drain_ready_jobs(&self) -> Result<DrainReport> {
        self.executor.drain_ready_jobs().await
    }

    pub async fn room_occupants(&self, guild_id: &str, channel_id: &str) -> Result<Vec<Value>> {
        self.timeline_store
            .room_occupants(guild_id, channel_id)
            .await
    }

    pub async fn voice_occupancy_snapshot(&self) -> Result<Value> {
        self.timeline_store.voice_occupancy_snapshot().await
    }
}

pub struct RuntimeService {
    handle: RuntimeHandle,
}

pub struct RuntimeServiceRunner {
    handle: RuntimeHandle,
    shutdown: watch::Sender<bool>,
    discord_text_task: JoinHandle<()>,
    live_voice_task: JoinHandle<()>,
    dispatch_task: JoinHandle<()>,
}

impl RuntimeService {
    pub async fn new() -> Result<Self> {
        let timeline_store = TimelineStore::new(None).context("constructing timeline store")?;
        timeline_store
            .initialize()
            .await
            .context("initializing timeline store")?;
        timeline_store
            .write_runtime_config_snapshot(
                &config::runtime_pool_config(),
                &config::control_config(),
                &config::guild_configs(),
                &config::room_configs(),
            )
            .await
            .context("writing runtime config snapshot")?;
        let runtime = Ctx::new(timeline_store.clone());
        match tasks::recover_interrupted_agent_tasks(&runtime).await {
            Ok(recovered) if !recovered.is_empty() => {
                log(&format!(
                    "recovered {} interrupted agent task(s)",
                    recovered.len()
                ));
            }
            Ok(_) => {}
            Err(error) => log(&format!("agent task recovery failed: {error}")),
        }
        let bus = JobBus::new(timeline_store.clone());
        let live_voice = Arc::new(LiveVoiceAdapter::new(bus.clone(), timeline_store.clone()));
        let executor = RuntimeExecutor::new(
            DiscordRuntimeApi::new(live_voice.clone()),
            timeline_store.clone(),
        );
        schedules::ensure_default_schedules(&timeline_store)
            .await
            .context("declaring default job schedules")?;
        Ok(Self {
            handle: RuntimeHandle {
                live_voice,
                timeline_store,
                executor,
                bus,
            },
        })
    }

    pub fn handle(&self) -> RuntimeHandle {
        self.handle.clone()
    }

    pub fn spawn(self) -> RuntimeServiceRunner {
        let (shutdown, _) = watch::channel(false);
        let discord_text_task = spawn_discord_text_loop(self.handle.bus(), shutdown.subscribe());
        let live_voice_task =
            spawn_live_voice_loop(self.handle.live_voice.clone(), shutdown.subscribe());
        let dispatch_task = spawn_dispatch_loop(self.handle.clone(), shutdown.subscribe());
        RuntimeServiceRunner {
            handle: self.handle,
            shutdown,
            discord_text_task,
            live_voice_task,
            dispatch_task,
        }
    }
}

impl RuntimeServiceRunner {
    fn shutdown_receiver(&self) -> watch::Receiver<bool> {
        self.shutdown.subscribe()
    }

    fn shutdown_sender(&self) -> watch::Sender<bool> {
        self.shutdown.clone()
    }

    pub fn request_shutdown(&self, reason: &str) {
        if !*self.shutdown.borrow() {
            log(&format!("runtime shutdown requested: {reason}"));
            let _ = self.shutdown.send(true);
        }
        self.handle.executor.wake();
    }

    pub async fn shutdown(self) -> Result<Value> {
        self.request_shutdown("service shutdown");
        let voice_idle = self
            .handle
            .executor
            .wait_for_voice_idle(SERVICE_SHUTDOWN_VOICE_IDLE_TIMEOUT)
            .await;
        let live_voice = self
            .handle
            .live_voice
            .shutdown_gracefully()
            .await
            .context("shutting down live voice adapter")?;
        let worker_idle = self
            .handle
            .executor
            .wait_for_idle(SERVICE_SHUTDOWN_WORKER_IDLE_TIMEOUT)
            .await;
        let discord_text = join_service_task(
            "discord_text",
            self.discord_text_task,
            SERVICE_SHUTDOWN_TASK_TIMEOUT,
        )
        .await;
        let live_voice_loop = join_service_task(
            "live_voice",
            self.live_voice_task,
            SERVICE_SHUTDOWN_TASK_TIMEOUT,
        )
        .await;
        let dispatch = join_service_task(
            "dispatch",
            self.dispatch_task,
            SERVICE_SHUTDOWN_TASK_TIMEOUT,
        )
        .await;
        let report = json!({
            "kind": "runtime_shutdown",
            "voiceIdle": voice_idle,
            "liveVoice": live_voice,
            "workerIdle": worker_idle,
            "tasks": [discord_text, live_voice_loop, dispatch],
        });
        log(&format!("runtime shutdown complete: {report}"));
        Ok(report)
    }
}

fn spawn_live_voice_loop(
    live_voice: Arc<LiveVoiceAdapter>,
    mut shutdown: watch::Receiver<bool>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(live_voice.flush_interval());
        loop {
            tokio::select! {
                _ = wait_for_shutdown(&mut shutdown) => break,
                _ = interval.tick() => {}
            }
            if let Err(error) = live_voice.start_missing_clients().await {
                log(&format!("voice client startup failed: {error}"));
            }
            if let Err(error) = live_voice.flush_ready_buffers().await {
                log(&format!("voice flush failed: {error}"));
            }
        }
        log("live voice loop stopped");
    })
}

fn spawn_discord_text_loop(bus: JobBus, shutdown: watch::Receiver<bool>) -> JoinHandle<()> {
    DiscordTextAdapter::new(bus).spawn(shutdown)
}

fn spawn_dispatch_loop(
    handle: RuntimeHandle,
    mut shutdown: watch::Receiver<bool>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let notify = handle.executor.notify_handle();
        loop {
            if shutdown_requested(&shutdown) {
                break;
            }
            match handle.drain_ready_jobs().await {
                Ok(report) => {
                    if !report.exhausted {
                        continue;
                    }
                }
                Err(error) => {
                    log(&format!(
                        "runtime dispatch drain failed: {}",
                        error_chain(&error)
                    ));
                }
            }
            let next_ready_at = match next_wake_instant(&handle).await {
                Ok(value) => value,
                Err(error) => {
                    log(&format!(
                        "runtime next-ready lookup failed: {}",
                        error_chain(&error)
                    ));
                    tokio::select! {
                        _ = wait_for_shutdown(&mut shutdown) => break,
                        _ = notify.notified() => {}
                    }
                    continue;
                }
            };
            let now = utc_now();
            match next_ready_at {
                Some(ready_at) => {
                    let sleep_ms = (ready_at - now)
                        .num_milliseconds()
                        .max(DISPATCH_BLOCKED_BACKLOG_POLL_MS as i64)
                        as u64;
                    let sleep = tokio::time::sleep(Duration::from_millis(sleep_ms));
                    tokio::pin!(sleep);
                    tokio::select! {
                        _ = wait_for_shutdown(&mut shutdown) => break,
                        _ = notify.notified() => {}
                        _ = &mut sleep => {}
                    }
                }
                None => {
                    tokio::select! {
                        _ = wait_for_shutdown(&mut shutdown) => break,
                        _ = notify.notified() => {}
                    }
                }
            }
        }
        log("runtime dispatch loop stopped");
    })
}

/// The dispatch loop parks until the earlier of the next queued job's ready
/// time and the next enabled schedule's due time.
async fn next_wake_instant(
    handle: &RuntimeHandle,
) -> Result<Option<chrono::DateTime<chrono::Utc>>> {
    let job_ready = handle.executor.next_queued_job_ready_at().await?;
    let schedule_due = handle
        .timeline_store
        .next_due_job_schedule_at_ms()
        .await?
        .and_then(crate::store::ms_to_datetime);
    Ok(match (job_ready, schedule_due) {
        (Some(job), Some(schedule)) => Some(job.min(schedule)),
        (value, None) | (None, value) => value,
    })
}

fn shutdown_requested(shutdown: &watch::Receiver<bool>) -> bool {
    *shutdown.borrow()
}

async fn wait_for_shutdown(shutdown: &mut watch::Receiver<bool>) {
    if shutdown_requested(shutdown) {
        return;
    }
    while shutdown.changed().await.is_ok() {
        if shutdown_requested(shutdown) {
            return;
        }
    }
}

async fn join_service_task(name: &str, mut task: JoinHandle<()>, timeout: Duration) -> Value {
    let started = std::time::Instant::now();
    match tokio::time::timeout(timeout, &mut task).await {
        Ok(Ok(())) => json!({
            "name": name,
            "status": "stopped",
            "elapsedMs": elapsed_ms(started.elapsed()),
        }),
        Ok(Err(error)) => json!({
            "name": name,
            "status": "join_error",
            "elapsedMs": elapsed_ms(started.elapsed()),
            "error": error.to_string(),
        }),
        Err(_) => {
            task.abort();
            let _ = task.await;
            json!({
                "name": name,
                "status": "aborted",
                "elapsedMs": elapsed_ms(started.elapsed()),
            })
        }
    }
}

fn elapsed_ms(duration: Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
}

fn spawn_process_signal_listener(shutdown: watch::Sender<bool>) -> JoinHandle<()> {
    tokio::spawn(async move {
        match wait_for_process_shutdown_signal().await {
            Ok(signal) => {
                log(&format!("process shutdown signal received: {signal}"));
                let _ = shutdown.send(true);
            }
            Err(error) => {
                log(&format!("process shutdown signal listener failed: {error}"));
                let _ = shutdown.send(true);
            }
        }
    })
}

async fn wait_for_process_shutdown_signal() -> Result<&'static str> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .context("installing SIGTERM handler")?;
        let mut interrupt =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
                .context("installing SIGINT handler")?;
        tokio::select! {
            _ = terminate.recv() => Ok("SIGTERM"),
            _ = interrupt.recv() => Ok("SIGINT"),
        }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c()
            .await
            .context("installing Ctrl-C handler")?;
        Ok("CTRL_C")
    }
}

fn error_chain(error: &anyhow::Error) -> String {
    error
        .chain()
        .map(|cause| cause.to_string())
        .collect::<Vec<_>>()
        .join(": ")
}

pub async fn start_persistent_process() -> Result<()> {
    let service = RuntimeService::new()
        .await
        .context("creating runtime service")?;
    let http_addr = config::http_addr().context("resolving HTTP bind address")?;
    let handle = service.handle();
    let runner = service.spawn();
    let signal_task = spawn_process_signal_listener(runner.shutdown_sender());
    let http_shutdown = wait_for_shutdown_request(runner.shutdown_receiver());
    let serve_result = crate::app::http::serve_until_shutdown(handle, http_addr, http_shutdown)
        .await
        .context("serving HTTP API");
    runner.request_shutdown("HTTP server stopped");
    let shutdown_result = runner.shutdown().await.context("stopping runtime service");
    signal_task.abort();
    let _ = signal_task.await;
    serve_result?;
    shutdown_result?;
    Ok(())
}

async fn wait_for_shutdown_request(mut shutdown: watch::Receiver<bool>) {
    wait_for_shutdown(&mut shutdown).await;
}

pub fn start_blocking() -> i32 {
    match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => {
            let result = runtime.block_on(start_persistent_process());
            runtime.shutdown_timeout(SERVICE_SHUTDOWN_TASK_TIMEOUT);
            match result {
                Ok(()) => 0,
                Err(error) => {
                    eprintln!("{}", error_chain(&error));
                    1
                }
            }
        }
        Err(error) => {
            eprintln!("{error}");
            1
        }
    }
}
