use std::fmt::{Display, Formatter};

#[derive(Debug, Clone)]
pub struct DiscordToolError {
    message: String,
}

impl DiscordToolError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl Display for DiscordToolError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for DiscordToolError {}

pub fn discord_tool_error(message: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(DiscordToolError::new(message))
}
