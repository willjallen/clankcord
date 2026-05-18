use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::Context;

use crate::Result;
use crate::config;
use crate::runtime::{AgentSessionRouteKind, TextTargetKind};

const AGENT_THREAD_TITLE_TEMPLATE_FILE: &str = "agent-thread-title.md";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptStage {
    SessionBootstrap,
    AgentInvocation,
}

impl PromptStage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SessionBootstrap => "session_bootstrap",
            Self::AgentInvocation => "agent_invocation",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PromptSection {
    Base,
    ClankcordTools,
    ResponseContract,
    RuntimeWork,
    AgentTaskBase,
    AgentTaskLocalContext,
    AgentTaskResponseContract,
    AgentTaskRouteVoice,
    AgentTaskRouteDm,
    AgentTaskOriginText,
    AgentTaskOriginVoice,
    AgentTaskOriginPublicText,
}

impl PromptSection {
    fn stage(self) -> PromptStage {
        match self {
            Self::Base | Self::ClankcordTools | Self::ResponseContract | Self::RuntimeWork => {
                PromptStage::SessionBootstrap
            }
            Self::AgentTaskBase
            | Self::AgentTaskLocalContext
            | Self::AgentTaskResponseContract
            | Self::AgentTaskRouteVoice
            | Self::AgentTaskRouteDm
            | Self::AgentTaskOriginText
            | Self::AgentTaskOriginVoice
            | Self::AgentTaskOriginPublicText => PromptStage::AgentInvocation,
        }
    }

    fn file_name(self) -> &'static str {
        match self {
            Self::Base => "base.md",
            Self::ClankcordTools => "clankcord-tools.md",
            Self::ResponseContract => "response-contract.md",
            Self::RuntimeWork => "runtime-work.md",
            Self::AgentTaskBase => "agent-task-base.md",
            Self::AgentTaskLocalContext => "agent-task-local-context.md",
            Self::AgentTaskResponseContract => "agent-task-response-contract.md",
            Self::AgentTaskRouteVoice => "agent-task-route-voice.md",
            Self::AgentTaskRouteDm => "agent-task-route-dm.md",
            Self::AgentTaskOriginText => "agent-task-origin-text.md",
            Self::AgentTaskOriginVoice => "agent-task-origin-voice.md",
            Self::AgentTaskOriginPublicText => "agent-task-origin-public-text.md",
        }
    }
}

const SESSION_BOOTSTRAP_SECTIONS: &[PromptSection] = &[
    PromptSection::Base,
    PromptSection::ClankcordTools,
    PromptSection::ResponseContract,
    PromptSection::RuntimeWork,
];

const AGENT_INVOCATION_BASE_SECTIONS: &[PromptSection] = &[
    PromptSection::AgentTaskBase,
    PromptSection::AgentTaskLocalContext,
    PromptSection::AgentTaskResponseContract,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentPromptRequestOrigin {
    Voice,
    Text,
    Internal,
}

impl AgentPromptRequestOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Voice => "voice",
            Self::Text => "text",
            Self::Internal => "internal",
        }
    }
}

#[derive(Debug, Clone)]
pub struct AgentTaskPromptVars {
    pub job_id: String,
    pub agent_session_id: String,
    pub resumed_from_agent_session_id: String,
    pub route_kind: AgentSessionRouteKind,
    pub request_origin: AgentPromptRequestOrigin,
    pub response_surface: TextTargetKind,
    pub guild_id: String,
    pub scope_id: String,
    pub requested_by_user_id: String,
    pub requested_by: String,
    pub request: String,
    pub workdir: String,
    pub recent_scope_events: Vec<String>,
    pub source_request_events: Vec<String>,
}

pub fn render_configured_master_prompt() -> Result<String> {
    render_master_prompt_from_dir(&config::prompt_templates_dir())
}

pub fn render_configured_agent_task_prompt(vars: &AgentTaskPromptVars) -> Result<String> {
    render_agent_task_prompt_from_dir(&config::prompt_templates_dir(), vars)
}

pub fn render_configured_agent_thread_title_prompt(
    vars: &BTreeMap<String, String>,
) -> Result<String> {
    render_agent_thread_title_prompt_from_dir(&config::prompt_templates_dir(), vars)
}

pub fn render_master_prompt_from_dir(prompt_dir: &Path) -> Result<String> {
    render_prompt_sections(
        prompt_dir,
        PromptStage::SessionBootstrap,
        SESSION_BOOTSTRAP_SECTIONS,
        &BTreeMap::new(),
    )
}

