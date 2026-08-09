use serde_json::json;

use crate::Result;
use crate::engine::JobDecision;
use crate::model::job::{
    DiscordTypingIndicatorOutput, DiscordTypingIndicatorPayload, Job, JobOutput, TextTarget,
    TextTargetKind,
};
use crate::ports::discord::DiscordApi;
use crate::runtime::Ctx;
use crate::runtime::agents::{AgentSessionRecord, AgentSessionRouteKind};
use crate::runtime::domain::messaging::session_threads::{
    UNAVAILABLE_SESSION_THREAD_STATUS, discord_error_targets_unavailable_session_thread,
    discord_error_unavailable_channel_id,
};
use crate::runtime::util::first_non_empty;

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

pub(crate) async fn execute_discord_typing_indicator_job<A>(
    ctx: &Ctx,
    job: &Job,
    payload: &DiscordTypingIndicatorPayload,
    external_api: &A,
) -> Result<JobDecision>
where
    A: DiscordApi,
{
    match crate::runtime::domain::children::await_children(
        ctx,
        &job.id,
        "discord typing dependency",
    )
    .await?
    {
        crate::runtime::domain::children::ChildResolution::Pending => {
            return Ok(JobDecision::Wait);
        }
        crate::runtime::domain::children::ChildResolution::Failed { message, .. } => {
            return Ok(JobDecision::fail(message));
        }
        crate::runtime::domain::children::ChildResolution::Settled(_) => {}
    }

    let output = match resolve_typing_target(ctx, job, payload).await? {
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
                crate::runtime::domain::messaging::session_threads::mark_agent_session_thread_unavailable(ctx,
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
    ctx.store
        .append_scope_event(
            &job.scope(),
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
    ctx: &Ctx,
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
            let control = ctx.store.control_config().await?;
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
            let session =
                crate::runtime::domain::messaging::session_threads::agent_session_for_source_job(
                    ctx,
                    job,
                    &payload.source_job_id,
                    "discord typing",
                )
                .await?;
            resolve_agent_session_typing_target(ctx, job, payload, session).await
        }
    }
}

async fn resolve_agent_session_typing_target(
    _ctx: &Ctx,
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

fn require_typing_target_id(value: &str, label: &str, job: &Job) -> Result<()> {
    if value.trim().is_empty() {
        anyhow::bail!("discord typing job {} has no {label} target id", job.id);
    }
    Ok(())
}
