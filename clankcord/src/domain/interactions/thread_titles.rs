use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::prompts::{
    render_agent_thread_title_prompt_from_dir, render_configured_agent_thread_title_prompt,
};
use crate::Result;
use crate::adapters::codex::codex_response_text;
use crate::config;
use crate::domain::Ctx;
use crate::domain::agents::{
    AgentInfrastructureError, AgentInvocationRequest, AgentRole, AgentRuntime,
};
use crate::domain::interactions::agent_sessions;
use crate::domain::messaging::session_threads;
use crate::domain::messaging::session_threads::{
    UNAVAILABLE_SESSION_THREAD_STATUS, discord_error_text_unavailable_channel_id,
};
use crate::domain::rooms::catalog;
use crate::engine::JobDecision;
use crate::errors::discord_error_text_is_unavailable_channel;
use crate::model::agents::{AgentSessionRecord, AgentSessionRouteKind};
use crate::model::job::{
    AgentThreadTitleRefreshPayload, DiscordForumThreadRenamePayload, Job, JobKind, JobOutput,
    JobPayload, JobState, TextTargetKind,
};
use crate::model::scope::RuntimeScope;
use crate::store::JobVisibility;
use crate::util::{first_non_empty, first_value_string, preview};

use super::linear_mcp::insert_linear_mcp_env;

const THREAD_TITLE_RESPONSE_INTERVAL: usize = 1;
const THREAD_TITLE_MAX_CANDIDATES_PER_RUN: usize = 1;
const THREAD_TITLE_MAX_CHARS: usize = 80;
const THREAD_TITLE_RESPONSE_PREVIEW_CHARS: usize = 420;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentThreadTitlePromptContext {
    pub agent_session_id: String,
    pub current_thread_title: String,
    pub voice_channel_name: String,
    pub response_count: usize,
    pub responses: Vec<String>,
}

#[derive(Debug, Clone)]
struct ThreadTitleInvocation {
    title: String,
}

pub(crate) async fn agent_thread_title_refresh_jobs(
    ctx: &Ctx,
    source_job: &Job,
) -> Result<Vec<Job>> {
    let active_session_ids = active_agent_thread_title_refresh_session_ids(ctx).await?;
    let records = ctx
        .store
        .list_agent_session_records("", "", "active", 500)
        .await?;
    let mut jobs = Vec::new();
    for record in records {
        if jobs.len() >= THREAD_TITLE_MAX_CANDIDATES_PER_RUN {
            break;
        }
        if record.route_kind != AgentSessionRouteKind::Voice
            || record.discord_thread_id.trim().is_empty()
            || active_session_ids.contains(&record.agent_session_id)
        {
            continue;
        }
        let responses = agent_thread_response_summaries(ctx, &record).await?;
        let response_count = responses.len();
        if response_count < THREAD_TITLE_RESPONSE_INTERVAL {
            continue;
        }
        let last_attempt_count =
            last_agent_thread_title_refresh_attempt_count(ctx, &record).await?;
        if response_count < last_attempt_count.saturating_add(THREAD_TITLE_RESPONSE_INTERVAL) {
            continue;
        }
        let current_thread_name = match latest_agent_thread_title(ctx, &record).await? {
            Some(title) => title,
            None => agent_sessions::default_agent_thread_name(ctx, &record).await?,
        };
        jobs.push(Job::agent_thread_title_refresh(
            source_job.id.clone(),
            record.agent_session_id,
            record.guild_id,
            record.scope_id,
            record.discord_thread_id,
            current_thread_name,
            response_count,
        ));
    }
    Ok(jobs)
}

