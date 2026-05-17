use serde_json::json;

use crate::Result;
use crate::config;
use crate::runtime::core::execution::JobDecision;
use crate::runtime::domain::external::RuntimeExternalApi;
use crate::runtime::util::first_non_empty;
use crate::runtime::{
    AgentSessionRecord, AgentSessionRecordState, AgentSessionRouteKind,
    DiscordForumThreadCreatePayload, DiscordTypingAction, DiscordTypingIndicatorPayload, Job,
    JobKind, JobOutput, JobState, Runtime, RuntimeScope, TextTarget, TextTargetKind,
};

enum TypingTarget {
    Ready(TextTarget),
    WaitFor(Job),
}

impl Runtime {
    pub(crate) async fn execute_discord_typing_indicator_job<A>(
        &self,
        job: &Job,
        payload: &DiscordTypingIndicatorPayload,
        external_api: &A,
    ) -> Result<JobDecision>
    where
        A: RuntimeExternalApi,
    {
        let children = self.timeline_store.list_child_jobs(&job.id).await?;
        if children.iter().any(|child| !child.state.is_terminal()) {
            return Ok(JobDecision::Wait);
        }
        if let Some(failed) = children
            .iter()
            .find(|child| child.state != JobState::Complete)
        {
            return Ok(JobDecision::fail(format!(
                "discord typing dependency {} ended as {}: {}",
                failed.id, failed.state, failed.metadata.error
            )));
        }

        let target = match self.resolve_typing_target(job, payload, &children).await? {
            TypingTarget::Ready(target) => target,
            TypingTarget::WaitFor(child) => return Ok(JobDecision::WaitFor(vec![child])),
        };
        let output = external_api
            .discord_typing_indicator(DiscordTypingIndicatorPayload {
                action: payload.action,
                target,
                source_job_id: payload.source_job_id.clone(),
                requested_by_user_id: payload.requested_by_user_id.clone(),
                agent_task_attempt: payload.agent_task_attempt,
            })
            .await?;
        self.timeline_store
            .append_event(
                &job.guild_id,
                &job.scope_id,
                json!({
                    "event_kind": "discord_typing_indicator",
                    "kind": "discord_typing_indicator",
                    "job_id": job.id,
                    "source_job_id": payload.source_job_id,
                    "action": payload.action.as_str(),
                    "target": output.target.to_json(),
                    "status": output.status,
                }),
            )
            .await?;
        Ok(JobDecision::Complete(JobOutput::DiscordTypingIndicator(
            output,
        )))
    }

    async fn resolve_typing_target(
        &self,
        job: &Job,
        payload: &DiscordTypingIndicatorPayload,
        children: &[Job],
    ) -> Result<TypingTarget> {
        match payload.target.kind {
            TextTargetKind::Channel => {
                require_typing_target_id(&payload.target.channel_id, "channel", job)?;
                Ok(TypingTarget::Ready(payload.target.clone()))
            }
            TextTargetKind::Dm => {
                require_typing_target_id(&payload.target.user_id, "dm", job)?;
                Ok(TypingTarget::Ready(payload.target.clone()))
            }
            TextTargetKind::AgentChat => {
                let control = self.timeline_store.control_config().await?;
                let channel_id = control.bots_channel_id.trim();
                if channel_id.is_empty() {
                    anyhow::bail!("botsChannelId is not configured");
                }
                Ok(TypingTarget::Ready(TextTarget {
                    kind: TextTargetKind::Channel,
                    channel_id: channel_id.to_string(),
                    user_id: String::new(),
                }))
            }
            TextTargetKind::AgentSession => {
                let session = self.session_for_typing_indicator(job, payload).await?;
                self.resolve_agent_session_typing_target(job, payload, children, session)
                    .await
            }
        }
    }

    async fn session_for_typing_indicator(
        &self,
        job: &Job,
        payload: &DiscordTypingIndicatorPayload,
    ) -> Result<AgentSessionRecord> {
        let source_job_id = payload.source_job_id.trim();
        if source_job_id.is_empty() {
            anyhow::bail!(
                "discord typing job {} uses session target without source job",
                job.id
            );
        }
        let source = self.timeline_store.get_job(source_job_id).await?;
        let crate::runtime::JobPayload::AgentTask(agent_task) = &source.payload else {
            anyhow::bail!(
                "discord typing job {} uses session target but source job {} is not an agent task",
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

    async fn resolve_agent_session_typing_target(
        &self,
        job: &Job,
        payload: &DiscordTypingIndicatorPayload,
        children: &[Job],
        mut session: AgentSessionRecord,
    ) -> Result<TypingTarget> {
        match session.text_target.kind {
            TextTargetKind::Dm => {
                require_typing_target_id(&session.text_target.user_id, "dm", job)?;
                Ok(TypingTarget::Ready(session.text_target))
            }
            TextTargetKind::Channel if !session.text_target.channel_id.trim().is_empty() => {
                Ok(TypingTarget::Ready(session.text_target))
            }
            TextTargetKind::Channel
                if session.route_kind == AgentSessionRouteKind::Voice
                    && payload.action == DiscordTypingAction::Start =>
            {
                if let Some(thread_job) = children
                    .iter()
                    .find(|child| child.kind == JobKind::DiscordForumThreadCreate)
                {
                    let Some(JobOutput::DiscordForumThreadCreate(output)) =
                        thread_job.metadata.output.clone()
                    else {
                        anyhow::bail!(
                            "discord typing thread job {} completed without thread output",
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
                            &session.scope_id,
                            json!({
                                "event_kind": "agent_session_thread_created",
                                "kind": "agent_session_thread_created",
                                "agent_session": session.to_json(),
                                "source_job_id": job.id,
                                "requested_by_user_id": payload.requested_by_user_id,
                            }),
                        )
                        .await?;
                    Ok(TypingTarget::Ready(session.text_target))
                } else {
                    if session.discord_parent_channel_id.trim().is_empty() {
                        anyhow::bail!(
                            "agent session {} has no Discord parent channel for thread allocation",
                            session.agent_session_id
                        );
                    }
                    Ok(TypingTarget::WaitFor(Job::discord_forum_thread_create(
                        RuntimeScope::voice_channel(
                            session.guild_id.clone(),
                            session.scope_id.clone(),
                        ),
                        payload.requested_by_user_id.clone(),
                        DiscordForumThreadCreatePayload {
                            parent_channel_id: session.discord_parent_channel_id.clone(),
                            name: self.default_agent_thread_name(&session).await?,
                            content: self
                                .agent_thread_content(
                                    &session.guild_id,
                                    &session.scope_id,
                                    &first_non_empty([
                                        payload.requested_by_user_id.clone(),
                                        job.requested_by_user_id.clone(),
                                    ]),
                                    &session.agent_session_id,
                                )
                                .await?,
                            auto_archive_minutes: config::agent_thread_auto_archive_minutes(),
                            source_job_id: job.id.clone(),
                        },
                    )))
                }
            }
            kind => anyhow::bail!(
                "agent session {} has unsupported typing target {} for {}",
                session.agent_session_id,
                kind.as_str(),
                payload.action.as_str()
            ),
        }
    }
}

fn require_typing_target_id(value: &str, label: &str, job: &Job) -> Result<()> {
    if value.trim().is_empty() {
        anyhow::bail!("discord typing job {} has no {label} target id", job.id);
    }
    Ok(())
}
