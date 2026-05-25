use serde_json::json;

use crate::Result;
use crate::runtime::core::execution::JobDecision;
use crate::runtime::domain::external::RuntimeExternalApi;
use crate::runtime::domain::messaging::session_threads::{
    UNAVAILABLE_SESSION_THREAD_STATUS, discord_error_targets_unavailable_session_thread,
    discord_error_unavailable_channel_id,
};
use crate::runtime::util::first_non_empty;
use crate::runtime::{
    AgentSessionRecord, AgentSessionRecordState, AgentSessionRouteKind,
    DiscordTypingIndicatorOutput, DiscordTypingIndicatorPayload, Job, JobOutput, JobState, Runtime,
    TextTarget, TextTargetKind,
};

const NO_SESSION_THREAD_TYPING_STATUS: &str = "skipped_no_session_thread";

enum TypingTarget {
    Ready(ResolvedTypingTarget),
    Skipped {
        target: TextTarget,
        status: &'static str,
    },
}

struct ResolvedTypingTarget {
    target: TextTarget,
    agent_session_id: String,
    thread_id: String,
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

        let output = match self.resolve_typing_target(job, payload).await? {
            TypingTarget::Ready(resolved) => match external_api
                .discord_typing_indicator(DiscordTypingIndicatorPayload {
                    action: payload.action,
                    target: resolved.target.clone(),
                    source_job_id: payload.source_job_id.clone(),
                    requested_by_user_id: payload.requested_by_user_id.clone(),
                    agent_task_attempt: payload.agent_task_attempt,
                })
                .await
            {
                Ok(output) => output,
                Err(error)
                    if !resolved.agent_session_id.trim().is_empty()
                        && discord_error_targets_unavailable_session_thread(
                            &error,
                            &resolved.target,
                        ) =>
                {
                    let thread_id = first_non_empty([
                        discord_error_unavailable_channel_id(&error),
                        resolved.thread_id.clone(),
                        resolved.target.channel_id.clone(),
                    ]);
                    self.mark_agent_session_thread_unavailable(
                        &resolved.agent_session_id,
                        &thread_id,
                        &job.id,
                        &error.to_string(),
                    )
                    .await?;
                    DiscordTypingIndicatorOutput {
                        action: payload.action,
                        target: resolved.target,
                        source_job_id: payload.source_job_id.clone(),
                        status: UNAVAILABLE_SESSION_THREAD_STATUS.to_string(),
                    }
                }
                Err(error) => return Err(error),
            },
            TypingTarget::Skipped { target, status } => DiscordTypingIndicatorOutput {
                action: payload.action,
                target,
                source_job_id: payload.source_job_id.clone(),
                status: status.to_string(),
            },
        };
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
    ) -> Result<TypingTarget> {
        match payload.target.kind {
            TextTargetKind::Channel => {
                require_typing_target_id(&payload.target.channel_id, "channel", job)?;
                Ok(TypingTarget::Ready(ResolvedTypingTarget {
                    target: payload.target.clone(),
                    agent_session_id: String::new(),
                    thread_id: String::new(),
                }))
            }
            TextTargetKind::Dm => {
                require_typing_target_id(&payload.target.user_id, "dm", job)?;
                Ok(TypingTarget::Ready(ResolvedTypingTarget {
                    target: payload.target.clone(),
                    agent_session_id: String::new(),
                    thread_id: String::new(),
                }))
            }
            TextTargetKind::AgentChat => {
                let control = self.timeline_store.control_config().await?;
                let channel_id = control.bots_channel_id.trim();
                if channel_id.is_empty() {
                    anyhow::bail!("botsChannelId is not configured");
                }
                Ok(TypingTarget::Ready(ResolvedTypingTarget {
                    target: TextTarget {
                        kind: TextTargetKind::Channel,
                        channel_id: channel_id.to_string(),
                        user_id: String::new(),
                    },
                    agent_session_id: String::new(),
                    thread_id: String::new(),
                }))
            }
            TextTargetKind::AgentSession => {
                let session = self.session_for_typing_indicator(job, payload).await?;
                self.resolve_agent_session_typing_target(job, payload, session)
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
        session: AgentSessionRecord,
    ) -> Result<TypingTarget> {
        match session.text_target.kind {
            TextTargetKind::Dm => {
                require_typing_target_id(&session.text_target.user_id, "dm", job)?;
                Ok(TypingTarget::Ready(ResolvedTypingTarget {
                    target: session.text_target,
                    agent_session_id: String::new(),
                    thread_id: String::new(),
                }))
            }
            TextTargetKind::Channel if !session.text_target.channel_id.trim().is_empty() => {
                let thread_id = session.text_target.channel_id.clone();
                Ok(TypingTarget::Ready(ResolvedTypingTarget {
                    target: session.text_target,
                    agent_session_id: session.agent_session_id,
                    thread_id,
                }))
            }
            TextTargetKind::Channel if session.route_kind == AgentSessionRouteKind::Voice => {
                Ok(TypingTarget::Skipped {
                    target: session.text_target,
                    status: NO_SESSION_THREAD_TYPING_STATUS,
                })
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
