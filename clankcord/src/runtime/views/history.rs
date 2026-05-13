use std::collections::BTreeSet;

use serde_json::{Map, Value, json};

use crate::Result;
use crate::config::{non_empty, string_field};
use crate::errors::discord_tool_error;
use crate::runtime::timeline::{
    TimelineStore, isoformat_z, parse_instant, resolve_time_reference, utc_now,
};

use crate::runtime::Runtime;
use crate::runtime::util::first_non_empty;

#[derive(Debug, Clone, Default)]
pub struct TimelineTailRequest {
    pub guild_id: String,
    pub channel_id: String,
    pub since: String,
}

#[derive(Debug, Clone, Default)]
pub struct TimelineRangeRequest {
    pub guild_id: String,
    pub channel_id: String,
    pub from: String,
    pub to: String,
    pub all_channels: bool,
}

#[derive(Debug, Clone, Default)]
pub struct MaterializeTranscriptRequest {
    pub guild_id: String,
    pub channel_id: String,
    pub since: String,
    pub from: String,
    pub to: String,
    pub publish: String,
    pub live: bool,
    pub refine: bool,
    pub created_by_user_id: String,
    pub parent_job_id: String,
}

#[derive(Debug, Clone, Default)]
pub struct RenderTranscriptRequest {
    pub window_id: String,
    pub guild_id: String,
    pub channel_id: String,
    pub since: String,
    pub from: String,
    pub to: String,
    pub prefer_refined: bool,
    pub format: String,
}

#[derive(Debug, Clone)]
pub struct SearchTranscriptsRequest {
    pub guild_id: String,
    pub channel_id: String,
    pub all_channels: bool,
    pub query: String,
    pub since: String,
    pub prefer_refined: bool,
    pub limit: usize,
}

