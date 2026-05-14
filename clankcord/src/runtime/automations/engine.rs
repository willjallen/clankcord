use std::cmp::Ordering;
use std::collections::BTreeSet;

use anyhow::Context;
use serde_json::{Value, json};

use crate::Result;
use crate::runtime::automations::room_agents::RoomAgentPlacementAutomation;
use crate::runtime::automations::{
    AutomationAction, AutomationCondition, AutomationConditionOp, AutomationRecord,
    AutomationResponseSink, AutomationResponseSinkKind, AutomationScalar, AutomationState,
    AutomationTrigger,
};
use crate::runtime::timeline::{
    event_start, first_value_string, isoformat_z, parse_instant, utc_now,
};
use crate::runtime::{
    CommandRequest, Job, JobKind, JobState, ResponseKind, ResponsePayload, ResponseSink,
    ResponseSinkKind, Runtime,
};

pub(crate) trait Automation: Send + Sync {
    fn name(&self) -> &'static str;
    fn evaluate(&self, context: &AutomationContext<'_>) -> Result<AutomationOutput>;
}

pub(crate) struct AutomationContext<'a> {
    runtime: &'a Runtime,
    active_jobs: &'a [Job],
}

impl<'a> AutomationContext<'a> {
    fn new(runtime: &'a Runtime, active_jobs: &'a [Job]) -> Self {
        Self {
            runtime,
            active_jobs,
        }
    }

    pub(crate) fn runtime(&self) -> &'a Runtime {
        self.runtime
    }

    pub(crate) fn has_active_job(
        &self,
        kind: JobKind,
        guild_id: &str,
        voice_channel_id: &str,
        matches: impl Fn(&Job) -> bool,
    ) -> bool {
        self.active_jobs.iter().any(|job| {
            job.kind == kind
                && is_active_job_state(job.state)
                && job.guild_id == guild_id
                && job.voice_channel_id == voice_channel_id
                && matches(job)
        })
    }
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
    automations: Vec<Box<dyn Automation>>,
}

impl AutomationRunner {
    fn runtime_default() -> Self {
        Self {
            automations: vec![Box::new(RoomAgentPlacementAutomation)],
        }
    }

