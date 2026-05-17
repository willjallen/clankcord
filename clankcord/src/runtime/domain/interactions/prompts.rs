use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::Context;

use crate::Result;
use crate::config;

const MASTER_TEMPLATE_FILE: &str = "master.md";
const AGENT_TASK_TEMPLATE_FILE: &str = "agent-task.md";

pub fn render_configured_master_prompt() -> Result<String> {
    render_master_prompt_from_dir(&config::prompt_templates_dir())
}

pub fn render_configured_agent_task_prompt(vars: &BTreeMap<String, String>) -> Result<String> {
    render_agent_task_prompt_from_dir(&config::prompt_templates_dir(), vars)
}

pub fn render_master_prompt_from_dir(prompt_dir: &Path) -> Result<String> {
    render_prompt_file(prompt_dir, MASTER_TEMPLATE_FILE, &BTreeMap::new())
}

pub fn render_agent_task_prompt_from_dir(
    prompt_dir: &Path,
    vars: &BTreeMap<String, String>,
) -> Result<String> {
    render_prompt_file(prompt_dir, AGENT_TASK_TEMPLATE_FILE, vars)
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
