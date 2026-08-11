//! Instant and duration vocabulary: RFC3339 wall-clock strings, epoch
//! milliseconds, relative-time references, and local rendering. Every
//! layer tells time through here.

use std::collections::BTreeMap;

use chrono::{DateTime, SecondsFormat, TimeZone, Utc};
use regex::Regex;

pub fn utc_now() -> DateTime<Utc> {
    Utc::now()
}

pub fn isoformat_z(value: Option<DateTime<Utc>>) -> String {
    value
        .unwrap_or_else(utc_now)
        .to_rfc3339_opts(SecondsFormat::Millis, true)
}

pub fn format_timestamp_local(value: DateTime<Utc>, tz: chrono_tz::Tz) -> BTreeMap<String, String> {
    let local = value.with_timezone(&tz);
    let unix = value.timestamp();
    BTreeMap::from([
        (
            "iso".to_string(),
            value.to_rfc3339_opts(SecondsFormat::Millis, true),
        ),
        ("local_iso".to_string(), local.to_rfc3339()),
        ("discord_full".to_string(), format!("<t:{unix}:F>")),
        ("discord_relative".to_string(), format!("<t:{unix}:R>")),
        ("discord_short_time".to_string(), format!("<t:{unix}:T>")),
        (
            "display_date".to_string(),
            local.format("%Y-%m-%d").to_string(),
        ),
        (
            "display_time".to_string(),
            local.format("%H:%M:%S").to_string(),
        ),
        (
            "display_minute".to_string(),
            local.format("%H:%M").to_string(),
        ),
        (
            "display_started".to_string(),
            local.format("%Y-%m-%d %H:%M:%S %Z").to_string(),
        ),
        ("hour_slug".to_string(), local.format("%H").to_string()),
        ("minute_slug".to_string(), local.format("%H-%M").to_string()),
        (
            "day_path".to_string(),
            format!(
                "{:04}/{:02}/{:02}",
                chrono::Datelike::year(&local),
                chrono::Datelike::month(&local),
                chrono::Datelike::day(&local)
            ),
        ),
    ])
}

pub fn parse_instant(raw: &str) -> Option<DateTime<Utc>> {
    let value = raw.trim();
    if value.is_empty() {
        return None;
    }
    let normalized = if let Some(prefix) = value.strip_suffix('Z') {
        format!("{prefix}+00:00")
    } else {
        value.to_string()
    };
    DateTime::parse_from_rfc3339(&normalized)
        .map(|value| value.with_timezone(&Utc))
        .ok()
        .or_else(|| {
            chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S%.f")
                .ok()
                .map(|value| value.and_utc())
        })
}

pub fn parse_duration(raw: &str) -> Option<chrono::Duration> {
    let value = raw.trim().to_lowercase();
    if value.is_empty() {
        return None;
    }
    let regex = Regex::new(r"^(?P<sign>[+-])?\s*(?P<count>\d+(?:\.\d+)?)\s*(?P<unit>ms|s|sec|secs|m|min|mins|h|hr|hrs|d|day|days)$").ok()?;
    let captures = regex.captures(&value)?;
    let count: f64 = captures.name("count")?.as_str().parse().ok()?;
    let sign = if captures.name("sign").map(|m| m.as_str()) == Some("-") {
        -1.0
    } else {
        1.0
    };
    let unit = captures.name("unit")?.as_str();
    let millis = match unit {
        "ms" => count,
        "s" | "sec" | "secs" => count * 1000.0,
        "m" | "min" | "mins" => count * 60_000.0,
        "h" | "hr" | "hrs" => count * 3_600_000.0,
        _ => count * 86_400_000.0,
    };
    Some(chrono::Duration::milliseconds((sign * millis) as i64))
}

pub fn resolve_time_reference(raw: &str, now: Option<DateTime<Utc>>) -> Option<DateTime<Utc>> {
    let value = raw.trim();
    if value.is_empty() {
        return None;
    }
    let current = now.unwrap_or_else(utc_now);
    parse_duration(value)
        .map(|delta| current + delta)
        .or_else(|| parse_instant(value))
}

pub fn instant_ms_dt(value: DateTime<Utc>) -> i64 {
    value.timestamp_millis()
}

pub fn instant_ms_str(value: Option<&str>) -> Option<i64> {
    parse_instant(value.unwrap_or("")).map(instant_ms_dt)
}

pub fn ms_to_datetime(value: i64) -> Option<DateTime<Utc>> {
    Utc.timestamp_millis_opt(value).single()
}