    fn run(&self, runtime: &mut Runtime) -> Result<AutomationRun> {
        runtime.prune_expired_room_controls(true);

        let mut active_jobs = runtime
            .timeline_store
            .list_jobs(None, None)
            .context("loading jobs for automation evaluation")?;
        let mut created = Vec::new();
        for automation in &self.automations {
            let automation_name = automation.name();
            let jobs = {
                let context = AutomationContext::new(runtime, &active_jobs);
                automation
                    .evaluate(&context)
                    .with_context(|| format!("evaluating automation {automation_name}"))?
                    .into_jobs()
            };
            for job in jobs {
                let job = runtime.timeline_store.create_job(job).with_context(|| {
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
            .context("running stored automations")?
        {
            created.push(job);
        }
        Ok(AutomationRun { created })
    }
}

impl Runtime {
    pub fn run_automations(&mut self) -> Result<AutomationRun> {
        AutomationRunner::runtime_default().run(self)
    }
}

fn is_active_job_state(state: JobState) -> bool {
    matches!(
        state,
        JobState::Queued | JobState::Running | JobState::Waiting
    )
}

fn run_stored_automations(
    runtime: &mut Runtime,
    active_jobs: &mut Vec<Job>,
) -> Result<Vec<AutomationJob>> {
    let automation_ids = runtime.automations.keys().cloned().collect::<Vec<_>>();
    let mut created = Vec::new();
    for automation_id in automation_ids {
        let Some(record) = runtime.automations.get(&automation_id).cloned() else {
            continue;
        };
        if is_expired_by_time(&record) || is_expired_by_count(&record) {
            let mut expired = record;
            expired.state = AutomationState::Expired;
            expired.mark_evaluated();
            runtime.timeline_store.save_automation_record(&expired)?;
            runtime.automations.remove(&automation_id);
            continue;
        }
        let outcome = evaluate_stored_automation(runtime, &record, active_jobs)?;
        let mut updated = record.clone();
        if outcome.evaluated {
            if outcome.jobs.is_empty() {
                updated.mark_evaluated();
            } else {
                updated.mark_fired();
            }
            runtime.timeline_store.save_automation_record(&updated)?;
            if updated.state == AutomationState::Active {
                runtime.automations.insert(automation_id.clone(), updated);
            } else {
                runtime.automations.remove(&automation_id);
            }
        }
        for job in outcome.jobs {
            let job = runtime.timeline_store.create_job(job)?;
            runtime.timeline_store.append_event(
                &job.guild_id,
                &job.voice_channel_id,
                json!({
                    "event_kind": "automation_fired",
                    "kind": "automation_fired",
                    "automation_id": automation_id,
                    "job_id": job.id,
                    "job_kind": job.kind.as_str(),
                }),
            )?;
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
}

fn evaluate_stored_automation(
    runtime: &Runtime,
    record: &AutomationRecord,
    active_jobs: &[Job],
) -> Result<StoredAutomationOutcome> {
    if record.state != AutomationState::Active {
        return Ok(StoredAutomationOutcome::default());
    }

    let contexts = trigger_contexts(runtime, record, active_jobs)?;
    if contexts.is_empty() {
        return Ok(StoredAutomationOutcome::default());
    }

    let mut jobs = Vec::new();
    for context in contexts {
        if !condition_matches(&record.spec.condition, &context)? {
            continue;
        }
        for action in &record.spec.actions {
            match job_for_action(record, action) {
                Ok(job) => jobs.push(job),
                Err(error) => {
                    runtime.timeline_store.append_event(
                        &record.spec.scope.guild_id,
                        &record.spec.scope.voice_channel_id,
                        json!({
                            "event_kind": "automation_action_failed",
                            "kind": "automation_action_failed",
                            "automation_id": record.automation_id,
                            "action": format!("{action:?}"),
                            "error": error.to_string(),
                        }),
                    )?;
                }
            }
        }
        break;
    }
    Ok(StoredAutomationOutcome {
        evaluated: true,
        jobs,
    })
}

fn trigger_contexts(
    runtime: &Runtime,
    record: &AutomationRecord,
    active_jobs: &[Job],
) -> Result<Vec<Value>> {
    match &record.spec.trigger {
        AutomationTrigger::Tick { interval_seconds } => {
            if !tick_due(record, *interval_seconds) {
                return Ok(Vec::new());
            }
            Ok(vec![base_context(runtime, record, None, None)])
        }
        AutomationTrigger::Event { event_kinds } => event_contexts(runtime, record, event_kinds),
        AutomationTrigger::Job { job_kinds, states } => Ok(job_contexts(
            runtime,
            record,
            active_jobs,
            job_kinds,
            states,
        )),
        AutomationTrigger::RoomStateChanged => event_contexts(
            runtime,
            record,
            &[
                "room_state_changed".to_string(),
                "occupancy_updated".to_string(),
                "participant_joined".to_string(),
                "participant_left".to_string(),
            ],
        ),
    }
}

fn event_contexts(
    runtime: &Runtime,
    record: &AutomationRecord,
    event_kinds: &[String],
) -> Result<Vec<Value>> {
    let kinds = event_kinds.iter().cloned().collect::<BTreeSet<_>>();
    let start = parse_instant(&record.cursor_at()).or_else(|| parse_instant(&record.created_at));
    let events = runtime.timeline_store.load_events(
        &record.spec.scope.guild_id,
        &record.spec.scope.voice_channel_id,
        start,
        None,
        Some(&kinds),
        None,
        false,
    )?;
    Ok(events
        .into_iter()
        .filter(|event| {
            let created = event_start(event).or_else(|| {
                parse_instant(&first_value_string(event, &["created_at", "timestamp"]))
            });
            let cursor = parse_instant(&record.cursor_at());
            match (created, cursor) {
                (Some(created), Some(cursor)) => created > cursor,
                _ => true,
            }
        })
        .map(|event| base_context(runtime, record, Some(event), None))
        .collect())
}

fn job_contexts(
    runtime: &Runtime,
    record: &AutomationRecord,
    active_jobs: &[Job],
    job_kinds: &[JobKind],
    states: &[JobState],
) -> Vec<Value> {
    let cursor = parse_instant(&record.cursor_at());
    active_jobs
        .iter()
        .filter(|job| job.guild_id == record.spec.scope.guild_id)
        .filter(|job| job.voice_channel_id == record.spec.scope.voice_channel_id)
        .filter(|job| job_kinds.contains(&job.kind) && states.contains(&job.state))
        .filter(|job| {
            let updated = parse_instant(&job.updated_at);
            match (updated, cursor) {
                (Some(updated), Some(cursor)) => updated > cursor,
                _ => true,
            }
        })
        .map(|job| base_context(runtime, record, None, Some(job.to_value())))
        .collect()
}

fn base_context(
    runtime: &Runtime,
    record: &AutomationRecord,
    event: Option<Value>,
    job: Option<Value>,
) -> Value {
    let room = runtime.room_for_channel_ids(
        &record.spec.scope.guild_id,
        &record.spec.scope.voice_channel_id,
        None,
    );
    json!({
        "automation": record.to_json(),
        "runtime": {
            "now": isoformat_z(None),
        },
        "room": runtime.status_for_room(&room),
        "event": event.unwrap_or(Value::Null),
        "job": job.unwrap_or(Value::Null),
    })
}

fn job_for_action(record: &AutomationRecord, action: &AutomationAction) -> Result<Job> {
    let guild_id = record.spec.scope.guild_id.clone();
    let voice_channel_id = record.spec.scope.voice_channel_id.clone();
    let requested_by_user_id = automation_requested_by(record);
    match action {
        AutomationAction::ResponseSend { sink, content } => Ok(Job::response(
            guild_id,
            voice_channel_id,
            requested_by_user_id.clone(),
            ResponsePayload::new(
                ResponseKind::Message,
                response_sink(sink)?,
                content.clone(),
                record.automation_id.clone(),
                requested_by_user_id,
                false,
            ),
        )),
        AutomationAction::AgentTaskStart { prompt, .. } => Ok(Job::agent_task(
            guild_id.clone(),
            voice_channel_id.clone(),
            requested_by_user_id.clone(),
            CommandRequest::agent_task(guild_id, voice_channel_id, requested_by_user_id, prompt),
        )),
        AutomationAction::TranscriptStartLive { title } => Ok(Job::command_request(
            guild_id.clone(),
            voice_channel_id.clone(),
            requested_by_user_id.clone(),
            CommandRequest::start_live_transcript(
                guild_id,
                voice_channel_id,
                requested_by_user_id,
                title,
            ),
        )),
        AutomationAction::SoundPlay { name } => anyhow::bail!(
            "automation action sound.play is not executable until the sound adapter job exists: {name}"
        ),
    }
}

fn response_sink(sink: &AutomationResponseSink) -> Result<ResponseSink> {
    Ok(match sink.kind {
        AutomationResponseSinkKind::AgentChat => ResponseSink {
            kind: ResponseSinkKind::AgentChat,
            ..ResponseSink::default()
        },
        AutomationResponseSinkKind::Channel => ResponseSink {
            kind: ResponseSinkKind::Channel,
            channel_id: sink.id.clone(),
            ..ResponseSink::default()
        },
        AutomationResponseSinkKind::Dm => ResponseSink {
            kind: ResponseSinkKind::Dm,
            user_id: sink.id.clone(),
            ..ResponseSink::default()
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
