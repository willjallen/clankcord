use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::Result;
use crate::adapters::discord::api::{create_dm_channel, discord_request};
use crate::runtime::jobs::{DiscordTypingIndicatorOutput, DiscordTypingIndicatorPayload};
use crate::runtime::util::string_field;
use crate::runtime::{TextTargetKind, log};

const TYPING_HEARTBEAT_SECONDS: u64 = 8;

#[derive(Default)]
pub(crate) struct DiscordTypingSupervisor {
    active: Mutex<BTreeMap<String, ActiveTyping>>,
}

struct ActiveTyping {
    cancelled: Arc<AtomicBool>,
    task: tokio::task::JoinHandle<()>,
}

impl DiscordTypingSupervisor {
    pub(crate) fn start(&self, key: String, channel_id: String) -> Result<()> {
        let mut active = self.active.lock().expect("typing supervisor lock poisoned");
        if active.contains_key(&key) {
            anyhow::bail!("Discord typing indicator is already active for {key}");
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        let task = spawn_typing_heartbeat(channel_id, cancelled.clone());
        active.insert(key, ActiveTyping { cancelled, task });
        Ok(())
    }

    pub(crate) fn stop(&self, key: &str) {
        let Some(active) = self
            .active
            .lock()
            .expect("typing supervisor lock poisoned")
            .remove(key)
        else {
            return;
        };
        active.cancelled.store(true, Ordering::Relaxed);
        active.task.abort();
    }
}

pub(crate) async fn execute(
    payload: DiscordTypingIndicatorPayload,
    supervisor: Arc<DiscordTypingSupervisor>,
) -> Result<DiscordTypingIndicatorOutput> {
    let key = typing_key(&payload);
    match payload.action {
        crate::runtime::DiscordTypingAction::Start => {
            let channel_id = concrete_channel_id(&payload).await?;
            post_typing(&channel_id).await?;
            supervisor.start(key, channel_id)?;
            Ok(DiscordTypingIndicatorOutput {
                action: payload.action,
                target: payload.target,
                source_job_id: payload.source_job_id,
                status: "started".to_string(),
            })
        }
        crate::runtime::DiscordTypingAction::Stop => {
            supervisor.stop(&key);
            Ok(DiscordTypingIndicatorOutput {
                action: payload.action,
                target: payload.target,
                source_job_id: payload.source_job_id,
                status: "stopped".to_string(),
            })
        }
    }
}

fn spawn_typing_heartbeat(
    channel_id: String,
    cancelled: Arc<AtomicBool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(TYPING_HEARTBEAT_SECONDS)).await;
            if cancelled.load(Ordering::Relaxed) {
                break;
            }
            let channel_id = channel_id.clone();
            match tokio::task::spawn_blocking(move || post_typing_blocking(&channel_id)).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => log(&format!("Discord typing heartbeat failed: {error}")),
                Err(error) => log(&format!("Discord typing heartbeat task failed: {error}")),
            }
        }
    })
}

async fn concrete_channel_id(payload: &DiscordTypingIndicatorPayload) -> Result<String> {
    let target = payload.target.clone();
    tokio::task::spawn_blocking(move || match target.kind {
        TextTargetKind::Channel => {
            let channel_id = target.channel_id.trim();
            if channel_id.is_empty() {
                anyhow::bail!("discord typing indicator has no target channel");
            }
            Ok(channel_id.to_string())
        }
        TextTargetKind::Dm => {
            let user_id = target.user_id.trim();
            if user_id.is_empty() {
                anyhow::bail!("discord typing indicator has no target DM user");
            }
            let channel = create_dm_channel(user_id)?;
            let channel_id = string_field(&channel, "id");
            if channel_id.is_empty() {
                anyhow::bail!("Discord did not return a DM channel id for {user_id}");
            }
            Ok(channel_id)
        }
        kind => anyhow::bail!(
            "discord typing indicator requires a concrete Discord target, got {}",
            kind.as_str()
        ),
    })
    .await?
}

async fn post_typing(channel_id: &str) -> Result<()> {
    let channel_id = channel_id.to_string();
    tokio::task::spawn_blocking(move || post_typing_blocking(&channel_id)).await?
}

fn post_typing_blocking(channel_id: &str) -> Result<()> {
    discord_request(
        "POST",
        &format!("/channels/{channel_id}/typing"),
        None,
        None,
        None,
        30,
    )?;
    Ok(())
}

fn typing_key(payload: &DiscordTypingIndicatorPayload) -> String {
    format!("{}:{}", payload.source_job_id, payload.agent_task_attempt)
}
