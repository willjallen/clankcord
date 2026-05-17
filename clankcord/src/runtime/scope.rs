use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::str::FromStr;

use crate::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum RuntimeScopeKind {
    VoiceChannel,
    Dm,
    TextChannel,
    Thread,
    Runtime,
}

impl RuntimeScopeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::VoiceChannel => "voice_channel",
            Self::Dm => "dm",
            Self::TextChannel => "text_channel",
            Self::Thread => "thread",
            Self::Runtime => "runtime",
        }
    }
}

impl FromStr for RuntimeScopeKind {
    type Err = anyhow::Error;

    fn from_str(raw: &str) -> Result<Self> {
        match raw.trim() {
            "voice_channel" => Ok(Self::VoiceChannel),
            "dm" => Ok(Self::Dm),
            "text_channel" => Ok(Self::TextChannel),
            "thread" => Ok(Self::Thread),
            "runtime" => Ok(Self::Runtime),
            value => anyhow::bail!("unknown runtime scope kind: {value}"),
        }
    }
}

impl Default for RuntimeScopeKind {
    fn default() -> Self {
        Self::Runtime
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RuntimeScope {
    pub kind: RuntimeScopeKind,
    pub guild_id: String,
    pub scope_id: String,
}

impl RuntimeScope {
    pub fn voice_channel(guild_id: impl Into<String>, voice_channel_id: impl Into<String>) -> Self {
        Self {
            kind: RuntimeScopeKind::VoiceChannel,
            guild_id: guild_id.into(),
            scope_id: voice_channel_id.into(),
        }
    }

    pub fn dm(user_id: impl Into<String>) -> Self {
        Self {
            kind: RuntimeScopeKind::Dm,
            guild_id: String::new(),
            scope_id: user_id.into(),
        }
    }

    pub fn text_channel(guild_id: impl Into<String>, channel_id: impl Into<String>) -> Self {
        Self {
            kind: RuntimeScopeKind::TextChannel,
            guild_id: guild_id.into(),
            scope_id: channel_id.into(),
        }
    }

    pub fn thread(guild_id: impl Into<String>, thread_id: impl Into<String>) -> Self {
        Self {
            kind: RuntimeScopeKind::Thread,
            guild_id: guild_id.into(),
            scope_id: thread_id.into(),
        }
    }

    pub fn runtime() -> Self {
        Self {
            kind: RuntimeScopeKind::Runtime,
            guild_id: String::new(),
            scope_id: "runtime".to_string(),
        }
    }

    pub fn is_voice_channel(&self) -> bool {
        self.kind == RuntimeScopeKind::VoiceChannel
    }

    pub fn to_json(&self) -> Value {
        let mut object = Map::new();
        object.insert(
            "scope_kind".to_string(),
            Value::String(self.kind.as_str().to_string()),
        );
        if !self.guild_id.trim().is_empty() {
            object.insert("guild_id".to_string(), Value::String(self.guild_id.clone()));
        }
        object.insert("scope_id".to_string(), Value::String(self.scope_id.clone()));
        if self.kind == RuntimeScopeKind::VoiceChannel {
            object.insert(
                "voice_channel_id".to_string(),
                Value::String(self.scope_id.clone()),
            );
        }
        Value::Object(object)
    }
}
