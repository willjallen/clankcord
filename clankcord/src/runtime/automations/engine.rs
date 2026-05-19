use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use anyhow::Context;
use serde_json::{Value, json};

use crate::Result;
use crate::config::PoolConfig;
use crate::runtime::automations::room_agents::RoomAgentPlacementAutomation;
use crate::runtime::automations::{
    AutomationAction, AutomationCondition, AutomationConditionOp, AutomationRecord,
    AutomationScalar, AutomationState, AutomationTextTarget, AutomationTextTargetKind,
    AutomationTrigger,
};
use crate::runtime::timeline::{event_start, isoformat_z, parse_instant, utc_now};
use crate::runtime::util::first_value_string;
use crate::runtime::{
    CommandRequest, Job, JobKind, JobState, RoomConfig, RoomControl, Runtime, RuntimeScope,
    RuntimeScopeKind, TextDeliveryKind, TextDeliveryPayload, TextTarget, TextTargetKind,
    VoiceAssignment, VoiceBotStatus, VoiceCaptureSessionStatus,
};

pub(crate) trait Automation: Send + Sync {
    fn name(&self) -> &'static str;
    fn evaluate(&self, context: &AutomationContext<'_>) -> Result<AutomationOutput>;
}

pub(crate) struct AutomationContext<'a> {
    active_jobs: &'a [Job],
    room_controls: &'a BTreeMap<String, RoomControl>,
    voice_state: &'a AutomationVoiceState,
    room_configs: &'a [RoomConfig],
    pool_config: &'a PoolConfig,
}

impl<'a> AutomationContext<'a> {
    fn new(
        active_jobs: &'a [Job],
        room_controls: &'a BTreeMap<String, RoomControl>,
        voice_state: &'a AutomationVoiceState,
        room_configs: &'a [RoomConfig],
        pool_config: &'a PoolConfig,
    ) -> Self {
        Self {
            active_jobs,
            room_controls,
            voice_state,
            room_configs,
            pool_config,
        }
    }

