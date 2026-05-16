use serde_json::{Value, json};

use crate::Result;
use crate::runtime::core::execution::JobDecision;
use crate::runtime::util::{first_non_empty, single_child_of_kind, string_field};
use crate::runtime::{
    BinaryPayload, DiscordTextSendPayload, Job, JobKind, JobOutput, JobState, Runtime,
    TextDeliveryOutput, TextDeliveryPayload, TextTarget, TextTargetKind,
};

impl Runtime {
    pub(crate) async fn text_delivery_job_from_value(&self, value: &Value) -> Result<Job> {
        let mut payload = TextDeliveryPayload::from_json(value)?;
        let source = if payload.source_job_id.trim().is_empty() {
            None
        } else {
            Some(self.timeline_store.get_job(&payload.source_job_id).await?)
        };
        let guild_id = first_non_empty([
            string_field(value, "guild_id"),
            source
                .as_ref()
                .map(|job| job.guild_id.clone())
                .unwrap_or_default(),
        ]);
        let voice_channel_id = first_non_empty([
            string_field(value, "voice_channel_id"),
            source
                .as_ref()
                .map(|job| job.voice_channel_id.clone())
                .unwrap_or_default(),
        ]);
        if guild_id.trim().is_empty() || voice_channel_id.trim().is_empty() {
            anyhow::bail!("text delivery is missing guild/channel scope");
        }
        if payload.requested_by_user_id.trim().is_empty() {
            payload.requested_by_user_id = source
                .as_ref()
                .map(|job| job.requested_by_user_id.clone())
                .unwrap_or_default();
        }
        Ok(Job::text_delivery(
            guild_id,
            voice_channel_id,
            payload.requested_by_user_id.clone(),
            payload,
        ))
    }

    pub(crate) async fn prepare_text_delivery_job(
        &mut self,
        job: &Job,
        payload: &TextDeliveryPayload,
    ) -> Result<JobDecision> {
        if payload.content.trim().is_empty() {
            return Ok(JobDecision::fail(format!(
                "text delivery job {} has empty content",
                job.id
            )));
        }
        let children = self.timeline_store.list_child_jobs(&job.id).await?;
        if children.iter().any(|child| !child.state.is_terminal()) {
            return Ok(JobDecision::Wait);
        }
        if let Some(failed) = children
            .iter()
            .find(|child| child.state != JobState::Complete)
        {
            return Ok(JobDecision::fail(format!(
                "text delivery dependency {} ended as {}: {}",
                failed.id, failed.state, failed.metadata.error
            )));
        }
        if !children.is_empty() {
            return self
                .complete_text_delivery_from_child(job, payload, &children)
                .await;
        }

        let target = self.resolve_text_delivery_target(job, payload).await?;
        let child = Job::discord_text_send(
            job.guild_id.clone(),
            job.voice_channel_id.clone(),
            job.requested_by_user_id.clone(),
            DiscordTextSendPayload {
                intent: payload.intent,
                target,
                content: payload.content.clone(),
                source_job_id: payload.source_job_id.clone(),
                requested_by_user_id: payload.requested_by_user_id.clone(),
                allowed_mentions: BinaryPayload::empty(),
                components: BinaryPayload::empty(),
            },
        );
        Ok(JobDecision::WaitFor(vec![child]))
    }

    async fn complete_text_delivery_from_child(
        &self,
        job: &Job,
        payload: &TextDeliveryPayload,
        children: &[Job],
    ) -> Result<JobDecision> {
        let send_child = single_child_of_kind(children, JobKind::DiscordTextSend)?;
        let Some(JobOutput::DiscordTextSend(output)) = send_child.metadata.output.clone() else {
            return Ok(JobDecision::fail(format!(
                "text delivery child {} completed without discord text output",
                send_child.id
            )));
        };
        self.timeline_store
            .append_event(
                &job.guild_id,
                &job.voice_channel_id,
                json!({
                    "event_kind": "text_delivered",
                    "kind": "text_delivered",
                    "job_id": job.id,
                    "source_job_id": payload.source_job_id,
                    "intent": payload.intent.as_str(),
                    "target": output.target.to_json(),
                    "discord_post": output.discord_post.to_json(),
                }),
            )
            .await?;
        Ok(JobDecision::Complete(JobOutput::TextDelivery(
            TextDeliveryOutput {
                intent: payload.intent.as_str().to_string(),
                target: output.target,
                source_job_id: payload.source_job_id.clone(),
                discord_post: Some(output.discord_post),
            },
        )))
    }

    async fn resolve_text_delivery_target(
        &self,
        job: &Job,
        payload: &TextDeliveryPayload,
    ) -> Result<TextTarget> {
        match payload.target.kind {
            TextTargetKind::Channel => {
                require_target_id(&payload.target.channel_id, "channel", job)?;
                Ok(payload.target.clone())
            }
            TextTargetKind::Dm => {
                require_target_id(&payload.target.user_id, "dm", job)?;
                Ok(payload.target.clone())
            }
            TextTargetKind::AgentChat => {
                let control = self.timeline_store.control_config().await?;
                let channel_id = control.bots_channel_id.trim();
                if channel_id.is_empty() {
                    anyhow::bail!("botsChannelId is not configured");
                }
                Ok(TextTarget {
                    kind: TextTargetKind::Channel,
                    channel_id: channel_id.to_string(),
                    user_id: String::new(),
                })
            }
            TextTargetKind::AgentSession => {
                let source_job_id = payload.source_job_id.trim();
                if source_job_id.is_empty() {
                    anyhow::bail!(
                        "text delivery job {} uses session target without source job",
                        job.id
                    );
                }
                let source = self.timeline_store.get_job(source_job_id).await?;
                let crate::runtime::JobPayload::AgentTask(agent_task) = &source.payload else {
                    anyhow::bail!(
                        "text delivery job {} uses session target but source job {} is not an agent task",
                        job.id,
                        source_job_id
                    );
                };
                let session = self
                    .timeline_store
                    .get_agent_session_record(&agent_task.agent_session_id)
                    .await?;
                match session.text_target.kind {
                    TextTargetKind::Channel | TextTargetKind::Dm => Ok(session.text_target),
                    kind => anyhow::bail!(
                        "agent session {} has unsupported text target {}",
                        session.agent_session_id,
                        kind.as_str()
                    ),
                }
            }
        }
    }
}

fn require_target_id(value: &str, label: &str, job: &Job) -> Result<()> {
    if value.trim().is_empty() {
        anyhow::bail!("text delivery job {} has no {label} target id", job.id);
    }
    Ok(())
}
