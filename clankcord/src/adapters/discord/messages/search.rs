use std::collections::{BTreeMap, BTreeSet};

use regex::Regex;
use serde_json::Value;

use crate::Result;
use crate::adapters::discord::api::{
    FORUM_CHANNEL_TYPE, GUILD_TEXT_CHANNEL_TYPES, get_channel, iter_channel_messages,
    list_active_guild_threads, list_forum_threads, list_guild_channels,
    list_public_archived_threads, string_field,
};
use crate::errors::discord_tool_error;

#[derive(Debug, clap::Args)]
pub struct Args {
    #[arg(long = "query")]
    pub query: Vec<String>,
    #[arg(long)]
    pub guild_id: Option<String>,
    #[arg(long = "channel-id")]
    pub channel_id: Vec<String>,
    #[arg(long = "forum-id")]
    pub forum_id: Vec<String>,
    #[arg(long = "author-id")]
    pub author_id: Vec<String>,
    #[arg(long)]
    pub author: Option<String>,
    #[arg(long)]
    pub limit: Option<usize>,
    #[arg(long)]
    pub json: bool,
}

pub fn normalize_queries(values: &[String]) -> Vec<String> {
    let whitespace = Regex::new(r"\s+").unwrap();
    let mut cleaned = Vec::new();
    let mut seen = BTreeSet::new();
    for value in values {
        let query = whitespace.replace_all(value, " ").trim().to_string();
        if query.is_empty() {
            continue;
        }
        if seen.insert(query.to_lowercase()) {
            cleaned.push(query);
        }
    }
    cleaned
}

pub fn message_matches_query(content: &str, queries: &[String]) -> bool {
    if queries.is_empty() {
        return true;
    }
    let haystack = content.to_lowercase();
    for query in queries {
        let needle = query.to_lowercase();
        if haystack.contains(&needle) {
            return true;
        }
        let terms = needle
            .split_whitespace()
            .filter(|term| !term.is_empty())
            .collect::<Vec<_>>();
        if !terms.is_empty() && terms.iter().all(|term| haystack.contains(term)) {
            return true;
        }
    }
    false
}

pub fn author_matches(
    message: &Value,
    author_ids: &BTreeSet<String>,
    author_query: Option<&str>,
) -> bool {
    if author_ids.is_empty() && author_query.is_none() {
        return true;
    }
    let author = message
        .get("author")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let author_id = string_field(&author, "id");
    if author_ids.contains(&author_id) {
        return true;
    }
    let Some(author_query) = author_query else {
        return false;
    };
    let wanted = author_query.to_lowercase();
    [
        string_field(&author, "username"),
        string_field(&author, "global_name"),
    ]
    .into_iter()
    .map(|value| value.to_lowercase())
    .any(|value| value == wanted)
}

pub fn build_channel_index(guild_id: &str) -> Result<BTreeMap<String, Value>> {
    Ok(list_guild_channels(guild_id)?
        .into_iter()
        .filter_map(|channel| {
            let id = string_field(&channel, "id");
            if id.is_empty() {
                None
            } else {
                Some((id, channel))
            }
        })
        .collect())
}

pub fn target_label(target: &Value) -> String {
    let parent_name = string_field(target, "parentName");
    let channel_name = {
        let name = string_field(target, "channelName");
        if name.is_empty() {
            string_field(target, "channelId")
        } else {
            name
        }
    };
    if parent_name.is_empty() {
        channel_name
    } else {
        format!("{parent_name} / {channel_name}")
    }
}