pub(crate) async fn execute_agent_thread_title_refresh_job(
    ctx: &Ctx,
    job: &Job,
    payload: &AgentThreadTitleRefreshPayload,
) -> Result<JobDecision> {
    validate_thread_title_refresh_payload(job, payload)?;
    let children = ctx.store.list_child_jobs(&job.id).await?;
    if children.iter().any(|child| !child.state.is_terminal()) {
        return Ok(JobDecision::Wait);
    }
    if let Some(decision) =
        complete_thread_title_refresh_for_unavailable_thread(ctx, job, payload, &children).await?
    {
        return Ok(decision);
    }
    if let Some(failed) = children
        .iter()
        .find(|child| child.state != JobState::Complete)
    {
        return Ok(JobDecision::fail(format!(
            "agent thread title dependency {} ended as {}: {}",
            failed.id, failed.state, failed.metadata.error
        )));
    }
    if !children.is_empty() {
        return complete_agent_thread_title_refresh_job(ctx, job, payload, &children).await;
    }

    record_agent_thread_title_refresh_attempt(ctx, job, payload).await?;
    let context = agent_thread_title_prompt_context(ctx, payload).await?;
    let prompt = build_agent_thread_title_prompt(&context)?;
    let invocation = invoke_agent_thread_title(ctx, job, payload, prompt).await?;
    let rename = Job::discord_forum_thread_rename(
        RuntimeScope::voice_channel(payload.guild_id.clone(), payload.voice_channel_id.clone()),
        "runtime",
        DiscordForumThreadRenamePayload {
            thread_id: payload.discord_thread_id.clone(),
            name: invocation.title.clone(),
            source_job_id: job.id.clone(),
        },
    );
    Ok(JobDecision::WaitFor(vec![rename]))
}

async fn active_agent_thread_title_refresh_session_ids(ctx: &Ctx) -> Result<BTreeSet<String>> {
    let jobs = ctx
        .store
        .list_jobs_by_states_with_visibility(
            None,
            &[
                JobState::Queued,
                JobState::Running,
                JobState::Waiting,
                JobState::CancelRequested,
            ],
            JobVisibility::IncludeEphemeral,
        )
        .await?;
    Ok(jobs
        .into_iter()
        .filter_map(|job| match job.payload {
            JobPayload::AgentThreadTitleRefresh(payload)
                if job.kind == JobKind::AgentThreadTitleRefresh =>
            {
                Some(payload.agent_session_id)
            }
            _ => None,
        })
        .collect())
}

async fn agent_thread_response_summaries(
    ctx: &Ctx,
    record: &AgentSessionRecord,
) -> Result<Vec<String>> {
    let mut agent_tasks = ctx
        .store
        .list_jobs_by_scope_kind(&record.guild_id, &record.scope_id, JobKind::AgentTask)
        .await?;
    agent_tasks.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then_with(|| left.id.cmp(&right.id))
    });
    let mut responses = Vec::new();
    for task in agent_tasks {
        let JobPayload::AgentTask(task_payload) = &task.payload else {
            continue;
        };
        if task_payload.agent_session_id != record.agent_session_id {
            continue;
        }
        let deliveries = ctx
            .store
            .list_text_delivery_jobs_for_source(&task.id)
            .await?;
        let Some(delivery) = deliveries
            .iter()
            .find(|delivery| delivery_is_visible_agent_thread_response(delivery, record))
        else {
            continue;
        };
        responses.push(agent_thread_response_summary(
            responses.len() + 1,
            task_payload.command.arguments.request_text(),
            text_delivery_content(delivery),
        ));
    }
    Ok(responses)
}

async fn agent_thread_title_prompt_context(
    ctx: &Ctx,
    payload: &AgentThreadTitleRefreshPayload,
) -> Result<AgentThreadTitlePromptContext> {
    let room =
        catalog::room_for_channel_ids(ctx, &payload.guild_id, &payload.voice_channel_id, None)
            .await?;
    let record = ctx
        .store
        .get_agent_session_record(&payload.agent_session_id)
        .await?;
    let mut responses = agent_thread_response_summaries(ctx, &record).await?;
    responses.truncate(payload.response_count);
    Ok(AgentThreadTitlePromptContext {
        agent_session_id: payload.agent_session_id.clone(),
        current_thread_title: payload.current_thread_name.clone(),
        voice_channel_name: room.channel_name,
        response_count: responses.len(),
        responses,
    })
}

