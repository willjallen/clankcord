use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::Path;

use crate::Result;
use crate::config;
use crate::engine::JobDecision;
use crate::model::job::{
    BinaryPayload, DiscordForumThreadCreatePayload, DiscordTextSendPayload, Job, JobKind,
    JobOutput, JobState, TextAttachmentPayload, TextDeliveryOutput, TextDeliveryPayload,
    TextTarget, TextTargetKind,
};
use crate::model::scope::{RuntimeScope, RuntimeScopeKind};
use crate::runtime::Ctx;
use crate::runtime::agents::{AgentSessionRecord, AgentSessionRouteKind};
use crate::runtime::domain::messaging::session_threads::{
    discord_error_text_targets_unavailable_session_thread,
    discord_error_text_unavailable_channel_id,
};
use crate::runtime::timeline::sha256_file;
use crate::runtime::util::{first_non_empty, string_field};

enum TextDeliveryTarget {
    Ready(TextTarget),
    WaitFor(Job),
}

/// The agent loopback entry: `clankcord responses send` posts here. The
/// delivery is created as a child of its source agent task, so lineage
/// covers the system's most important causal chain and the parent/child
/// machinery drives the task's completion.
pub(crate) async fn submit_agent_response_delivery(ctx: &Ctx, value: &Value) -> Result<Value> {
    let job = text_delivery_job_from_value(ctx, value).await?;
    let source_job_id = match &job.payload {
        crate::model::job::JobPayload::TextDelivery(payload) => payload.source_job_id.clone(),
        _ => String::new(),
    };
    let created = if source_job_id.trim().is_empty() {
        ctx.store.create_job(job).await?
    } else {
        let parent = ctx.store.get_job(&source_job_id).await?;
        ctx.store.create_child_job(&parent, job).await?
    };
    Ok(serde_json::json!({
        "kind": "job_created",
        "job_ids": [created.id.clone()],
        "job": created.to_value(),
    }))
}

pub(crate) async fn text_delivery_job_from_value(ctx: &Ctx, value: &Value) -> Result<Job> {
    let mut payload = TextDeliveryPayload::from_json(value)?;
    let source = if payload.source_job_id.trim().is_empty() {
        None
    } else {
        Some(ctx.store.get_job(&payload.source_job_id).await?)
    };
    let scope = text_delivery_scope_from_value(value, source.as_ref())?;
    if payload.requested_by_user_id.trim().is_empty() {
        payload.requested_by_user_id = source
            .as_ref()
            .map(|job| job.requested_by_user_id.clone())
            .unwrap_or_default();
    }
    Ok(Job::text_delivery(
        scope,
        payload.requested_by_user_id.clone(),
        payload,
    ))
}

pub(crate) async fn prepare_text_delivery_job(
    ctx: &Ctx,
    job: &Job,
    payload: &TextDeliveryPayload,
) -> Result<JobDecision> {
    if payload.content.trim().is_empty() {
        return Ok(JobDecision::fail(format!(
            "text delivery job {} has empty content",
            job.id
        )));
    }
    let children = ctx.store.list_child_jobs(&job.id).await?;
    if children.iter().any(|child| !child.state.is_terminal()) {
        return Ok(JobDecision::Wait);
    }
    let ignored_failures =
        repair_text_delivery_unavailable_thread_failures(ctx, job, payload, &children).await?;
    if let Some(failed) = children
        .iter()
        .find(|child| child.state != JobState::Complete && !ignored_failures.contains(&child.id))
    {
        return Ok(JobDecision::fail(format!(
            "text delivery dependency {} ended as {}: {}",
            failed.id, failed.state, failed.metadata.error
        )));
    }
    if completed_child_of_kind(&children, JobKind::DiscordTextSend)?.is_some() {
        return complete_text_delivery_from_child(ctx, job, payload, &children).await;
    }

    let target = match resolve_text_delivery_target(ctx, job, payload, &children).await? {
        TextDeliveryTarget::Ready(target) => target,
        TextDeliveryTarget::WaitFor(child) => return Ok(JobDecision::WaitFor(vec![child])),
    };
    let attachments = resolve_text_delivery_attachments(&payload.attachments)?;
    let child = Job::discord_text_send(
        job.scope(),
        job.requested_by_user_id.clone(),
        DiscordTextSendPayload {
            intent: payload.intent,
            target,
            content: payload.content.clone(),
            source_job_id: payload.source_job_id.clone(),
            requested_by_user_id: payload.requested_by_user_id.clone(),
            allowed_mentions: BinaryPayload::empty(),
            components: BinaryPayload::empty(),
            attachments,
        },
    );
    Ok(JobDecision::WaitFor(vec![child]))
}

async fn complete_text_delivery_from_child(
    ctx: &Ctx,
    job: &Job,
    payload: &TextDeliveryPayload,
    children: &[Job],
) -> Result<JobDecision> {
    let Some(send_child) = completed_child_of_kind(children, JobKind::DiscordTextSend)? else {
        return Ok(JobDecision::fail(format!(
            "text delivery job {} has no completed discord text send child",
            job.id
        )));
    };
    let Some(JobOutput::DiscordTextSend(output)) = send_child.metadata.output.clone() else {
        return Ok(JobDecision::fail(format!(
            "text delivery child {} completed without discord text output",
            send_child.id
        )));
    };
    ctx.store
        .append_scope_event(
            &job.scope(),
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
    ctx: &Ctx,
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
            let control = ctx.store.control_config().await?;
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
            let session =
                crate::runtime::domain::messaging::session_threads::agent_session_for_source_job(
                    ctx,
                    job,
                    &payload.source_job_id,
                    "text delivery",
                )
                .await?;
            resolve_agent_session_target(ctx, job, payload, children, session).await
        }
    }
}

