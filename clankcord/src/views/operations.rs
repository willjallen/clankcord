//! Operator timeline and rooms surfaces: transcript events, the rooms status payload, and the automations rollup.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::{Postgres, QueryBuilder};

use crate::Result;
use crate::domain::Ctx;
use crate::domain::rooms::status;
use crate::model::automations::{AutomationRecord, AutomationTrigger};
use crate::store::timeline_event_payload;
use crate::time::{instant_ms_dt, isoformat_z, utc_now};
use crate::views::diagnostics::count_rows;
use crate::views::health::apply_voice_observation_freshness;
use crate::views::render::compact_dashboard_event;
use crate::views::search;

pub async fn dashboard_rooms_payload(ctx: &Ctx) -> Result<Value> {
    let now = utc_now();
    let mut status = status::status_payload(ctx, None).await?;
    if let Value::Object(object) = &mut status {
        object.insert(
            "liveOccupancy".to_string(),
            ctx.store.voice_occupancy_snapshot().await?,
        );
    }
    apply_voice_observation_freshness(ctx, &mut status, now).await?;
    Ok(json!({
        "generatedAt": isoformat_z(now),
        "status": status,
    }))
}

pub async fn recent_transcript_events(
    ctx: &Ctx,
    since: Option<DateTime<Utc>>,
    limit: usize,
    channel: &str,
    query: &str,
) -> Result<Vec<Value>> {
    let kinds = BTreeSet::from(["speech_segment".to_string(), "transcript".to_string()]);
    let channel = channel.trim();
    recent_events_by_kind_filtered(
        ctx,
        since,
        None,
        limit,
        Some(&kinds),
        query,
        (!channel.is_empty()).then_some(channel),
    )
    .await
}

async fn recent_events_by_kind_filtered(
    ctx: &Ctx,
    start: Option<DateTime<Utc>>,
    end: Option<DateTime<Utc>>,
    limit: usize,
    kinds: Option<&BTreeSet<String>>,
    query: &str,
    channel: Option<&str>,
) -> Result<Vec<Value>> {
    if kinds.is_some_and(BTreeSet::is_empty) {
        return Ok(Vec::new());
    }

    let mut statement = QueryBuilder::<Postgres>::new(
        r#"
            WITH selected_events AS MATERIALIZED (
            SELECT e.sequence, e.started_at_ms, e.event_id
            FROM timeline_events e
            "#,
    );
    if !query.trim().is_empty() {
        statement.push(
            r#"
            LEFT JOIN voice_rooms r
              ON e.scope_kind = 'voice_channel'
             AND r.guild_id = e.guild_id
             AND r.voice_channel_id = e.scope_id
                "#,
        );
        search::push_event_member_joins(&mut statement, true, true);
    }
    statement.push(
        r#"
            WHERE e.forgotten = FALSE
            "#,
    );
    if let Some(start) = start {
        statement
            .push(" AND e.ended_at_ms > ")
            .push_bind(instant_ms_dt(start));
    }
    if let Some(end) = end {
        statement
            .push(" AND e.started_at_ms < ")
            .push_bind(instant_ms_dt(end));
    }
    if let Some(kinds) = kinds {
        statement.push(" AND e.event_kind IN (");
        let mut separated = statement.separated(", ");
        for kind in kinds {
            separated.push_bind(kind);
        }
        separated.push_unseparated(")");
    }
    if let Some(channel) = channel {
        statement.push(" AND e.scope_id = ").push_bind(channel);
    }
    search::push_event_search(&mut statement, query, search::SearchField::All);
    statement
        .push(" ORDER BY e.started_at_ms DESC, e.sequence DESC, e.event_id DESC LIMIT ")
        .push_bind(limit as i64)
        .push(
            r#"
            )
            SELECT e.*,
                   r.guild_slug AS room_guild_slug,
                   r.voice_channel_name AS room_voice_channel_name,
                   r.voice_channel_slug AS room_voice_channel_slug
            FROM selected_events selected
            JOIN timeline_events e ON e.sequence = selected.sequence
            LEFT JOIN voice_rooms r
              ON e.scope_kind = 'voice_channel'
             AND r.guild_id = e.guild_id
             AND r.voice_channel_id = e.scope_id
            ORDER BY selected.started_at_ms DESC, selected.sequence DESC, selected.event_id DESC
                "#,
        );

    let rows = statement.build().fetch_all(&ctx.store.pool).await?;
    rows.iter()
        .map(timeline_event_payload)
        .map(|event| event.map(compact_dashboard_event))
        .collect()
}

pub(super) fn automation_dashboard_payload(records: &[AutomationRecord]) -> Value {
    let mut by_state = BTreeMap::<String, usize>::new();
    let mut by_trigger = BTreeMap::<String, usize>::new();
    let mut active = 0_usize;
    let mut fired = 0_usize;
    for record in records {
        let state = format!("{:?}", record.state).to_lowercase();
        *by_state.entry(state.clone()).or_insert(0) += 1;
        if state == "active" {
            active += 1;
        }
        if record.fire_count > 0 {
            fired += 1;
        }
        *by_trigger
            .entry(automation_trigger_kind(&record.spec.trigger).to_string())
            .or_insert(0) += 1;
    }
    json!({
        "records": records.iter().map(AutomationRecord::to_json).collect::<Vec<_>>(),
        "summary": {
            "total": records.len(),
            "active": active,
            "fired": fired,
            "byState": count_rows(by_state, "state"),
            "byTrigger": count_rows(by_trigger, "trigger"),
        },
    })
}

fn automation_trigger_kind(trigger: &AutomationTrigger) -> &'static str {
    match trigger {
        AutomationTrigger::Tick { .. } => "tick",
        AutomationTrigger::Event { .. } => "event",
        AutomationTrigger::Job { .. } => "job",
        AutomationTrigger::RoomStateChanged => "room_state_changed",
    }
}
