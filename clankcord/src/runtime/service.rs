use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;

use crate::Result;
use crate::adapters::discord::gateway::text::DiscordTextAdapter;
use crate::adapters::discord::runtime_api::DiscordRuntimeApi;
use crate::adapters::discord::voice::live::LiveVoiceAdapter;
use crate::config;
use crate::runtime::core::execution::RuntimeExecutor;
use crate::runtime::timeline::{TimelineStore, utc_now};
use crate::runtime::{CommandRequest, Job, Runtime, RuntimeControlAction, log};

type ServiceRuntimeExecutor = RuntimeExecutor<DiscordRuntimeApi>;
const DISPATCH_DUE_BACKLOG_RETRY_MS: u64 = 25;
const SERVICE_SHUTDOWN_TASK_TIMEOUT: Duration = Duration::from_secs(5);
const SERVICE_SHUTDOWN_VOICE_IDLE_TIMEOUT: Duration = Duration::from_secs(10);
const SERVICE_SHUTDOWN_WORKER_IDLE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone)]
pub struct RuntimeHandle {
    live_voice: Arc<LiveVoiceAdapter>,
    timeline_store: TimelineStore,
    executor: ServiceRuntimeExecutor,
    intake: mpsc::Sender<RuntimeSubmission>,
    job_sink: RuntimeJobSink,
}

impl RuntimeHandle {
    pub(crate) fn runtime_context(&self) -> Result<Runtime> {
        Runtime::from_store(self.timeline_store.clone())
    }

    pub async fn submit_command(&self, command: CommandRequest) -> Result<Value> {
        submit_to_intake(&self.intake, |reply| RuntimeSubmission::Command {
            command,
            reply,
        })
        .await
    }

    pub async fn submit_job(&self, job: Job) -> Result<Value> {
        self.job_sink.submit(job).await
    }

    pub fn job_sink(&self) -> RuntimeJobSink {
        self.job_sink.clone()
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
        let target = self.timeline_store.get_job(&target_job_id).await?;
        let job = Job::runtime_control(target.scope(), actor_user_id, action, target_job_id);
        self.submit_job(job).await
    }

    pub async fn drain_ready_jobs(&self) -> Result<Value> {
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

#[derive(Clone)]
pub struct RuntimeJobSink {
    intake: mpsc::Sender<RuntimeSubmission>,
}

impl RuntimeJobSink {
    pub async fn submit(&self, job: Job) -> Result<Value> {
        submit_to_intake(&self.intake, |reply| RuntimeSubmission::Job { job, reply }).await
    }

    pub async fn submit_runtime_control_for_target(
        &self,
        target_job_id: &str,
        action: RuntimeControlAction,
        actor_user_id: String,
    ) -> Result<Value> {
        submit_to_intake(&self.intake, |reply| {
            RuntimeSubmission::RuntimeControlTarget {
                target_job_id: target_job_id.to_string(),
                action,
                actor_user_id,
                reply,
            }
        })
        .await
    }

    pub fn submit_detached(&self, job: Job) {
        let sink = self.clone();
        tokio::spawn(async move {
            let job_id = job.id.clone();
            if let Err(error) = sink.submit(job).await {
                log(&format!("detached job submission failed {job_id}: {error}"));
            }
        });
    }
}

pub struct RuntimeService {
    handle: RuntimeHandle,
    intake: mpsc::Receiver<RuntimeSubmission>,
}

pub struct RuntimeServiceRunner {
    handle: RuntimeHandle,
    shutdown: watch::Sender<bool>,
    intake_task: JoinHandle<()>,
    discord_text_task: JoinHandle<()>,
    live_voice_task: JoinHandle<()>,
    dispatch_task: JoinHandle<()>,
}

impl RuntimeService {
    pub async fn new() -> Result<Self> {
        let mut runtime = Runtime::new().context("constructing runtime")?;
        let timeline_store = runtime.timeline_store.clone();
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
        runtime.start().await.context("starting runtime domain")?;
        match runtime.recover_interrupted_agent_tasks().await {
            Ok(recovered) if !recovered.is_empty() => {
                log(&format!(
                    "recovered {} interrupted agent task(s)",
                    recovered.len()
                ));
            }
            Ok(_) => {}
            Err(error) => log(&format!("agent task recovery failed: {error}")),
        }
        let (intake, intake_receiver) = mpsc::channel(config::intake_queue_depth());
        let job_sink = RuntimeJobSink {
            intake: intake.clone(),
        };
        let live_voice = Arc::new(LiveVoiceAdapter::new(
            job_sink.clone(),
            timeline_store.clone(),
        ));
        let executor = RuntimeExecutor::new(
            DiscordRuntimeApi::new(live_voice.clone()),
            timeline_store.clone(),
        );
        timeline_store
            .replace_runtime_maintenance_job(Job::runtime_maintenance(
                config::runtime_maintenance_interval_ms(),
            ))
            .await
            .context("replacing runtime maintenance job")?;
        Ok(Self {
            handle: RuntimeHandle {
                live_voice,
                timeline_store,
                executor,
                intake,
                job_sink,
            },
            intake: intake_receiver,
        })
    }

    pub fn handle(&self) -> RuntimeHandle {
        self.handle.clone()
    }

    pub fn spawn(self) -> RuntimeServiceRunner {
        let (shutdown, _) = watch::channel(false);
        let intake_task = spawn_intake_loop(self.handle.clone(), self.intake, shutdown.subscribe());
        let discord_text_task =
            spawn_discord_text_loop(self.handle.job_sink(), shutdown.subscribe());
        let live_voice_task =
            spawn_live_voice_loop(self.handle.live_voice.clone(), shutdown.subscribe());
        let dispatch_task = spawn_dispatch_loop(self.handle.clone(), shutdown.subscribe());
        RuntimeServiceRunner {
            handle: self.handle,
            shutdown,
            intake_task,
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
        let intake =
            join_service_task("intake", self.intake_task, SERVICE_SHUTDOWN_TASK_TIMEOUT).await;
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
            "tasks": [intake, discord_text, live_voice_loop, dispatch],
        });
        log(&format!("runtime shutdown complete: {report}"));
        Ok(report)
    }
}

enum RuntimeSubmission {
    Command {
        command: CommandRequest,
        reply: oneshot::Sender<Result<Value>>,
    },
    Job {
        job: Job,
        reply: oneshot::Sender<Result<Value>>,
    },
    RuntimeControlTarget {
        target_job_id: String,
        action: RuntimeControlAction,
        actor_user_id: String,
        reply: oneshot::Sender<Result<Value>>,
    },
}

async fn submit_to_intake(
    intake: &mpsc::Sender<RuntimeSubmission>,
    submission: impl FnOnce(oneshot::Sender<Result<Value>>) -> RuntimeSubmission,
) -> Result<Value> {
    let (reply, result) = oneshot::channel();
    intake
        .send(submission(reply))
        .await
        .map_err(|_| anyhow::anyhow!("runtime intake queue is closed"))?;
    result
        .await
        .map_err(|_| anyhow::anyhow!("runtime intake loop stopped"))?
}

fn spawn_intake_loop(
    handle: RuntimeHandle,
    mut intake: mpsc::Receiver<RuntimeSubmission>,
    mut shutdown: watch::Receiver<bool>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = wait_for_shutdown(&mut shutdown) => {
                    intake.close();
                    while let Some(submission) = intake.recv().await {
                        handle_runtime_submission(&handle, submission).await;
                    }
                    break;
                }
                submission = intake.recv() => {
                    let Some(submission) = submission else {
                        break;
                    };
                    handle_runtime_submission(&handle, submission).await;
                }
            }
        }
        log("runtime intake queue stopped");
    })
}