async fn repair_text_delivery_unavailable_thread_failures(
    ctx: &Ctx,
    job: &Job,
    payload: &TextDeliveryPayload,
    children: &[Job],
) -> Result<BTreeSet<String>> {
    let mut ignored = BTreeSet::new();
    if payload.target.kind != TextTargetKind::AgentSession {
        return Ok(ignored);
    }
    let session = crate::runtime::domain::messaging::session_threads::agent_session_for_source_job(
        ctx,
        job,
        &payload.source_job_id,
        "text delivery",
    )
    .await?;
    for child in children {
        if child.kind != JobKind::DiscordTextSend || child.state == JobState::Complete {
            continue;
        }
        let Some(target) = discord_text_send_child_target(child) else {
            continue;
        };
        if !discord_error_text_targets_unavailable_session_thread(&child.metadata.error, target) {
            continue;
        }
        let thread_id = first_non_empty([
            discord_error_text_unavailable_channel_id(&child.metadata.error),
            target.channel_id.clone(),
        ]);
        crate::runtime::domain::messaging::session_threads::mark_agent_session_thread_unavailable(
            ctx,
            &session.agent_session_id,
            &thread_id,
            &child.id,
            &child.metadata.error,
        )
        .await?;
        ignored.insert(child.id.clone());
    }
    Ok(ignored)
}

async fn resolve_agent_session_target(
    ctx: &Ctx,
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
                ctx.store.update_agent_session_record(&session).await?;
                ctx.store
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
                            RuntimeScope::voice_channel(
                                session.guild_id.clone(),
                                session.scope_id.clone(),
                            ),
                            payload.requested_by_user_id.clone(),
                            DiscordForumThreadCreatePayload {
                                parent_channel_id: session.discord_parent_channel_id.clone(),
                                name: crate::runtime::domain::interactions::agent_sessions::default_agent_thread_name(ctx, &session).await?,
                                content: crate::runtime::domain::interactions::agent_sessions::agent_thread_content(ctx,
                                        &session.guild_id,
                                        &session.scope_id,
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

fn completed_child_of_kind(children: &[Job], kind: JobKind) -> Result<Option<&Job>> {
    let matches = children
        .iter()
        .filter(|child| child.kind == kind && child.state == JobState::Complete)
        .collect::<Vec<_>>();
    if matches.len() > 1 {
        anyhow::bail!(
            "expected at most one completed {kind} child, found {}",
            matches.len()
        );
    }
    Ok(matches.first().copied())
}

fn discord_text_send_child_target(child: &Job) -> Option<&TextTarget> {
    match &child.payload {
        crate::model::job::JobPayload::DiscordTextSend(payload) => Some(&payload.target),
        _ => None,
    }
}

fn require_target_id(value: &str, label: &str, job: &Job) -> Result<()> {
    if value.trim().is_empty() {
        anyhow::bail!("text delivery job {} has no {label} target id", job.id);
    }
    Ok(())
}

fn resolve_text_delivery_attachments(
    attachments: &[TextAttachmentPayload],
) -> Result<Vec<TextAttachmentPayload>> {
    attachments
        .iter()
        .map(|attachment| {
            let path_text = attachment.path.trim();
            if path_text.is_empty() {
                anyhow::bail!("text delivery attachment has empty path");
            }
            let path = Path::new(path_text);
            if !path.is_file() {
                anyhow::bail!("text delivery attachment is not a readable file: {path_text}");
            }
            let extension = path
                .extension()
                .and_then(|value| value.to_str())
                .unwrap_or_default();
            if !extension.eq_ignore_ascii_case("zip") {
                anyhow::bail!("text delivery attachment must be a .zip file: {path_text}");
            }
            let filename = path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or_default()
                .trim()
                .to_string();
            if filename.is_empty() {
                anyhow::bail!("text delivery attachment has no filename: {path_text}");
            }
            Ok(TextAttachmentPayload {
                path: path_text.to_string(),
                filename,
                size_bytes: path.metadata()?.len(),
                sha256: sha256_file(path)?,
            })
        })
        .collect()
}

fn text_delivery_scope_from_value(value: &Value, source: Option<&Job>) -> Result<RuntimeScope> {
    let source_scope = source.map(Job::scope);
    let raw_kind = string_field(value, "scope_kind");
    let kind = if raw_kind.trim().is_empty() {
        source_scope
            .as_ref()
            .map(|scope| scope.kind)
            .ok_or_else(|| anyhow::anyhow!("text delivery is missing scope_kind"))?
    } else {
        raw_kind.parse::<RuntimeScopeKind>()?
    };
    let scope_id = {
        let explicit = string_field(value, "scope_id");
        if explicit.trim().is_empty() {
            source_scope
                .as_ref()
                .map(|scope| scope.scope_id.clone())
                .unwrap_or_default()
        } else {
            explicit
        }
    };
    if scope_id.trim().is_empty() {
        anyhow::bail!("text delivery is missing scope_id");
    }
    let guild_id = {
        let explicit = string_field(value, "guild_id");
        if explicit.trim().is_empty() {
            source_scope
                .as_ref()
                .map(|scope| scope.guild_id.clone())
                .unwrap_or_default()
        } else {
            explicit
        }
    };
    if matches!(
        kind,
        RuntimeScopeKind::VoiceChannel | RuntimeScopeKind::TextChannel | RuntimeScopeKind::Thread
    ) && guild_id.trim().is_empty()
    {
        anyhow::bail!("text delivery scope {kind:?} requires guild_id");
    }
    Ok(RuntimeScope {
        kind,
        guild_id,
        scope_id,
    })
}
