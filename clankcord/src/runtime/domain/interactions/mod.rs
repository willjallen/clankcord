pub mod agent_sessions;
pub mod commands;
pub(crate) mod confirmations;
mod linear_mcp;
mod policy;
pub(crate) mod prompts;
pub(crate) mod tasks;
pub(crate) mod thread_titles;

pub use policy::requires_confirmation;
pub use prompts::AgentPromptRequestOrigin;
pub use tasks::{
    AgentTaskPromptContext, agent_invocation_infrastructure_failure,
    agent_invocation_warning_event_kind, build_agent_task_message,
    build_agent_task_message_for_session, build_agent_task_message_from_template_dir,
};
pub use thread_titles::{
    AgentThreadTitlePromptContext, build_agent_thread_title_prompt,
    build_agent_thread_title_prompt_from_template_dir, sanitize_agent_thread_title,
};