impl Default for SearchTranscriptsRequest {
    fn default() -> Self {
        Self {
            guild_id: String::new(),
            channel_id: String::new(),
            all_channels: false,
            query: String::new(),
            since: "-7d".to_string(),
            prefer_refined: true,
            limit: 50,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ListConversationsRequest {
    pub guild_id: String,
    pub channel_id: String,
    pub all_channels: bool,
    pub since: String,
}

impl Default for ListConversationsRequest {
    fn default() -> Self {
        Self {
            guild_id: String::new(),
            channel_id: String::new(),
            all_channels: false,
            since: "-2d".to_string(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ParticipantTraceRequest {
    pub guild_id: String,
    pub user_id: String,
    pub from: String,
    pub to: String,
    pub include_speech_snippets: bool,
}

#[derive(Debug, Clone, Default)]
pub struct ContextResolveRequest {
    pub guild_id: String,
    pub channel_id: String,
    pub reference: String,
}

#[derive(Debug, Clone)]
pub struct ForgetRequest {
    pub window_id: String,
    pub guild_id: String,
    pub channel_id: String,
    pub since: String,
    pub to: String,
    pub requested_by_user_id: String,
    pub unpublished_only: bool,
}

impl Default for ForgetRequest {
    fn default() -> Self {
        Self {
            window_id: String::new(),
            guild_id: String::new(),
            channel_id: String::new(),
            since: "-10m".to_string(),
            to: String::new(),
            requested_by_user_id: String::new(),
            unpublished_only: true,
        }
    }
}

impl Runtime {
    pub fn timeline_tail(&self, request: TimelineTailRequest) -> Result<Value> {
        let guild_id = request.guild_id;
        let channel_id = request.channel_id;
        let room = if guild_id.is_empty() || channel_id.is_empty() {
            self.room_for_identifier(if channel_id.is_empty() {
                None
            } else {
                Some(&channel_id)
            })?
        } else {
            self.resolve_room_scope(&guild_id, Some(&channel_id))?
        };
        let now = utc_now();
        let start = resolve_time_reference(&non_empty(request.since, "-1h".to_string()), Some(now))
            .unwrap_or_else(|| now - chrono::Duration::hours(1));
        let events = self.timeline_store.load_events(
            &room.guild_id,
            &room.channel_id,
            Some(start),
            None,
            None,
            None,
            false,
        )?;
        Ok(
            json!({"guildId": room.guild_id, "channelId": room.channel_id, "since": isoformat_z(Some(start)), "events": events}),
        )
    }

    pub fn timeline_range(&self, request: TimelineRangeRequest) -> Result<Value> {
        let guild_id = request.guild_id;
        let start = resolve_time_reference(&request.from, None)
            .ok_or_else(|| discord_tool_error("guild and from are required"))?;
        let end = resolve_time_reference(&request.to, None).unwrap_or_else(utc_now);
        if guild_id.is_empty() {
            return Err(discord_tool_error("guild and from are required"));
        }
        let channel_id = request.channel_id;
        let all_channels = request.all_channels;
        let mut channels = Vec::new();
        for dir in self.timeline_store.channel_dirs(
            &guild_id,
            if all_channels {
                None
            } else {
                Some(&channel_id)
            },
        )? {
            let current_channel_id = TimelineStore::channel_id_from_dir(&dir);
            let events = self.timeline_store.load_events(
                &guild_id,
                &current_channel_id,
                Some(start),
                Some(end),
                None,
                None,
                false,
            )?;
            channels.push(json!({"voice_channel_id": current_channel_id, "events": events}));
        }
        Ok(
            json!({"guildId": guild_id, "from": isoformat_z(Some(start)), "to": isoformat_z(Some(end)), "channels": channels}),
        )
    }

    pub fn materialize_transcript(&self, request: MaterializeTranscriptRequest) -> Result<Value> {
        let mut guild_id = request.guild_id;
        let mut channel_id = request.channel_id;
        if !guild_id.is_empty() && !channel_id.is_empty() {
            let room = self.resolve_room_scope(&guild_id, Some(&channel_id))?;
            guild_id = room.guild_id;
            channel_id = room.channel_id;
        } else {
            let room = self.room_for_identifier(if channel_id.is_empty() {
                None
            } else {
                Some(&channel_id)
            })?;
            guild_id = room.guild_id;
            channel_id = room.channel_id;
        }
        let now = utc_now();
        let has_since = !request.since.trim().is_empty();
        let start_raw = first_non_empty([request.since, request.from]);
        let start = resolve_time_reference(&start_raw, Some(now))
            .unwrap_or_else(|| now - chrono::Duration::minutes(10));
        let end = resolve_time_reference(&request.to, Some(now)).unwrap_or(now);
        let publish = non_empty(request.publish, "local".to_string());
        let mut result = self.timeline_store.materialize(
            &guild_id,
            &channel_id,
            start,
            end,
            if has_since {
                "relative_time"
            } else {
                "absolute_time_range"
            },
            &non_empty(start_raw, "last 10 minutes".to_string()),
            &request.created_by_user_id,
            &publish,
            request.live,
            request.refine,
            true,
            if request.parent_job_id.trim().is_empty() {
                None
            } else {
                Some(request.parent_job_id.as_str())
            },
        )?;
        if publish == "discord" {
            self.publish_materialized_transcript(&mut result, request.live, request.refine)?;
        }
        Ok(result)
    }
    pub fn render_transcript(&self, request: RenderTranscriptRequest) -> Result<Value> {
        let window_id = request.window_id;
        let (window, guild_id, channel_id, start, end) = if !window_id.is_empty() {
            let window = self.timeline_store.get_window(&window_id)?;
            let guild_id = string_field(&window, "guild_id");
            let channel_id = string_field(&window, "voice_channel_id");
            let start = parse_instant(&string_field(&window, "start_time"))
                .ok_or_else(|| discord_tool_error("invalid transcript window start"))?;
            let end = parse_instant(&string_field(&window, "end_time"))
                .ok_or_else(|| discord_tool_error("invalid transcript window end"))?;
            (window, guild_id, channel_id, start, end)
        } else {
            let guild_id = request.guild_id;
            let channel_id = request.channel_id;
            let room = self.resolve_room_scope(&guild_id, Some(&channel_id))?;
            let now = utc_now();
            let start = resolve_time_reference(
                &first_non_empty([request.since, request.from, "-1h".to_string()]),
                Some(now),
            )
            .ok_or_else(|| discord_tool_error("invalid transcript start"))?;
            let end = resolve_time_reference(&request.to, Some(now)).unwrap_or(now);
            (
                Value::Object(Map::new()),
                room.guild_id,
                room.channel_id,
                start,
                end,
            )
        };
        let rendered = self.timeline_store.render_transcript(
            &guild_id,
            &channel_id,
            start,
            end,
            &window_id,
            request.prefer_refined,
            &non_empty(request.format, "markdown".to_string()),
        )?;
        Ok(json!({
            "window": if window.is_object() && window.as_object().is_some_and(|map| map.is_empty()) { rendered.window } else { window },
            "content": rendered.content,
            "events": rendered.events,
            "authoritativeSpans": rendered.spans,
        }))
    }

    pub fn search_transcripts(&self, request: SearchTranscriptsRequest) -> Result<Value> {
        let mut guild_id = request.guild_id;
        let mut channel_id = request.channel_id;
        let all_channels = request.all_channels;
        if guild_id.is_empty() && !channel_id.is_empty() {
            let room = self.resolve_room_scope("", Some(&channel_id))?;
            guild_id = room.guild_id;
            channel_id = room.channel_id;
        }
        if guild_id.is_empty() {
            return Err(discord_tool_error("guild is required"));
        }
        if !channel_id.is_empty() && !all_channels {
            let room = self.resolve_room_scope(&guild_id, Some(&channel_id))?;
            guild_id = room.guild_id;
            channel_id = room.channel_id;
        }
        let query = request.query;
        let since = resolve_time_reference(&non_empty(request.since, "-7d".to_string()), None);
        let limit = request.limit;
        let hits = self.timeline_store.search(
            &guild_id,
            if all_channels || channel_id.is_empty() {
                None
            } else {
                Some(&channel_id)
            },
            &query,
            since,
            request.prefer_refined,
            limit,
        )?;
        Ok(json!({"guildId": guild_id, "query": query, "count": hits.len(), "hits": hits}))
    }

    pub fn list_conversations(&self, request: ListConversationsRequest) -> Result<Value> {
        let mut guild_id = request.guild_id;
        let mut channel_id = request.channel_id;
        let all_channels = request.all_channels;
        if guild_id.is_empty() && !channel_id.is_empty() {
            let room = self.room_for_identifier(Some(&channel_id))?;
            guild_id = room.guild_id;
            channel_id = room.channel_id;
        }
        if guild_id.is_empty() {
            return Err(discord_tool_error("guild is required"));
        }
        let since = resolve_time_reference(&non_empty(request.since, "-2d".to_string()), None);
        let conversations = self.timeline_store.list_conversations(
            &guild_id,
            if all_channels || channel_id.is_empty() {
                None
            } else {
                Some(&channel_id)
            },
            since,
        )?;
        Ok(
            json!({"guildId": guild_id, "count": conversations.len(), "conversations": conversations}),
        )
    }

    pub fn participant_trace(&self, request: ParticipantTraceRequest) -> Result<Value> {
        let guild_id = request.guild_id;
        let user_id = request.user_id;
        let start = resolve_time_reference(&request.from, None)
            .ok_or_else(|| discord_tool_error("guild, user, and from are required"))?;
        let end = resolve_time_reference(&request.to, None).unwrap_or_else(utc_now);
        if guild_id.is_empty() || user_id.is_empty() {
            return Err(discord_tool_error("guild, user, and from are required"));
        }
        let trace = self.timeline_store.participant_trace(
            &guild_id,
            &user_id,
            start,
            end,
            request.include_speech_snippets,
        )?;
        Ok(json!({"guildId": guild_id, "userId": user_id, "count": trace.len(), "trace": trace}))
    }

    pub fn context_resolve(&self, request: ContextResolveRequest) -> Result<Value> {
        let guild_id = request.guild_id;
        let channel_id = request.channel_id;
        let reference = request.reference;
        if guild_id.is_empty() || channel_id.is_empty() || reference.is_empty() {
            return Err(discord_tool_error(
                "guild, channel, and reference are required",
            ));
        }
        let room = self.resolve_room_scope(&guild_id, Some(&channel_id))?;
        let now = utc_now();
        let lowered = reference.to_lowercase();
        if lowered.contains("just said") || lowered.contains("last thing") {
            let kinds = BTreeSet::from(["speech_segment".to_string(), "transcript".to_string()]);
            let events = self.timeline_store.load_events(
                &room.guild_id,
                &room.channel_id,
                Some(now - chrono::Duration::minutes(5)),
                None,
                Some(&kinds),
                None,
                false,
            )?;
            if let Some(event) = events.last() {
                return Ok(
                    json!({"resolution": "recent_speaker_turn", "confidence": 0.78, "event": event, "reference": reference}),
                );
            }
        }
        let (start, confidence) = if lowered.contains("hour ago") {
            (now - chrono::Duration::hours(1), 0.72)
        } else {
            (now - chrono::Duration::minutes(10), 0.35)
        };
        let window = self.timeline_store.create_window(
            &room.guild_id,
            &room.channel_id,
            start,
            now,
            "context_reference",
            &reference,
            "single_channel",
        )?;
        Ok(
            json!({"resolution": "fallback_window", "confidence": confidence, "window": window, "reference": reference}),
        )
    }
    pub fn forget(&self, request: ForgetRequest) -> Result<Value> {
        let window_id = request.window_id;
        let (guild_id, channel_id, start, end) = if !window_id.is_empty() {
            let window = self.timeline_store.get_window(&window_id)?;
            (
                string_field(&window, "guild_id"),
                string_field(&window, "voice_channel_id"),
                parse_instant(&string_field(&window, "start_time"))
                    .ok_or_else(|| discord_tool_error("invalid forget window"))?,
                parse_instant(&string_field(&window, "end_time"))
                    .ok_or_else(|| discord_tool_error("invalid forget window"))?,
            )
        } else {
            let guild_id = request.guild_id;
            let channel_id = request.channel_id;
            let now = utc_now();
            (
                guild_id,
                channel_id,
                resolve_time_reference(&non_empty(request.since, "-10m".to_string()), Some(now))
                    .ok_or_else(|| discord_tool_error("invalid forget start"))?,
                resolve_time_reference(&request.to, Some(now)).unwrap_or(now),
            )
        };
        if guild_id.is_empty() || channel_id.is_empty() {
            return Err(discord_tool_error("invalid forget window"));
        }
        self.timeline_store.apply_forget(
            &guild_id,
            &channel_id,
            start,
            end,
            &request.requested_by_user_id,
            request.unpublished_only,
        )
    }
}