async fn invoke_agent_thread_title(
    ctx: &Ctx,
    job: &Job,
    payload: &AgentThreadTitleRefreshPayload,
    prompt: String,
) -> Result<ThreadTitleInvocation> {
    let workdir = agent_thread_title_workdir(&payload.agent_session_id);
    fs::create_dir_all(&workdir)?;
    let job_dir = ctx
        .store
        .channel_dir(&payload.guild_id, &payload.voice_channel_id)
        .join("jobs");
    fs::create_dir_all(&job_dir)?;
    let prompt_path = job_dir.join(format!("{}.agent-thread-title-prompt.txt", job.id));
    let result_path = job_dir.join(format!("{}.agent-thread-title-result.txt", job.id));
    let raw_result_path = job_dir.join(format!("{}.agent-thread-title.codex.jsonl", job.id));
    fs::write(&prompt_path, &prompt)?;
    let invocation = AgentRuntime::default().invoke(AgentInvocationRequest {
        role: AgentRole::ThreadTitle,
        prior_session_id: String::new(),
        prompt,
        cwd: Some(workdir),
        model: config::codex_model(),
        reasoning_effort: config::codex_reasoning_effort(),
        fast_mode: config::codex_fast_mode(),
        env: agent_thread_title_env(job)?,
        result_path: result_path.clone(),
        raw_result_path: raw_result_path.clone(),
    })?;
    if !invocation.success {
        let detail = first_non_empty([
            invocation.stderr.trim().to_string(),
            invocation.stdout.trim().to_string(),
            format!(
                "codex exited {}",
                invocation
                    .returncode
                    .map(|code| code.to_string())
                    .unwrap_or_else(|| "without a status code".to_string())
            ),
        ]);
        if detail.contains("TokenRefreshFailed")
            || detail.contains("invalid_grant")
            || detail.contains("Auth(")
        {
            return Err(AgentInfrastructureError::new(detail).into());
        }
        anyhow::bail!("{detail}");
    }
    let response_text = codex_response_text(&invocation.stdout, &invocation.final_message);
    Ok(ThreadTitleInvocation {
        title: sanitize_agent_thread_title(&response_text)?,
    })
}

async fn complete_agent_thread_title_refresh_job(
    ctx: &Ctx,
    job: &Job,
    payload: &AgentThreadTitleRefreshPayload,
    children: &[Job],
) -> Result<JobDecision> {
    let rename_child = children
        .iter()
        .find(|child| child.kind == JobKind::DiscordForumThreadRename)
        .ok_or_else(|| anyhow::anyhow!("agent thread title refresh has no rename child"))?;
    let Some(JobOutput::DiscordForumThreadRename(output)) = rename_child.metadata.output.clone()
    else {
        return Ok(JobDecision::fail(format!(
            "agent thread title child {} completed without rename output",
            rename_child.id
        )));
    };
    ctx.store
        .append_event(
            &payload.guild_id,
            &payload.voice_channel_id,
            json!({
                "event_kind": "agent_thread_titled",
                "kind": "agent_thread_titled",
                "agent_session_id": payload.agent_session_id,
                "discord_thread_id": payload.discord_thread_id,
                "title": output.name,
                "response_count": payload.response_count,
                "refresh_job_id": job.id,
                "rename_job_id": rename_child.id,
            }),
        )
        .await?;
    Ok(JobDecision::Complete(JobOutput::from_boundary_json(
        &json!({
            "kind": "agent_thread_title_refresh",
            "agent_session_id": payload.agent_session_id,
            "discord_thread_id": payload.discord_thread_id,
            "title": output.name,
            "response_count": payload.response_count,
            "rename_job_id": rename_child.id,
        }),
    )?))
}

