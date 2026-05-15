use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use serde_json::{Map, Value, json};
use tokio::sync::{mpsc, oneshot};

use crate::Result;
use crate::adapters::discord::voice::live::LiveVoiceAdapter;
use crate::config::{config_path, read_json};
use crate::runtime::core::execution::RuntimeExecutor;
use crate::runtime::timeline::TimelineStore;
use crate::runtime::{CommandRequest, Job, Runtime, RuntimeControlAction, log};

const DEFAULT_INTAKE_QUEUE_DEPTH: usize = 256;
const DEFAULT_MAINTAINER_INTERVAL_SECONDS: f64 = 0.5;

type ServiceRuntimeExecutor = RuntimeExecutor<Arc<LiveVoiceAdapter>>;

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
        let job = Job::runtime_control(
            target.guild_id,
            target.voice_channel_id,
            actor_user_id,
            action,
            target_job_id,
        );
        self.submit_job(job).await
    }

    pub async fn run_maintenance_once(&self) -> Result<Value> {
        run_maintainer_cycle(
            self.timeline_store.clone(),
            self.live_voice.clone(),
            self.executor.clone(),
        )
        .await
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

impl RuntimeService {
    pub async fn new() -> Result<Self> {
        let mut runtime = Runtime::new()?;
        let timeline_store = runtime.timeline_store.clone();
        timeline_store.initialize().await?;
        runtime.start().await?;
        let (intake, intake_receiver) = mpsc::channel(DEFAULT_INTAKE_QUEUE_DEPTH);
        let job_sink = RuntimeJobSink {
            intake: intake.clone(),
        };
        let live_voice = Arc::new(LiveVoiceAdapter::new(
            job_sink.clone(),
            timeline_store.clone(),
        ));
        let executor = RuntimeExecutor::new(live_voice.clone(), timeline_store.clone());
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

    pub fn spawn(self) {
        spawn_intake_loop(self.handle.clone(), self.intake);
        spawn_live_voice_loop(self.handle.live_voice.clone());
        spawn_dispatch_loop(self.handle.clone());
        spawn_maintainer_loop(self.handle.clone());
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

fn spawn_intake_loop(handle: RuntimeHandle, mut intake: mpsc::Receiver<RuntimeSubmission>) {
    tokio::spawn(async move {
        while let Some(submission) = intake.recv().await {
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
                        Ok(target) => Job::runtime_control(
                            target.guild_id,
                            target.voice_channel_id,
                            actor_user_id,
                            action,
                            target_job_id,
                        ),
                        Err(error) => {
                            let _ = reply.send(Err(error));
                            continue;
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
        log("runtime intake queue stopped");
    });
}

fn spawn_live_voice_loop(live_voice: Arc<LiveVoiceAdapter>) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(live_voice.flush_interval());
        loop {
            interval.tick().await;
            if let Err(error) = live_voice.start_missing_clients().await {
                log(&format!("voice client startup failed: {error}"));
            }
            if let Err(error) = live_voice.flush_ready_buffers().await {
                log(&format!("voice flush failed: {error}"));
            }
        }
    });
}

fn spawn_maintainer_loop(handle: RuntimeHandle) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(maintainer_interval());
        loop {
            interval.tick().await;
            if let Err(error) = handle.run_maintenance_once().await {
                log(&format!(
                    "runtime maintainer cycle failed: {}",
                    error_chain(&error)
                ));
            }
        }
    });
}

fn spawn_dispatch_loop(handle: RuntimeHandle) {
    tokio::spawn(async move {
        let notify = handle.executor.notify_handle();
        loop {
            notify.notified().await;
            if let Err(error) = handle.drain_ready_jobs().await {
                log(&format!(
                    "runtime dispatch drain failed: {}",
                    error_chain(&error)
                ));
            }
        }
    });
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

async fn run_maintainer_cycle(
    timeline_store: TimelineStore,
    live_voice: Arc<LiveVoiceAdapter>,
    executor: ServiceRuntimeExecutor,
) -> Result<Value> {
    sync_voice_adapter_state(timeline_store.clone(), live_voice)
        .await
        .context("syncing voice adapter state")?;
    let automation = {
        let mut runtime = Runtime::from_store(timeline_store.clone())?;
        runtime
            .run_automations()
            .await
            .context("running runtime automations")?
            .to_json()
    };
    let maintenance = executor
        .run_maintenance()
        .await
        .context("running runtime maintenance")?;
    executor.wake();
    Ok(json!({
        "ok": true,
        "automation": automation,
        "maintenance": maintenance,
        "dispatchNotified": true,
    }))
}

async fn sync_voice_adapter_state(
    timeline_store: TimelineStore,
    live_voice: Arc<LiveVoiceAdapter>,
) -> Result<()> {
    let bots = live_voice.bot_statuses().await;
    let sessions = live_voice.session_statuses().await;
    Runtime::from_store(timeline_store)?
        .sync_voice_adapter_status(bots, sessions)
        .await
}

fn maintainer_interval() -> Duration {
    let seconds = std::env::var("CLANKCORD_MAINTAINER_INTERVAL_SECONDS")
        .ok()
        .and_then(|value| value.parse::<f64>().ok())
        .unwrap_or(DEFAULT_MAINTAINER_INTERVAL_SECONDS)
        .max(DEFAULT_MAINTAINER_INTERVAL_SECONDS);
    Duration::from_millis((seconds * 1000.0).round() as u64)
}

pub async fn start_persistent_process() -> Result<()> {
    let service = RuntimeService::new().await?;
    let http_addr = http_addr()?;
    let handle = service.handle();
    service.spawn();
    crate::adapters::http::serve(handle, http_addr).await
}

pub fn start_blocking() -> i32 {
    match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => match runtime.block_on(start_persistent_process()) {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("{error}");
                1
            }
        },
        Err(error) => {
            eprintln!("{error}");
            1
        }
    }
}

fn http_addr() -> Result<SocketAddr> {
    let payload = read_json(&config_path(), json!({}));
    let api_config = payload
        .get("api")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let host = std::env::var("CLANKCORD_API_HOST")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| string_from_map(&api_config, "host", "0.0.0.0"));
    let port = std::env::var("CLANKCORD_API_PORT")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or_else(|| {
            string_from_map(&api_config, "port", "8091")
                .parse()
                .unwrap_or(8091)
        });
    Ok(format!("{host}:{port}").parse()?)
}

fn string_from_map(map: &Map<String, Value>, key: &str, fallback: &str) -> String {
    match map.get(key) {
        Some(Value::String(value)) if !value.trim().is_empty() => value.trim().to_string(),
        Some(Value::Number(value)) => value.to_string(),
        _ => fallback.to_string(),
    }
}
