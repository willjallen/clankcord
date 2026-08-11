use crate::adapters::codex::CodexAdapter;

#[derive(Debug, Clone, Default)]
pub struct AgentRuntime {
    codex: CodexAdapter,
}

impl AgentRuntime {
    pub(crate) fn codex(&self) -> &CodexAdapter {
        &self.codex
    }
}