    pub(crate) fn voice_state(&self) -> &'a AutomationVoiceState {
        self.voice_state
    }

    pub(crate) fn room_configs(&self) -> &'a [RoomConfig] {
        self.room_configs
    }

    pub(crate) fn pool_config(&self) -> &'a PoolConfig {
        self.pool_config
    }

    pub(crate) fn room_control(&self, channel_id: &str) -> Option<&'a RoomControl> {
        self.room_controls.get(channel_id)
    }

    pub(crate) fn has_active_job_in_guild(
        &self,
        kind: JobKind,
        guild_id: &str,
        matches: impl Fn(&Job) -> bool,
    ) -> bool {
        self.active_jobs.iter().any(|job| {
            job.kind == kind
                && is_active_job_state(job.state)
                && job.guild_id == guild_id
                && matches(job)
        })
    }

    pub(crate) fn room_control_datetime_active(&self, channel_id: &str, key: &str) -> bool {
        crate::runtime::rooms::control_state::room_control_datetime_active_from_map(
            self.room_controls,
            channel_id,
            key,
        )
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct AutomationVoiceState {
    pub bots: Vec<VoiceBotStatus>,
    pub sessions: Vec<VoiceCaptureSessionStatus>,
    pub assignments: Vec<VoiceAssignment>,
    pub room_occupants: BTreeMap<String, Vec<Value>>,
    pub room_empty_since: BTreeMap<String, chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct AutomationOutput {
    jobs: Vec<Job>,
}

impl AutomationOutput {
    pub(crate) fn empty() -> Self {
        Self { jobs: Vec::new() }
    }

    pub(crate) fn emit(&mut self, job: Job) {
        self.jobs.push(job);
    }

    fn into_jobs(self) -> Vec<Job> {
        self.jobs
    }
}

#[derive(Debug, Clone)]
pub struct AutomationJob {
    pub automation: String,
    pub job: Job,
}

#[derive(Debug, Clone, Default)]
pub struct AutomationRun {
    created: Vec<AutomationJob>,
}

impl AutomationRun {
    pub fn to_json(&self) -> Value {
        json!({
            "createdJobs": self.created.iter().map(|created| {
                json!({
                    "automation": created.automation,
                    "job": created.job.to_value(),
                })
            }).collect::<Vec<_>>(),
        })
    }
}

struct AutomationRunner {
    built_ins: Vec<Box<dyn Automation>>,
}

impl AutomationRunner {
    fn runtime_default() -> Self {
        Self {
            built_ins: vec![Box::new(RoomAgentPlacementAutomation)],
        }
    }

    async fn run(&self, runtime: &mut Runtime) -> Result<AutomationRun> {
        runtime.prune_expired_room_controls().await?;
        let pool_config = runtime
            .timeline_store
            .runtime_pool_config()
            .await
            .context("loading runtime pool config for automation evaluation")?;
        let room_configs = runtime
            .timeline_store
            .list_room_configs()
            .await
            .context("loading room config for automation evaluation")?;
        let room_controls = runtime.timeline_store.list_room_controls().await?;
        let mut room_occupants = BTreeMap::new();
        let mut room_empty_since = BTreeMap::new();
        for room in &room_configs {
            let occupants = runtime
                .timeline_store
                .room_occupants(&room.guild_id, &room.channel_id)
                .await
                .with_context(|| {
                    format!(
                        "loading room occupants for automation evaluation: {}:{}",
                        room.guild_id, room.channel_id
                    )
                })?;
            if occupants.is_empty() {
                if let Some(empty_since) = runtime
                    .timeline_store
                    .room_empty_since(&room.guild_id, &room.channel_id)
                    .await
                    .with_context(|| {
                        format!(
                            "loading room empty timestamp for automation evaluation: {}:{}",
                            room.guild_id, room.channel_id
                        )
                    })?
                {
                    room_empty_since.insert(room.channel_id.clone(), empty_since);
                }
            }
            room_occupants.insert(room.channel_id.clone(), occupants);
        }
        let voice_state = AutomationVoiceState {
            bots: runtime
                .timeline_store
                .list_voice_bot_states()
                .await
                .context("loading voice bot states for automation evaluation")?,
            sessions: runtime
                .timeline_store
                .list_active_capture_sessions()
                .await
                .context("loading active capture sessions for automation evaluation")?,
            assignments: runtime
                .timeline_store
                .list_active_voice_assignments()
                .await
                .context("loading active voice assignments for automation evaluation")?,
            room_occupants,
            room_empty_since,
        };

        let mut active_jobs = runtime
            .timeline_store
            .list_jobs_by_states(
                None,
                &[JobState::Queued, JobState::Running, JobState::Waiting],
            )
            .await
            .context("loading active jobs for automation evaluation")?;
        let mut created = Vec::new();
        for automation in &self.built_ins {
            let automation_name = automation.name();
            let jobs = {
                let context = AutomationContext::new(
                    &active_jobs,
                    &room_controls,
                    &voice_state,
                    &room_configs,
                    &pool_config,
                );
                automation
                    .evaluate(&context)
                    .with_context(|| format!("evaluating automation {automation_name}"))?
                    .into_jobs()
            };
            for job in jobs {
                let job = runtime
                    .timeline_store
                    .create_job(job)
                    .await
                    .with_context(|| {
                        format!("creating job emitted by automation {automation_name}")
                    })?;
                active_jobs.push(job.clone());
                created.push(AutomationJob {
                    automation: automation_name.to_string(),
                    job,
                });
            }
        }
        for job in run_stored_automations(runtime, &mut active_jobs)
            .await
            .context("running stored automations")?
        {
            created.push(job);
        }
        Ok(AutomationRun { created })
    }
}

impl Runtime {
    pub async fn run_automations(&mut self) -> Result<AutomationRun> {
        AutomationRunner::runtime_default().run(self).await
    }
}

fn is_active_job_state(state: JobState) -> bool {
    matches!(
        state,
        JobState::Queued | JobState::Running | JobState::Waiting
    )
}

async fn run_stored_automations(
    runtime: &mut Runtime,
    active_jobs: &mut Vec<Job>,
) -> Result<Vec<AutomationJob>> {
    let mut created = Vec::new();
    let records = runtime
        .timeline_store
        .list_automations(None, None, Some(AutomationState::Active))
        .await?;
    for record in records {
        let automation_id = record.automation_id.clone();
        if is_expired_by_time(&record) || is_expired_by_count(&record) {
            let mut expired = record;
            expired.state = AutomationState::Expired;
            expired.mark_evaluated();
            runtime
                .timeline_store
                .save_automation_record(&expired)
                .await?;
            continue;
        }
        let outcome = evaluate_stored_automation(runtime, &record).await?;
        let mut updated = record.clone();
        if outcome.evaluated {
            if let Some(pending) = outcome.pending_recheck {
                updated.mark_pending_recheck(pending.due_at, pending.event_json, pending.job_json);
            } else if outcome.fire_count > 0 {
                for _ in 0..outcome.fire_count {
                    updated.mark_fired();
                }
            } else {
                updated.mark_evaluated();
            }
            runtime
                .timeline_store
                .save_automation_record(&updated)
                .await?;
        }
        for job in outcome.jobs {
            let job = runtime.timeline_store.create_job(job).await?;
            runtime
                .timeline_store
                .append_event(
                    &job.guild_id,
                    &job.scope_id,
                    json!({
                        "event_kind": "automation_fired",
                        "kind": "automation_fired",
                        "automation_id": automation_id,
                        "job_id": job.id,
                        "job_kind": job.kind.as_str(),
                    }),
                )
                .await?;
            active_jobs.push(job.clone());
            created.push(AutomationJob {
                automation: automation_id.clone(),
                job,
            });
        }
    }
    Ok(created)
}

#[derive(Debug, Clone, Default)]
struct StoredAutomationOutcome {
    evaluated: bool,
    jobs: Vec<Job>,
    fire_count: u64,
    pending_recheck: Option<PendingRecheck>,
}

#[derive(Debug, Clone)]
struct PendingRecheck {
    due_at: String,
    event_json: Option<String>,
    job_json: Option<String>,
}

async fn evaluate_stored_automation(
    runtime: &Runtime,
    record: &AutomationRecord,
) -> Result<StoredAutomationOutcome> {
    if record.state != AutomationState::Active {
        return Ok(StoredAutomationOutcome::default());
    }

    if let Some(pending) = &record.pending_recheck {
        if !pending_recheck_due(&pending.due_at)? {
            return Ok(StoredAutomationOutcome::default());
        }
        let context = base_context(
            runtime,
            record,
            pending_json_value(pending.event_json.as_deref(), "event")?,
            pending_json_value(pending.job_json.as_deref(), "job")?,
        )
        .await?;
        let delay_matches = match record
            .spec
            .delay
            .as_ref()
            .and_then(|delay| delay.condition.as_ref())
        {
            Some(condition) => condition_matches(condition, &context)?,
            None => true,
        };
        let jobs = if delay_matches {
            jobs_for_actions(runtime, record).await?
        } else {
            Vec::new()
        };
        let fire_count = if jobs.is_empty() { 0 } else { 1 };
        return Ok(StoredAutomationOutcome {
            evaluated: true,
            jobs,
            fire_count,
            pending_recheck: None,
        });
    }

    let contexts = trigger_contexts(runtime, record).await?;
    if contexts.is_empty() {
        return Ok(StoredAutomationOutcome::default());
    }

    let mut jobs = Vec::new();
    let mut fire_count = 0;
    let max_fires = record.spec.expiry.max_fires.unwrap_or(u64::MAX);
    let remaining_fires = max_fires.saturating_sub(record.fire_count);
    for context in contexts {
        if fire_count >= remaining_fires {
            break;
        }
        if !condition_matches(&record.spec.condition, &context)? {
            continue;
        }
        if let Some(delay) = &record.spec.delay {
            let delay_seconds =
                i64::try_from(delay.seconds).context("automation delay seconds exceeds i64")?;
            let due_at = isoformat_z(Some(utc_now() + chrono::Duration::seconds(delay_seconds)));
            return Ok(StoredAutomationOutcome {
                evaluated: true,
                jobs,
                fire_count,
                pending_recheck: Some(PendingRecheck {
                    due_at,
                    event_json: context_value_json(&context, "event")?,
                    job_json: context_value_json(&context, "job")?,
                }),
            });
        }
        let action_jobs = jobs_for_actions(runtime, record).await?;
        if !action_jobs.is_empty() {
            fire_count += 1;
            jobs.extend(action_jobs);
        }
    }
    Ok(StoredAutomationOutcome {
        evaluated: true,
        jobs,
        fire_count,
        pending_recheck: None,
    })
}

async fn jobs_for_actions(runtime: &Runtime, record: &AutomationRecord) -> Result<Vec<Job>> {
    let mut jobs = Vec::new();
    for action in &record.spec.actions {
        match job_for_action(record, action) {
            Ok(job) => jobs.push(job),
            Err(error) => {
                runtime
                    .timeline_store
                    .append_event(
                        &record.spec.scope.guild_id,
                        &record.spec.scope.scope_id,
                        json!({
                            "event_kind": "automation_action_failed",
                            "kind": "automation_action_failed",
                            "automation_id": record.automation_id,
                            "action": format!("{action:?}"),
                            "error": error.to_string(),
                        }),
                    )
                    .await?;
            }
        }
    }
    Ok(jobs)
}

fn pending_recheck_due(due_at: &str) -> Result<bool> {
    let due_at = parse_instant(due_at)
        .with_context(|| format!("automation pending recheck has invalid due_at: {due_at}"))?;
    Ok(utc_now() >= due_at)
}

fn pending_json_value(value: Option<&str>, field: &str) -> Result<Option<Value>> {
    value
        .map(|value| {
            serde_json::from_str(value)
                .with_context(|| format!("automation pending recheck has invalid {field}_json"))
        })
        .transpose()
}

fn context_value_json(context: &Value, key: &str) -> Result<Option<String>> {
    context
        .get(key)
        .filter(|value| !value.is_null())
        .map(serde_json::to_string)
        .transpose()
        .with_context(|| format!("serializing automation pending {key} context"))
}

async fn trigger_contexts(runtime: &Runtime, record: &AutomationRecord) -> Result<Vec<Value>> {
    match &record.spec.trigger {
        AutomationTrigger::Tick { interval_seconds } => {
            if !tick_due(record, *interval_seconds) {
                return Ok(Vec::new());
            }
            Ok(vec![base_context(runtime, record, None, None).await?])
        }
        AutomationTrigger::Event { event_kinds } => {
            event_contexts(runtime, record, event_kinds).await
        }
        AutomationTrigger::Job { job_kinds, states } => {
            job_contexts(runtime, record, job_kinds, states).await
        }
        AutomationTrigger::RoomStateChanged => {
            event_contexts(
                runtime,
                record,
                &[
                    "room_state_changed".to_string(),
                    "occupancy_updated".to_string(),
                    "participant_joined".to_string(),
                    "participant_left".to_string(),
                    "participant_mute_changed".to_string(),
                    "participant_deafen_changed".to_string(),
                    "participant_stream_changed".to_string(),
                    "participant_video_changed".to_string(),
                    "participant_suppress_changed".to_string(),
                ],
            )
            .await
        }
    }
}

async fn event_contexts(
    runtime: &Runtime,
    record: &AutomationRecord,
    event_kinds: &[String],
) -> Result<Vec<Value>> {
    let kinds = event_kinds.iter().cloned().collect::<BTreeSet<_>>();
    let start = parse_instant(&record.cursor_at()).or_else(|| parse_instant(&record.created_at));
    let events = runtime
        .timeline_store
        .load_events(
            &record.spec.scope.guild_id,
            &record.spec.scope.scope_id,
            start,
            None,
            Some(&kinds),
            None,
            false,
        )
        .await?;
    let mut contexts = Vec::new();
    for event in events.into_iter().filter(|event| {
        let created = event_start(event)
            .or_else(|| parse_instant(&first_value_string(event, &["created_at", "timestamp"])));
        let cursor = parse_instant(&record.cursor_at());
        match (created, cursor) {
            (Some(created), Some(cursor)) => created > cursor,
            _ => true,
        }
    }) {
        contexts.push(base_context(runtime, record, Some(event), None).await?);
    }
    Ok(contexts)
}

async fn job_contexts(
    runtime: &Runtime,
    record: &AutomationRecord,
    job_kinds: &[JobKind],
    states: &[JobState],
) -> Result<Vec<Value>> {
    let cursor = parse_instant(&record.cursor_at());
    let jobs = runtime
        .timeline_store
        .list_jobs_for_trigger(
            &record.spec.scope.guild_id,
            &record.spec.scope.scope_id,
            job_kinds,
            states,
            cursor,
        )
        .await?
        .into_iter()
        .filter(|job| job.guild_id == record.spec.scope.guild_id)
        .filter(|job| job.scope_id == record.spec.scope.scope_id)
        .filter(|job| job_kinds.contains(&job.kind) && states.contains(&job.state))
        .filter(|job| {
            let updated = parse_instant(&job.updated_at);
            match (updated, cursor) {
                (Some(updated), Some(cursor)) => updated > cursor,
                _ => true,
            }
        })
        .collect::<Vec<_>>();
    let mut contexts = Vec::new();
    for job in jobs {
        contexts.push(base_context(runtime, record, None, Some(job.to_value())).await?);
    }
    Ok(contexts)
}

async fn base_context(
    runtime: &Runtime,
    record: &AutomationRecord,
    event: Option<Value>,
    job: Option<Value>,
) -> Result<Value> {
    let room = runtime
        .room_for_channel_ids(
            &record.spec.scope.guild_id,
            &record.spec.scope.scope_id,
            None,
        )
        .await?;
    let occupants = runtime
        .timeline_store
        .room_occupants(&record.spec.scope.guild_id, &record.spec.scope.scope_id)
        .await?;
    let participants = room_participants(&occupants);
    let mut room_status = runtime.status_for_room(&room).await?;
    if let Value::Object(object) = &mut room_status {
        object.insert("liveOccupants".to_string(), json!(occupants));
        object.insert("participants".to_string(), json!(participants));
    }
    let event_room = event
        .as_ref()
        .and_then(|event| event.get("event_room"))
        .cloned()
        .unwrap_or_else(|| json!({"before": Value::Null, "after": Value::Null}));
    Ok(json!({
        "automation": record.to_json(),
        "runtime": {
            "now": isoformat_z(None),
        },
        "room": room_status,
        "event_room": event_room,
        "event": event.unwrap_or(Value::Null),
        "job": job.unwrap_or(Value::Null),
    }))
}

fn room_participants(occupants: &[Value]) -> BTreeMap<String, Value> {
    occupants
        .iter()
        .filter_map(|occupant| {
            let user_id = first_value_string(occupant, &["user_id", "userId", "speaker_user_id"]);
            (!user_id.is_empty()).then(|| {
                (
                    user_id.clone(),
                    json!({
                        "present": true,
                        "user_id": user_id,
                        "display_name": first_value_string(occupant, &["display_name", "member_display_name", "global_name", "globalName", "username"]),
                        "username": first_value_string(occupant, &["username"]),
                        "mute": occupant_bool(occupant, "mute"),
                        "deaf": occupant_bool(occupant, "deaf"),
                        "self_mute": occupant_bool(occupant, "self_mute"),
                        "self_deaf": occupant_bool(occupant, "self_deaf"),
                        "server_mute": occupant_bool(occupant, "mute"),
                        "server_deaf": occupant_bool(occupant, "deaf"),
                        "muted": occupant_muted(occupant),
                        "deafened": occupant_deafened(occupant),
                        "streaming": occupant_bool(occupant, "self_stream"),
                        "video": occupant_bool(occupant, "self_video"),
                        "suppress": occupant_bool(occupant, "suppress"),
                    }),
                )
            })
        })
        .collect()
}

fn occupant_muted(occupant: &Value) -> bool {
    occupant_bool(occupant, "mute") || occupant_bool(occupant, "self_mute")
}

fn occupant_deafened(occupant: &Value) -> bool {
    occupant_bool(occupant, "deaf") || occupant_bool(occupant, "self_deaf")
}

fn occupant_bool(occupant: &Value, key: &str) -> bool {
    occupant.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn job_for_action(record: &AutomationRecord, action: &AutomationAction) -> Result<Job> {
    let guild_id = record.spec.scope.guild_id.clone();
    let scope_id = record.spec.scope.scope_id.clone();
    let requested_by_user_id = automation_requested_by(record);
    match action {
        AutomationAction::TextSend { sink, content } => Ok(Job::text_delivery(
            automation_voice_scope(record)?,
            requested_by_user_id.clone(),
            TextDeliveryPayload::new(
                TextDeliveryKind::Message,
                text_target(sink)?,
                content.clone(),
                record.automation_id.clone(),
                requested_by_user_id,
                false,
            ),
        )),
        AutomationAction::AgentTaskStart { prompt, .. } => Ok(Job::command_request(
            automation_voice_scope(record)?,
            requested_by_user_id.clone(),
            CommandRequest::agent_task(guild_id, scope_id, requested_by_user_id, prompt),
        )),
        AutomationAction::TranscriptStartLive { title } => Ok(Job::command_request(
            automation_voice_scope(record)?,
            requested_by_user_id.clone(),
            CommandRequest::start_live_transcript(guild_id, scope_id, requested_by_user_id, title),
        )),
        AutomationAction::SoundPlay { name } => anyhow::bail!(
            "automation action sound.play is not executable until a sound playback job exists: {name}"
        ),
    }
}

fn automation_voice_scope(record: &AutomationRecord) -> Result<RuntimeScope> {
    let scope_kind = record.spec.scope.scope_kind.parse::<RuntimeScopeKind>()?;
    if scope_kind != RuntimeScopeKind::VoiceChannel {
        anyhow::bail!("automation action requires voice_channel scope");
    }
    Ok(RuntimeScope::voice_channel(
        record.spec.scope.guild_id.clone(),
        record.spec.scope.scope_id.clone(),
    ))
}

fn text_target(sink: &AutomationTextTarget) -> Result<TextTarget> {
    Ok(match sink.kind {
        AutomationTextTargetKind::AgentChat => TextTarget {
            kind: TextTargetKind::AgentChat,
            ..TextTarget::default()
        },
        AutomationTextTargetKind::Channel => TextTarget {
            kind: TextTargetKind::Channel,
            channel_id: sink.id.clone(),
            ..TextTarget::default()
        },
        AutomationTextTargetKind::Dm => TextTarget {
            kind: TextTargetKind::Dm,
            user_id: sink.id.clone(),
            ..TextTarget::default()
        },
    })
}

fn automation_requested_by(record: &AutomationRecord) -> String {
    match &record.spec.owner {
        crate::runtime::automations::AutomationOwner::Agent { user_id, .. }
        | crate::runtime::automations::AutomationOwner::User { user_id } => user_id.clone(),
        crate::runtime::automations::AutomationOwner::System => "automation".to_string(),
    }
}

fn condition_matches(condition: &AutomationCondition, context: &Value) -> Result<bool> {
    Ok(match condition {
        AutomationCondition::True => true,
        AutomationCondition::All { conditions } => {
            for condition in conditions {
                if !condition_matches(condition, context)? {
                    return Ok(false);
                }
            }
            true
        }
        AutomationCondition::Any { conditions } => {
            for condition in conditions {
                if condition_matches(condition, context)? {
                    return Ok(true);
                }
            }
            false
        }
        AutomationCondition::Not { condition } => !condition_matches(condition, context)?,
        AutomationCondition::Predicate { path, op, value } => {
            predicate_matches(context, path, *op, value.as_ref())
        }
    })
}

fn predicate_matches(
    context: &Value,
    path: &str,
    op: AutomationConditionOp,
    expected: Option<&AutomationScalar>,
) -> bool {
    let actual = value_at_path(context, path);
    match op {
        AutomationConditionOp::Present => actual.is_some_and(|value| !value.is_null()),
        AutomationConditionOp::Empty => actual.is_none_or(value_is_empty),
        AutomationConditionOp::Eq => {
            compare_values(actual, expected, |ordering| ordering == Ordering::Equal)
        }
        AutomationConditionOp::Ne => {
            !compare_values(actual, expected, |ordering| ordering == Ordering::Equal)
        }
        AutomationConditionOp::Gt => {
            compare_values(actual, expected, |ordering| ordering == Ordering::Greater)
        }
        AutomationConditionOp::Gte => compare_values(actual, expected, |ordering| {
            matches!(ordering, Ordering::Greater | Ordering::Equal)
        }),
        AutomationConditionOp::Lt => {
            compare_values(actual, expected, |ordering| ordering == Ordering::Less)
        }
        AutomationConditionOp::Lte => compare_values(actual, expected, |ordering| {
            matches!(ordering, Ordering::Less | Ordering::Equal)
        }),
        AutomationConditionOp::Contains => actual
            .and_then(Value::as_str)
            .zip(expected.and_then(scalar_string))
            .is_some_and(|(actual, expected)| actual.contains(&expected)),
        AutomationConditionOp::Matches => actual
            .and_then(Value::as_str)
            .zip(expected.and_then(scalar_string))
            .is_some_and(|(actual, expected)| {
                regex::Regex::new(&expected)
                    .map(|regex| regex.is_match(actual))
                    .unwrap_or(false)
            }),
    }
}

fn value_at_path<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    let mut current = value;
    for part in path.split('.').filter(|part| !part.trim().is_empty()) {
        if let Ok(index) = part.parse::<usize>() {
            current = current.as_array()?.get(index)?;
        } else {
            current = current.as_object()?.get(part)?;
        }
    }
    Some(current)
}

fn compare_values(
    actual: Option<&Value>,
    expected: Option<&AutomationScalar>,
    predicate: impl Fn(Ordering) -> bool,
) -> bool {
    let Some(actual) = actual else {
        return false;
    };
    let Some(expected) = expected else {
        return false;
    };
    if let (Some(actual), Some(expected)) = (actual.as_f64(), scalar_f64(expected)) {
        return predicate(actual.total_cmp(&expected));
    }
    let actual = scalar_like_string(actual);
    let expected = scalar_string(expected);
    actual
        .zip(expected)
        .map(|(actual, expected)| predicate(actual.cmp(&expected)))
        .unwrap_or(false)
}

fn value_is_empty(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(value) => value.trim().is_empty(),
        Value::Array(value) => value.is_empty(),
        Value::Object(value) => value.is_empty(),
        _ => false,
    }
}

fn scalar_like_string(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Bool(value) => Some(value.to_string()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn scalar_string(value: &AutomationScalar) -> Option<String> {
    match value {
        AutomationScalar::String(value) => Some(value.clone()),
        AutomationScalar::Number(value) => Some(value.to_string()),
        AutomationScalar::Bool(value) => Some(value.to_string()),
    }
}

fn scalar_f64(value: &AutomationScalar) -> Option<f64> {
    match value {
        AutomationScalar::Number(value) => Some(*value),
        AutomationScalar::String(value) => value.parse::<f64>().ok(),
        AutomationScalar::Bool(_) => None,
    }
}

fn tick_due(record: &AutomationRecord, interval_seconds: u64) -> bool {
    let Some(last) = parse_instant(&record.cursor_at()) else {
        return true;
    };
    let elapsed = (utc_now() - last).num_seconds();
    elapsed >= interval_seconds as i64
}

fn is_expired_by_count(record: &AutomationRecord) -> bool {
    record
        .spec
        .expiry
        .max_fires
        .is_some_and(|max_fires| record.fire_count >= max_fires)
}

fn is_expired_by_time(record: &AutomationRecord) -> bool {
    record
        .spec
        .expiry
        .expires_at
        .as_deref()
        .and_then(parse_instant)
        .is_some_and(|expires_at| utc_now() >= expires_at)
}