async fn handle_runtime_submission(handle: &RuntimeHandle, submission: RuntimeSubmission) {
    match submission {
        RuntimeSubmission::Command { command, reply } => {
            let result = match handle.runtime_context() {
                Ok(mut runtime) => runtime.create_command_job(command, None).await,
                Err(error) => Err(error),
            };
            if result.is_ok() {
                handle.executor.wake();
            }
            let _ = reply.send(result);
        }
        RuntimeSubmission::Job { job, reply } => {
            let result = if job.kind == crate::runtime::JobKind::WakeProbe {
                handle.timeline_store.create_wake_probe_job(job).await
            } else {
                handle.timeline_store.create_job(job).await
            }
            .map(job_created_payload);
            if result.is_ok() {
                handle.executor.wake();
            }
            let _ = reply.send(result);
        }
        RuntimeSubmission::RuntimeControlTarget {
            target_job_id,
            action,
            actor_user_id,
            reply,
        } => {
            let result = match handle.timeline_store.get_job(&target_job_id).await {
                Ok(target) => {
                    Job::runtime_control(target.scope(), actor_user_id, action, target_job_id)
                }
                Err(error) => {
                    let _ = reply.send(Err(error));
                    return;
                }
            };
            let result = handle
                .timeline_store
                .create_job(result)
                .await
                .map(job_created_payload);
            if result.is_ok() {
                handle.executor.wake();
            }
            let _ = reply.send(result);
        }
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

fn spawn_discord_text_loop(
    job_sink: RuntimeJobSink,
    shutdown: watch::Receiver<bool>,
) -> JoinHandle<()> {
    DiscordTextAdapter::new(job_sink).spawn(shutdown)
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
                    if report
                        .get("exhausted")
                        .and_then(Value::as_bool)
                        .is_some_and(|exhausted| !exhausted)
                    {
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
            let next_ready_at = match handle.executor.next_queued_job_ready_at().await {
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
            let next_wake_at = match next_ready_at {
                Some(ready_at) if ready_at <= now => {
                    Some(now + chrono::Duration::milliseconds(DISPATCH_DUE_BACKLOG_RETRY_MS as i64))
                }
                value => value,
            };
            match next_wake_at {
                Some(ready_at) => {
                    let sleep_ms = (ready_at - now).num_milliseconds().max(0) as u64;
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

fn job_created_payload(job: Job) -> Value {
    json!({"kind": "job_created", "job_ids": [job.id.clone()], "job": job.to_value()})
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
    let serve_result =
        crate::adapters::http::serve_until_shutdown(handle, http_addr, http_shutdown)
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
