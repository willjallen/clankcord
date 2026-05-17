use serde_json::{Value, json};

use crate::Result;
use crate::config;
use crate::runtime::core::execution::JobDecision;
use crate::runtime::util::{first_non_empty, single_child_of_kind, string_field};
use crate::runtime::{
    AgentSessionRecord, AgentSessionRecordState, AgentSessionRouteKind, BinaryPayload,
    DiscordForumThreadCreatePayload, DiscordTextSendPayload, Job, JobKind, JobOutput, JobState,
    Runtime, TextDeliveryOutput, TextDeliveryPayload, TextTarget, TextTargetKind,
};

enum TextDeliveryTarget {
    Ready(TextTarget),
    WaitFor(Job),
}

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
        if children
            .iter()
            .any(|child| child.kind == JobKind::DiscordTextSend)
        {
            return self
                .complete_text_delivery_from_child(job, payload, &children)
                .await;
        }

        let target = match self
            .resolve_text_delivery_target(job, payload, &children)
            .await?
        {
            TextDeliveryTarget::Ready(target) => target,
            TextDeliveryTarget::WaitFor(child) => return Ok(JobDecision::WaitFor(vec![child])),
        };
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
        children: &[Job],
    ) -> Result<TextDeliveryTarget> {
        match payload.target.kind {
            TextTargetKind::Channel => {
                require_target_id(&payload.target.channel_id, "channel", job)?;
                Ok(TextDeliveryTarget::Ready(payload.target.clone()))
            }
            TextTargetKind::Dm => {
                require_target_id(&payload.target.user_id, "dm", job)?;
                Ok(TextDeliveryTarget::Ready(payload.target.clone()))
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
                .map(TextDeliveryTarget::Ready)
            }
            TextTargetKind::AgentSession => {
                let session = self.session_for_text_delivery(job, payload).await?;
                self.resolve_agent_session_target(job, payload, children, session)
                    .await
            }
        }
    }

    async fn session_for_text_delivery(
        &self,
        job: &Job,
        payload: &TextDeliveryPayload,
    ) -> Result<AgentSessionRecord> {
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
        if session.state == AgentSessionRecordState::Retired
            && session.retirement_reason == "agent_session_resume_route_takeover"
        {
            return self
                .timeline_store
                .active_agent_session_for_route(&session.route_key)
                .await?
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "agent session {} was retired by resume takeover but route {} has no active session",
                        session.agent_session_id,
                        session.route_key
                    )
                });
        }
        Ok(session)
    }

    async fn resolve_agent_session_target(
        &self,
        job: &Job,
        payload: &TextDeliveryPayload,
        children: &[Job],
        mut session: AgentSessionRecord,
    ) -> Result<TextDeliveryTarget> {
        match session.text_target.kind {
            TextTargetKind::Dm => {
                require_target_id(&session.text_target.user_id, "dm", job)?;
                Ok(TextDeliveryTarget::Ready(session.text_target))
            }
            TextTargetKind::Channel if !session.text_target.channel_id.trim().is_empty() => {
                Ok(TextDeliveryTarget::Ready(session.text_target))
            }
            TextTargetKind::Channel if session.route_kind == AgentSessionRouteKind::Voice => {
                if let Some(thread_job) = children
                    .iter()
                    .find(|child| child.kind == JobKind::DiscordForumThreadCreate)
                {
                    let Some(JobOutput::DiscordForumThreadCreate(output)) =
                        thread_job.metadata.output.clone()
                    else {
                        anyhow::bail!(
                            "text delivery thread job {} completed without thread output",
                            thread_job.id
                        );
                    };
                    session.discord_thread_id = output.thread_id.clone();
                    session.discord_parent_channel_id = output.parent_channel_id;
                    session.text_target = TextTarget {
                        kind: TextTargetKind::Channel,
                        channel_id: output.thread_id,
                        user_id: String::new(),
                    };
                    self.timeline_store
                        .update_agent_session_record(&session)
                        .await?;
                    self.timeline_store
                        .append_event(
                            &session.guild_id,
                            &session.voice_channel_id,
                            json!({
                                "event_kind": "agent_session_thread_created",
                                "kind": "agent_session_thread_created",
                                "agent_session": session.to_json(),
                                "source_job_id": job.id,
                                "requested_by_user_id": payload.requested_by_user_id,
                            }),
                        )
                        .await?;
                    Ok(TextDeliveryTarget::Ready(session.text_target))
                } else {
                    if session.discord_parent_channel_id.trim().is_empty() {
                        anyhow::bail!(
                            "agent session {} has no Discord parent channel for thread allocation",
                            session.agent_session_id
                        );
                    }
                    Ok(TextDeliveryTarget::WaitFor(
                        Job::discord_forum_thread_create(
                            session.guild_id.clone(),
                            session.voice_channel_id.clone(),
                            payload.requested_by_user_id.clone(),
                            DiscordForumThreadCreatePayload {
                                parent_channel_id: session.discord_parent_channel_id.clone(),
                                name: self.default_agent_thread_name(&session).await?,
                                content: self
                                    .agent_thread_content(
                                        &session.guild_id,
                                        &session.voice_channel_id,
                                        &payload.requested_by_user_id,
                                        &session.agent_session_id,
                                    )
                                    .await?,
                                auto_archive_minutes: config::agent_thread_auto_archive_minutes(),
                                source_job_id: job.id.clone(),
                            },
                        ),
                    ))
                }
            }
            kind => anyhow::bail!(
                "agent session {} has unsupported text target {}",
                session.agent_session_id,
                kind.as_str()
            ),
        }
    }
}

fn require_target_id(value: &str, label: &str, job: &Job) -> Result<()> {
    if value.trim().is_empty() {
        anyhow::bail!("text delivery job {} has no {label} target id", job.id);
    }
    Ok(())
}
