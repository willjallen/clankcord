use serde_json::Value;

use crate::Result;
use crate::adapters::discord::api::{get_channel, iter_channel_messages, string_field};
use crate::errors::discord_tool_error;

#[derive(Debug, clap::Args)]
pub struct Args {
    #[arg(long)]
    pub target: String,
    #[arg(long, default_value_t = 50)]
    pub limit: usize,
    #[arg(long)]
    pub json: bool,
    #[arg(long, default_value = "json")]
    pub format: String,
    #[arg(long)]
    pub file: Option<String>,
}

pub fn parse_target(raw: &str) -> Result<String> {
    let mut value = raw.trim().to_string();
    if let Some((kind, channel_id)) = value.split_once(':') {
        if !matches!(kind, "channel" | "thread") {
            return Err(discord_tool_error(format!(
                "unsupported target kind: {kind}"
            )));
        }
        value = channel_id.to_string();
    }
    if value.trim().is_empty() {
        Err(discord_tool_error("target channel id is empty"))
    } else {
        Ok(value.trim().to_string())
    }
}

pub fn format_text(channel: &Value, messages: &[Value]) -> String {
    let channel_name = {
        let name = string_field(channel, "name");
        if name.is_empty() {
            string_field(channel, "id")
        } else {
            name
        }
    };
    let mut lines = vec![
        "# Discord Messages".to_string(),
        String::new(),
        format!("- Channel: {channel_name}"),
        format!("- Channel ID: {}", string_field(channel, "id")),
        String::new(),
    ];
    if messages.is_empty() {
        lines.push("No messages found.".to_string());
        return lines.join("\n");
    }
    for message in messages {
        let author = message
            .get("author")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));
        let author_name = first_non_empty([
            string_field(&author, "global_name"),
            string_field(&author, "username"),
            string_field(&author, "id"),
            "unknown".to_string(),
        ]);
        lines.push(format!(
            "## {} {author_name}",
            string_field(message, "timestamp")
        ));
        lines.push(string_field(message, "content").trim_end().to_string());
        lines.push(String::new());
    }
    lines.join("\n").trim_end().to_string()
}

pub fn run(args: Args) -> Result<i32> {
    let channel_id = parse_target(&args.target)?;
    let channel = get_channel(&channel_id)?;
    let mut messages = iter_channel_messages(&channel_id, 100)?;
    let limit = if args.limit > 0 { args.limit } else { 50 };
    messages.truncate(limit);
    messages.reverse();
    let payload = serde_json::json!({
        "ok": true,
        "target": args.target,
        "channel": channel,
        "count": messages.len(),
        "messages": messages
    });
    if args.json || args.file.is_some() || args.format == "json" {
        emit_json(payload, &args.format, args.file.as_deref())?;
    } else {
        println!(
            "{}",
            format_text(
                payload.get("channel").unwrap(),
                payload.get("messages").and_then(Value::as_array).unwrap()
            )
        );
    }
    Ok(0)
}

fn emit_json(payload: Value, format: &str, file: Option<&str>) -> Result<()> {
    if format.trim() != "json" {
        return Err(discord_tool_error(
            "--format json is the only supported format",
        ));
    }
    let rendered = serde_json::to_string_pretty(&payload)?;
    if let Some(path) = file.map(str::trim).filter(|value| !value.is_empty()) {
        std::fs::write(path, format!("{rendered}\n"))?;
        println!("Wrote JSON to {path}");
        println!(
            "Records: {}",
            payload.get("count").and_then(Value::as_u64).unwrap_or(0)
        );
    } else {
        println!("{rendered}");
    }
    Ok(())
}

fn first_non_empty<const N: usize>(values: [String; N]) -> String {
    values
        .into_iter()
        .find(|value| !value.trim().is_empty())
        .unwrap_or_default()
}