pub fn guild_targets(guild_id: &str) -> Result<Vec<Value>> {
    let index = build_channel_index(guild_id)?;
    let active_threads = list_active_guild_threads(guild_id)?;
    let mut targets = Vec::new();
    for channel in index.values() {
        let channel_id = string_field(channel, "id");
        let channel_type = channel.get("type").and_then(Value::as_i64).unwrap_or(-1);
        if channel_id.is_empty() {
            continue;
        }
        if GUILD_TEXT_CHANNEL_TYPES.contains(&channel_type) {
            let channel_name = non_empty(string_field(channel, "name"), channel_id.clone());
            targets.push(serde_json::json!({
                "channelId": channel_id,
                "channelName": channel_name,
                "channelType": "channel",
                "parentId": "",
                "parentName": ""
            }));
            let mut thread_candidates = active_threads
                .iter()
                .filter(|thread| string_field(thread, "parent_id") == channel_id)
                .cloned()
                .collect::<Vec<_>>();
            thread_candidates.extend(list_public_archived_threads(&channel_id)?);
            for thread in thread_candidates {
                let thread_id = string_field(&thread, "id");
                if thread_id.is_empty() {
                    continue;
                }
                targets.push(serde_json::json!({
                    "channelId": thread_id,
                    "channelName": non_empty(string_field(&thread, "name"), thread_id),
                    "channelType": "thread",
                    "parentId": channel_id,
                    "parentName": channel_name
                }));
            }
        } else if channel_type == FORUM_CHANNEL_TYPE {
            let forum_name = non_empty(string_field(channel, "name"), channel_id.clone());
            for thread in list_forum_threads(&channel_id, true)? {
                let thread_id = string_field(&thread, "id");
                if thread_id.is_empty() {
                    continue;
                }
                targets.push(serde_json::json!({
                    "channelId": thread_id,
                    "channelName": non_empty(string_field(&thread, "name"), thread_id),
                    "channelType": "thread",
                    "parentId": channel_id,
                    "parentName": forum_name
                }));
            }
        }
    }
    Ok(dedupe_targets(targets))
}

pub fn explicit_targets(channel_ids: &[String], forum_ids: &[String]) -> Result<Vec<Value>> {
    let mut targets = Vec::new();
    for channel_id in channel_ids {
        let channel = get_channel(channel_id)?;
        let parent_id = string_field(&channel, "parent_id");
        let parent_name = if parent_id.is_empty() {
            String::new()
        } else {
            string_field(&get_channel(&parent_id)?, "name")
        };
        targets.push(serde_json::json!({
            "channelId": channel_id,
            "channelName": non_empty(string_field(&channel, "name"), channel_id.clone()),
            "channelType": if parent_id.is_empty() { "channel" } else { "thread" },
            "parentId": parent_id,
            "parentName": parent_name
        }));
    }
    for forum_id in forum_ids {
        let forum = get_channel(forum_id)?;
        let forum_name = non_empty(string_field(&forum, "name"), forum_id.clone());
        for thread in list_forum_threads(forum_id, true)? {
            let thread_id = string_field(&thread, "id");
            if thread_id.is_empty() {
                continue;
            }
            targets.push(serde_json::json!({
                "channelId": thread_id,
                "channelName": non_empty(string_field(&thread, "name"), thread_id),
                "channelType": "thread",
                "parentId": forum_id,
                "parentName": forum_name
            }));
        }
    }
    Ok(dedupe_targets(targets))
}

pub fn scan_targets(
    targets: &[Value],
    queries: &[String],
    author_ids: &BTreeSet<String>,
    author_query: Option<&str>,
) -> Result<(Vec<Value>, usize)> {
    let mut hits = Vec::new();
    let mut messages_scanned = 0;
    for target in targets {
        for message in iter_channel_messages(&string_field(target, "channelId"), 100)? {
            messages_scanned += 1;
            let content = string_field(&message, "content");
            if content.trim().is_empty()
                || !message_matches_query(&content, queries)
                || !author_matches(&message, author_ids, author_query)
            {
                continue;
            }
            let author = message
                .get("author")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));
            hits.push(serde_json::json!({
                "channelId": string_field(target, "channelId"),
                "channelName": string_field(target, "channelName"),
                "channelLabel": target_label(target),
                "channelType": string_field(target, "channelType"),
                "parentId": string_field(target, "parentId"),
                "parentName": string_field(target, "parentName"),
                "messageId": string_field(&message, "id"),
                "timestamp": string_field(&message, "timestamp"),
                "authorId": string_field(&author, "id"),
                "authorUsername": string_field(&author, "username"),
                "authorDisplayName": string_field(&author, "global_name"),
                "content": content
            }));
        }
    }
    hits.sort_by_key(|hit| std::cmp::Reverse(string_field(hit, "timestamp")));
    Ok((hits, messages_scanned))
}

