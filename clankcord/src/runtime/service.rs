use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use serde_json::{Map, Value, json};
use tokio::sync::{Mutex, mpsc, oneshot};

use crate::Result;
use crate::adapters::discord::voice::live::LiveVoiceAdapter;
use crate::config::{config_path, read_json};
use crate::runtime::{CommandRequest, Job, Runtime, RuntimeControlAction, log};

const DEFAULT_INTAKE_QUEUE_DEPTH: usize = 256;
const DEFAULT_MAINTAINER_INTERVAL_SECONDS: f64 = 0.5;

#[derive(Clone)]
pub struct RuntimeHandle {
    runtime: Arc<Mutex<Runtime>>,
    live_voice: Arc<LiveVoiceAdapter>,
    intake: mpsc::Sender<RuntimeSubmission>,
    job_sink: RuntimeJobSink,
}

impl RuntimeHandle {
    pub(crate) fn runtime(&self) -> Arc<Mutex<Runtime>> {
        self.runtime.clone()
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
        let job = {
            let runtime = self.runtime.lock().await;
            runtime.runtime_control_job_for_target(&target_job_id, action, actor_user_id)?
        };
        self.submit_job(job).await
    }

    pub async fn run_maintenance_once(&self) -> Result<Value> {
        run_maintainer_cycle(self.runtime.clone(), self.live_voice.clone()).await
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
        runtime.start().await?;
        let runtime = Arc::new(Mutex::new(runtime));
        let (intake, intake_receiver) = mpsc::channel(DEFAULT_INTAKE_QUEUE_DEPTH);
        let job_sink = RuntimeJobSink {
            intake: intake.clone(),
        };
        let live_voice = Arc::new(LiveVoiceAdapter::new(job_sink.clone()));
        Ok(Self {
            handle: RuntimeHandle {
                runtime,
                live_voice,
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
                    let result = {
                        let mut runtime = handle.runtime.lock().await;
                        runtime.create_command_job(command, None).await
                    };
                    let _ = reply.send(result);
                }
                RuntimeSubmission::Job { job, reply } => {
                    let result = {
                        let runtime = handle.runtime.lock().await;
                        runtime.intake_job(job)
                    };
                    let _ = reply.send(result);
                }
                RuntimeSubmission::RuntimeControlTarget {
                    target_job_id,
                    action,
                    actor_user_id,
                    reply,
                } => {
                    let result = {
                        let runtime = handle.runtime.lock().await;
                        runtime
                            .runtime_control_job_for_target(&target_job_id, action, actor_user_id)
                            .and_then(|job| runtime.intake_job(job))
                    };
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

fn error_chain(error: &anyhow::Error) -> String {
    error
        .chain()
        .map(|cause| cause.to_string())
        .collect::<Vec<_>>()
        .join(": ")
}

async fn run_maintainer_cycle(
    runtime: Arc<Mutex<Runtime>>,
    live_voice: Arc<LiveVoiceAdapter>,
) -> Result<Value> {
    sync_voice_adapter_state(runtime.clone(), live_voice.clone())
        .await
        .context("syncing voice adapter state")?;
    let automation = {
        let mut runtime = runtime.lock().await;
        runtime
            .run_automations()
            .context("running runtime automations")?
            .to_json()
    };
    let async_jobs = {
        let mut runtime = runtime.lock().await;
        runtime
            .dispatch_due_jobs(Some(&live_voice))
            .await
            .context("dispatching due async jobs")?
    };
    let snapshot = {
        let runtime = runtime.lock().await;
        runtime.clone()
    };
    let blocking = tokio::task::spawn_blocking(move || {
        let jobs = snapshot
            .dispatch_due_blocking_jobs()
            .context("dispatching due blocking jobs")?;
        let maintenance = snapshot
            .run_blocking_maintenance()
            .context("running blocking maintenance")?;
        Ok::<Value, anyhow::Error>(json!({
            "jobs": jobs,
            "maintenance": maintenance,
        }))
    })
    .await
    .context("joining blocking maintainer task")??;
    let mut payload = Map::new();
    payload.insert("ok".to_string(), Value::Bool(true));
    payload.insert("automation".to_string(), automation);
    payload.insert("jobs".to_string(), async_jobs);
    payload.insert("blocking".to_string(), blocking);
    Ok(Value::Object(payload))
}

async fn sync_voice_adapter_state(
    runtime: Arc<Mutex<Runtime>>,
    live_voice: Arc<LiveVoiceAdapter>,
) -> Result<()> {
    let bots = live_voice.bot_statuses().await;
    let sessions = live_voice.session_statuses().await;
    let mut runtime = runtime.lock().await;
    runtime.sync_voice_adapter_status(bots, sessions)
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