async fn complete_thread_title_refresh_for_unavailable_thread(
    ctx: &Ctx,
    job: &Job,
    payload: &AgentThreadTitleRefreshPayload,
    children: &[Job],
) -> Result<Option<JobDecision>> {
    let Some(rename_child) = children
        .iter()
        .find(|child| child.kind == JobKind::DiscordForumThreadRename)
    else {
        return Ok(None);
    };
    if rename_child.state == JobState::Complete
        || !discord_error_text_is_unavailable_channel(&rename_child.metadata.error)
    {
        return Ok(None);
    }
    let Some(rename_thread_id) = discord_forum_thread_rename_child_thread_id(rename_child) else {
        return Ok(None);
    };
    if rename_thread_id != payload.discord_thread_id {
        return Ok(None);
    }
    let thread_id = first_non_empty([
        discord_error_text_unavailable_channel_id(&rename_child.metadata.error),
        payload.discord_thread_id.clone(),
    ]);
    session_threads::mark_agent_session_thread_unavailable(
        ctx,
        &payload.agent_session_id,
        &thread_id,
        &rename_child.id,
        &rename_child.metadata.error,
    )
    .await?;
    ctx.store
        .append_event(
            &payload.guild_id,
            &payload.voice_channel_id,
            json!({
                "event_kind": "agent_thread_title_skipped",
                "kind": "agent_thread_title_skipped",
                "agent_session_id": payload.agent_session_id,
                "discord_thread_id": payload.discord_thread_id,
                "response_count": payload.response_count,
                "refresh_job_id": job.id,
                "rename_job_id": rename_child.id,
                "status": UNAVAILABLE_SESSION_THREAD_STATUS,
                "reason": preview(&rename_child.metadata.error, 500),
            }),
        )
        .await?;
    Ok(Some(JobDecision::Complete(JobOutput::from_boundary_json(
        &json!({
            "kind": "agent_thread_title_refresh",
            "agent_session_id": payload.agent_session_id,
            "discord_thread_id": payload.discord_thread_id,
            "response_count": payload.response_count,
            "rename_job_id": rename_child.id,
            "status": UNAVAILABLE_SESSION_THREAD_STATUS,
        }),
    )?)))
}

async fn record_agent_thread_title_refresh_attempt(
    ctx: &Ctx,
    job: &Job,
    payload: &AgentThreadTitleRefreshPayload,
) -> Result<()> {
    ctx.store
        .append_event(
            &payload.guild_id,
            &payload.voice_channel_id,
            json!({
                "event_kind": "agent_thread_title_refresh_attempted",
                "kind": "agent_thread_title_refresh_attempted",
                "agent_session_id": payload.agent_session_id,
                "discord_thread_id": payload.discord_thread_id,
                "response_count": payload.response_count,
                "refresh_job_id": job.id,
            }),
        )
        .await
        .map(|_| ())
}

async fn last_agent_thread_title_refresh_attempt_count(
    ctx: &Ctx,
    record: &AgentSessionRecord,
) -> Result<usize> {
    let mut count = 0usize;
    for event in ctx
        .store
        .load_events(
            &record.guild_id,
            &record.scope_id,
            None,
            None,
            None,
            None,
            false,
        )
        .await?
    {
        if first_value_string(&event, &["event_kind", "kind"])
            != "agent_thread_title_refresh_attempted"
            || first_value_string(&event, &["agent_session_id"]) != record.agent_session_id
        {
            continue;
        }
        count = count.max(usize_event_field(&event, "response_count"));
    }
    Ok(count)
}

async fn latest_agent_thread_title(
    ctx: &Ctx,
    record: &AgentSessionRecord,
) -> Result<Option<String>> {
    let mut latest = None::<(usize, String)>;
    for event in ctx
        .store
        .load_events(
            &record.guild_id,
            &record.scope_id,
            None,
            None,
            None,
            None,
            false,
        )
        .await?
    {
        if first_value_string(&event, &["event_kind", "kind"]) != "agent_thread_titled"
            || first_value_string(&event, &["agent_session_id"]) != record.agent_session_id
        {
            continue;
        }
        let title = first_value_string(&event, &["title"]);
        if title.trim().is_empty() {
            continue;
        }
        let response_count = usize_event_field(&event, "response_count");
        if latest
            .as_ref()
            .map(|(latest_count, _)| response_count >= *latest_count)
            .unwrap_or(true)
        {
            latest = Some((response_count, title));
        }
    }
    Ok(latest.map(|(_, title)| title))
}

pub fn build_agent_thread_title_prompt(context: &AgentThreadTitlePromptContext) -> Result<String> {
    render_configured_agent_thread_title_prompt(&agent_thread_title_template_vars(context))
}

pub fn build_agent_thread_title_prompt_from_template_dir(
    context: &AgentThreadTitlePromptContext,
    prompt_dir: &Path,
) -> Result<String> {
    render_agent_thread_title_prompt_from_dir(
        prompt_dir,
        &agent_thread_title_template_vars(context),
    )
}