pub fn render_text(
    hits: &[Value],
    queries: &[String],
    total_matches: usize,
    targets_scanned: usize,
    messages_scanned: usize,
    display_limit: Option<usize>,
) -> String {
    let query_summary = match queries.len() {
        0 => "all messages".to_string(),
        1 => format!("matching \"{}\"", queries[0]),
        _ => format!(
            "matching any of: {}",
            queries
                .iter()
                .map(|query| format!("\"{query}\""))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    };
    if hits.is_empty() {
        return format!(
            "No Discord messages found {query_summary}. Scanned {targets_scanned} target(s) and {messages_scanned} message(s)."
        );
    }
    let summary = if display_limit.is_none() || total_matches <= display_limit.unwrap_or(usize::MAX)
    {
        format!(
            "Found {total_matches} Discord message(s) {query_summary}. Scanned {targets_scanned} target(s) and {messages_scanned} message(s)."
        )
    } else {
        format!(
            "Found {total_matches} Discord message(s) {query_summary}. Showing the first {} after scanning {targets_scanned} target(s) and {messages_scanned} message(s).",
            display_limit.unwrap()
        )
    };
    let mut lines = vec![summary, String::new()];
    for (index, hit) in hits.iter().enumerate() {
        lines.push(format!(
            "{}. **{}** ({})",
            index + 1,
            string_field(hit, "channelLabel"),
            string_field(hit, "channelId")
        ));
        lines.push(format!("   - Time: {}", string_field(hit, "timestamp")));
        let author = string_field(hit, "authorUsername");
        if !author.is_empty() {
            lines.push(format!("   - Author: {author}"));
        }
        lines.push(format!("   - Content: {}", string_field(hit, "content")));
        lines.push(String::new());
    }
    lines.join("\n").trim_end().to_string()
}

pub fn run(args: Args) -> Result<i32> {
    let queries = normalize_queries(&args.query);
    let author_ids = args
        .author_id
        .iter()
        .filter_map(|value| {
            let value = value.trim();
            if value.is_empty() {
                None
            } else {
                Some(value.to_string())
            }
        })
        .collect::<BTreeSet<_>>();
    let author = args
        .author
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if queries.is_empty() && author_ids.is_empty() && author.is_none() {
        return Err(discord_tool_error(
            "provide at least one --query, --author-id, or --author filter",
        ));
    }
    if args.guild_id.as_deref().unwrap_or("").is_empty()
        && args.channel_id.is_empty()
        && args.forum_id.is_empty()
    {
        return Err(discord_tool_error(
            "provide --guild-id, --channel-id, or --forum-id",
        ));
    }
    let targets = if let Some(guild_id) = args.guild_id.as_deref().filter(|value| !value.is_empty())
    {
        guild_targets(guild_id)?
    } else {
        explicit_targets(&args.channel_id, &args.forum_id)?
    };
    if targets.is_empty() {
        return Err(discord_tool_error(
            "no Discord channels or threads resolved for the requested search",
        ));
    }
    let (all_hits, messages_scanned) = scan_targets(&targets, &queries, &author_ids, author)?;
    let total_matches = all_hits.len();
    let display_limit = args.limit.filter(|limit| *limit > 0);
    let hits = display_limit
        .map(|limit| all_hits[..all_hits.len().min(limit)].to_vec())
        .unwrap_or(all_hits);
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "ok": true,
                "queries": queries,
                "authorIds": author_ids.into_iter().collect::<Vec<_>>(),
                "author": args.author.unwrap_or_default(),
                "count": hits.len(),
                "totalMatches": total_matches,
                "targetsScanned": targets.len(),
                "messagesScanned": messages_scanned,
                "displayLimit": display_limit.unwrap_or(0),
                "hits": hits
            }))?
        );
    } else {
        println!(
            "{}",
            render_text(
                &hits,
                &queries,
                total_matches,
                targets.len(),
                messages_scanned,
                display_limit
            )
        );
    }
    Ok(0)
}

fn dedupe_targets(targets: Vec<Value>) -> Vec<Value> {
    let mut deduped = BTreeMap::new();
    for target in targets {
        let id = string_field(&target, "channelId");
        if !id.is_empty() {
            deduped.insert(id, target);
        }
    }
    deduped.into_values().collect()
}

fn non_empty(value: String, fallback: String) -> String {
    if value.trim().is_empty() {
        fallback
    } else {
        value
    }
}
