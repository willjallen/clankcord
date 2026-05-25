use std::fmt::{Display, Formatter};

#[derive(Debug, Clone)]
pub struct DiscordToolError {
    message: String,
    method: String,
    path: String,
    status_code: Option<u16>,
    discord_code: Option<i64>,
    detail: String,
}

impl DiscordToolError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            method: String::new(),
            path: String::new(),
            status_code: None,
            discord_code: None,
            detail: String::new(),
        }
    }

    pub fn api(
        method: impl Into<String>,
        path: impl Into<String>,
        status_code: u16,
        body: impl Into<String>,
    ) -> Self {
        let method = method.into();
        let path = path.into();
        let body = body.into();
        let detail = body.split_whitespace().collect::<Vec<_>>().join(" ");
        let discord_code = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|value| value.get("code").and_then(serde_json::Value::as_i64));
        let message = format!(
            "discord api {method} {path} failed ({status_code}): {}",
            detail.chars().take(500).collect::<String>()
        );
        Self {
            message,
            method,
            path,
            status_code: Some(status_code),
            discord_code,
            detail,
        }
    }

    pub fn is_unavailable_channel(&self) -> bool {
        self.path.starts_with("/channels/")
            && matches!(self.discord_code, Some(10003 | 50001 | 50013))
    }

    pub fn channel_id(&self) -> String {
        discord_channel_id_from_path(&self.path)
    }

    pub fn status_code(&self) -> Option<u16> {
        self.status_code
    }

    pub fn discord_code(&self) -> Option<i64> {
        self.discord_code
    }

    pub fn method(&self) -> &str {
        &self.method
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn detail(&self) -> &str {
        &self.detail
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

pub fn discord_api_error(
    method: impl Into<String>,
    path: impl Into<String>,
    status_code: u16,
    body: impl Into<String>,
) -> anyhow::Error {
    anyhow::Error::new(DiscordToolError::api(method, path, status_code, body))
}

pub fn discord_error_is_unavailable_channel(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<DiscordToolError>()
        .map(DiscordToolError::is_unavailable_channel)
        .unwrap_or_else(|| discord_error_text_is_unavailable_channel(&error.to_string()))
}

pub fn discord_error_channel_id(error: &anyhow::Error) -> String {
    error
        .downcast_ref::<DiscordToolError>()
        .map(DiscordToolError::channel_id)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| discord_error_text_channel_id(&error.to_string()))
}

pub fn discord_error_text_is_unavailable_channel(text: &str) -> bool {
    let compact = text
        .to_ascii_lowercase()
        .split_whitespace()
        .collect::<String>();
    if !compact.contains("/channels/") {
        return false;
    }
    if let Some(code) = discord_error_text_code(text) {
        return matches!(code, 10003 | 50001 | 50013);
    }
    compact.contains("unknownchannel")
        || compact.contains("missingaccess")
        || compact.contains("missingpermissions")
}

pub fn discord_error_text_channel_id(text: &str) -> String {
    text.find("/channels/")
        .map(|index| {
            text[index + "/channels/".len()..]
                .chars()
                .take_while(|character| character.is_ascii_digit())
                .collect::<String>()
        })
        .unwrap_or_default()
}

fn discord_channel_id_from_path(path: &str) -> String {
    path.strip_prefix("/channels/")
        .map(|rest| {
            rest.chars()
                .take_while(|character| character.is_ascii_digit())
                .collect::<String>()
        })
        .unwrap_or_default()
}

fn discord_error_text_code(text: &str) -> Option<i64> {
    text.find('{')
        .and_then(|index| serde_json::from_str::<serde_json::Value>(&text[index..]).ok())
        .and_then(|value| value.get("code").and_then(serde_json::Value::as_i64))
}