pub fn sanitize_agent_thread_title(raw: &str) -> Result<String> {
    for line in raw.lines() {
        let title = line
            .trim()
            .trim_start_matches('#')
            .trim()
            .trim_matches('`')
            .trim_matches('"')
            .trim_matches('\'')
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if title.is_empty() {
            continue;
        }
        return Ok(title.chars().take(THREAD_TITLE_MAX_CHARS).collect());
    }
    anyhow::bail!("agent thread title response was empty")
}

fn agent_thread_title_template_vars(
    context: &AgentThreadTitlePromptContext,
) -> BTreeMap<String, String> {
    BTreeMap::from([
        (
            "agent_session_id".to_string(),
            context.agent_session_id.clone(),
        ),
        (
            "current_thread_title".to_string(),
            context.current_thread_title.clone(),
        ),
        (
            "voice_channel_name".to_string(),
            context.voice_channel_name.clone(),
        ),
        (
            "response_count".to_string(),
            context.response_count.to_string(),
        ),
        ("responses".to_string(), context.responses.join("\n\n")),
    ])
}

fn validate_thread_title_refresh_payload(
    job: &Job,
    payload: &AgentThreadTitleRefreshPayload,
) -> Result<()> {
    if job.id.trim().is_empty()
        || payload.agent_session_id.trim().is_empty()
        || payload.guild_id.trim().is_empty()
        || payload.voice_channel_id.trim().is_empty()
        || payload.discord_thread_id.trim().is_empty()
    {
        anyhow::bail!("agent thread title refresh job is missing required identity");
    }
    if payload.response_count < THREAD_TITLE_RESPONSE_INTERVAL {
        anyhow::bail!("agent thread title refresh requires at least one visible response");
    }
    Ok(())
}

fn delivery_is_visible_agent_thread_response(delivery: &Job, record: &AgentSessionRecord) -> bool {
    if delivery.state != JobState::Complete {
        return false;
    }
    matches!(
        delivery.metadata.output.as_ref(),
        Some(JobOutput::TextDelivery(output))
            if output.target.kind == TextTargetKind::Channel
                && output.target.channel_id == record.discord_thread_id
    )
}

fn text_delivery_content(job: &Job) -> String {
    match &job.payload {
        JobPayload::TextDelivery(payload) => payload.content.clone(),
        _ => String::new(),
    }
}

fn discord_forum_thread_rename_child_thread_id(child: &Job) -> Option<&str> {
    match &child.payload {
        JobPayload::DiscordForumThreadRename(payload) => Some(payload.thread_id.as_str()),
        _ => None,
    }
}

fn agent_thread_response_summary(index: usize, request: String, response: String) -> String {
    let request = compact_preview(&request, THREAD_TITLE_RESPONSE_PREVIEW_CHARS);
    let response = compact_preview(&response, THREAD_TITLE_RESPONSE_PREVIEW_CHARS);
    let mut parts = vec![format!("response {index}:")];
    if !request.is_empty() {
        parts.push(format!("request: {request}"));
    }
    if !response.is_empty() {
        parts.push(format!("response: {response}"));
    }
    parts.join("\n")
}

fn compact_preview(value: &str, limit: usize) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(limit)
        .collect()
}

fn usize_event_field(value: &Value, key: &str) -> usize {
    value
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(0)
}

fn agent_thread_title_workdir(agent_session_id: &str) -> PathBuf {
    config::agent_workspaces_root()
        .join("thread-title")
        .join(agent_session_id)
}

fn agent_thread_title_env(job: &Job) -> Result<BTreeMap<String, String>> {
    let mut vars = BTreeMap::new();
    vars.insert(
        "CODEX_HOME".to_string(),
        config::codex_home().display().to_string(),
    );
    vars.insert(
        "HOME".to_string(),
        config::codex_home().display().to_string(),
    );
    vars.insert("CLANKCORD_AGENT_JOB_ID".to_string(), job.id.clone());
    vars.insert("CLANKCORD_AGENT_GUILD_ID".to_string(), job.guild_id.clone());
    vars.insert("CLANKCORD_AGENT_SCOPE_ID".to_string(), job.scope_id.clone());
    insert_linear_mcp_env(&mut vars)?;
    Ok(vars)
}