pub fn render_agent_task_prompt_from_dir(
    prompt_dir: &Path,
    vars: &AgentTaskPromptVars,
) -> Result<String> {
    let mut sections = AGENT_INVOCATION_BASE_SECTIONS.to_vec();
    match vars.route_kind {
        AgentSessionRouteKind::Voice => sections.push(PromptSection::AgentTaskRouteVoice),
        AgentSessionRouteKind::Dm => sections.push(PromptSection::AgentTaskRouteDm),
        AgentSessionRouteKind::Thread => {}
    }
    match vars.request_origin {
        AgentPromptRequestOrigin::Text => {
            sections.push(PromptSection::AgentTaskOriginText);
            if vars.route_kind != AgentSessionRouteKind::Dm {
                sections.push(PromptSection::AgentTaskOriginPublicText);
            }
        }
        AgentPromptRequestOrigin::Voice => sections.push(PromptSection::AgentTaskOriginVoice),
        AgentPromptRequestOrigin::Internal => {}
    }
    render_prompt_sections(
        prompt_dir,
        PromptStage::AgentInvocation,
        &sections,
        &agent_task_template_vars(vars),
    )
}

pub fn render_agent_thread_title_prompt_from_dir(
    prompt_dir: &Path,
    vars: &BTreeMap<String, String>,
) -> Result<String> {
    render_prompt_file(prompt_dir, AGENT_THREAD_TITLE_TEMPLATE_FILE, vars)
}

fn render_prompt_file(
    prompt_dir: &Path,
    file_name: &str,
    vars: &BTreeMap<String, String>,
) -> Result<String> {
    let path = prompt_dir.join(file_name);
    let template = fs::read_to_string(&path)
        .with_context(|| format!("reading prompt template {}", path.display()))?;
    render_prompt_template(&template, vars, &path)
}

fn render_prompt_files(
    prompt_dir: &Path,
    file_names: &[&str],
    vars: &BTreeMap<String, String>,
) -> Result<String> {
    let mut sections = Vec::new();
    for file_name in file_names {
        sections.push(render_prompt_file(prompt_dir, file_name, vars)?);
    }
    Ok(sections.join("\n\n"))
}

fn render_prompt_sections(
    prompt_dir: &Path,
    expected_stage: PromptStage,
    sections: &[PromptSection],
    vars: &BTreeMap<String, String>,
) -> Result<String> {
    let file_names = sections
        .iter()
        .map(|section| {
            let actual_stage = section.stage();
            if actual_stage != expected_stage {
                anyhow::bail!(
                    "prompt section {} belongs to {} stage, not {} stage",
                    section.file_name(),
                    actual_stage.as_str(),
                    expected_stage.as_str()
                );
            }
            Ok(section.file_name())
        })
        .collect::<Result<Vec<_>>>()?;
    render_prompt_files(prompt_dir, &file_names, vars)
}

fn agent_task_template_vars(context: &AgentTaskPromptVars) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("job_id".to_string(), context.job_id.clone()),
        (
            "agent_session_id".to_string(),
            context.agent_session_id.clone(),
        ),
        (
            "resumed_from_agent_session_id".to_string(),
            context.resumed_from_agent_session_id.clone(),
        ),
        (
            "route_kind".to_string(),
            context.route_kind.as_str().to_string(),
        ),
        (
            "request_origin".to_string(),
            context.request_origin.as_str().to_string(),
        ),
        (
            "response_surface".to_string(),
            context.response_surface.as_str().to_string(),
        ),
        ("guild_id".to_string(), context.guild_id.clone()),
        ("scope_id".to_string(), context.scope_id.clone()),
        (
            "requested_by_user_id".to_string(),
            context.requested_by_user_id.clone(),
        ),
        ("requested_by".to_string(), context.requested_by.clone()),
        ("request".to_string(), context.request.clone()),
        ("workdir".to_string(), context.workdir.clone()),
        (
            "recent_scope_events".to_string(),
            context.recent_scope_events.join("\n"),
        ),
        (
            "source_request_events".to_string(),
            context.source_request_events.join("\n"),
        ),
    ])
}

fn render_prompt_template(
    template: &str,
    vars: &BTreeMap<String, String>,
    source: &Path,
) -> Result<String> {
    let mut rendered = String::with_capacity(template.len());
    let mut cursor = 0;
    while cursor < template.len() {
        let remaining = &template[cursor..];
        let Some(open_offset) = remaining.find("{{") else {
            if remaining.contains("}}") {
                anyhow::bail!(
                    "unmatched prompt template close marker in {}",
                    source.display()
                );
            }
            rendered.push_str(remaining);
            break;
        };
        let literal = &remaining[..open_offset];
        if literal.contains("}}") {
            anyhow::bail!(
                "unmatched prompt template close marker in {}",
                source.display()
            );
        }
        rendered.push_str(literal);

        let name_start = cursor + open_offset + 2;
        let after_open = &template[name_start..];
        let Some(close_offset) = after_open.find("}}") else {
            anyhow::bail!("unclosed prompt template variable in {}", source.display());
        };
        let name_end = name_start + close_offset;
        let name = template[name_start..name_end].trim();
        if name.is_empty() {
            anyhow::bail!("empty prompt template variable in {}", source.display());
        }
        let value = vars.get(name).with_context(|| {
            format!(
                "unknown prompt template variable `{name}` in {}",
                source.display()
            )
        })?;
        rendered.push_str(value);
        cursor = name_end + 2;
    }
    Ok(rendered)
}
