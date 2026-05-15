use std::collections::BTreeMap;

use serde_json::{Value, json};

use clankcord::runtime::automations::{
    AutomationAction, AutomationResponseSinkKind, AutomationSpec, AutomationState,
    AutomationTrigger,
};
use clankcord::runtime::timeline::TimelineStore;
use clankcord::runtime::{
    AgentRuntime, CommandRequest, ControlConfig, Job, JobKind, JobState, ResponseKind,
    ResponsePayload, ResponseSink, ResponseSinkKind, Runtime,
};

mod common;
use common::{dt, test_store};

#[tokio::test(flavor = "current_thread")]
async fn automation_spec_lowers_boundary_json_to_typed_structs() {
    let spec = AutomationSpec::from_json(&json!({
        "schema": "clankcord.automation.v0",
        "name": "alarm",
        "idempotency_key": "job_1:alarm",
        "owner": {"kind": "agent", "user_id": "user-a", "source_job_id": "job_1"},
        "scope": {"guild_id": "guild", "voice_channel_id": "code"},
        "trigger": {"kind": "event", "event_kinds": ["timer.elapsed"]},
        "condition": {"kind": "true"},
        "actions": [{
            "kind": "response.send",
            "sink": {"kind": "agent_chat"},
            "content": "Timer done."
        }]
    }))
    .unwrap();

    assert_eq!(spec.expiry.max_fires, Some(1));
    assert_eq!(spec.scope.guild_id, "guild");
    let AutomationAction::ResponseSend { sink, content } = &spec.actions[0] else {
        panic!("expected response action");
    };
    assert_eq!(sink.kind, AutomationResponseSinkKind::AgentChat);
    assert_eq!(content, "Timer done.");
}

#[tokio::test(flavor = "current_thread")]
async fn automation_job_trigger_accepts_runtime_job_names() {
    let spec = AutomationSpec::from_json(&json!({
        "schema": "clankcord.automation.v0",
        "name": "job watcher",
        "owner": {"kind": "system"},
        "scope": {"guild_id": "guild", "voice_channel_id": "code"},
        "trigger": {
            "kind": "job",
            "job_kinds": ["agent_task"],
            "states": ["complete", "failed"]
        },
        "actions": [{
            "kind": "agent_task.start",
            "prompt": "Summarize the completed job."
        }]
    }))
    .unwrap();

    let AutomationTrigger::Job { job_kinds, states } = spec.trigger else {
        panic!("expected job trigger");
    };
    assert_eq!(job_kinds, vec![JobKind::AgentTask]);
    assert_eq!(states, vec![JobState::Complete, JobState::Failed]);
}

#[tokio::test(flavor = "current_thread")]
async fn automation_spec_accepts_camel_case_agent_json_at_the_boundary() {
    let spec = AutomationSpec::from_json(&json!({
        "schema": "clankcord.automation.v0",
        "name": "camel case reminder",
        "idempotencyKey": "job_1:camel",
        "owner": {"kind": "agent", "userId": "user-a", "sourceJobId": "job_1"},
        "scope": {"guildId": "guild", "voiceChannelId": "code"},
        "trigger": {"kind": "event", "eventKinds": ["room.member_joined"]},
        "condition": {
            "kind": "predicate",
            "path": "event.confidence",
            "op": "gte",
            "value": {"kind": "number", "value": 0.85}
        },
        "expiry": {"maxFires": 2, "expiresAt": "2026-05-12T18:00:00Z"},
        "actions": [{
            "kind": "agent_task.start",
            "prompt": "Do the follow-up work.",
            "responseSink": {"kind": "channel", "channelId": "agent-thread"}
        }]
    }))
    .unwrap();

    assert_eq!(spec.idempotency_key, "job_1:camel");
    assert_eq!(spec.expiry.max_fires, Some(2));
    let AutomationAction::AgentTaskStart {
        response_sink: Some(sink),
        ..
    } = &spec.actions[0]
    else {
        panic!("expected agent task action with response sink");
    };
    assert_eq!(sink.kind, AutomationResponseSinkKind::Channel);
    assert_eq!(sink.id, "agent-thread");
}

#[tokio::test(flavor = "current_thread")]
async fn invalid_automation_specs_return_actionable_errors() {
    let cases = [
        (
            "root array",
            json!([]),
            vec!["automation spec must be a JSON object"],
        ),
        (
            "outer spec wrapper",
            json!({"spec": spec_value(json!({}))}),
            vec!["top-level JSON object", "remove the outer `spec` wrapper"],
        ),
        (
            "scope channel shorthand",
            spec_value_replacing("scope", json!({"guild_id": "guild", "channel": "code"})),
            vec![
                "$.scope requires voice_channel_id",
                "use voice_channel_id instead of channel",
            ],
        ),
        (
            "unknown trigger kind",
            spec_value(json!({
                "trigger": {"kind": "cron", "schedule": "* * * * *"}
            })),
            vec![
                "$.trigger.kind `cron`",
                "tick, event, job, room_state_changed",
            ],
        ),
        (
            "singular event kind",
            spec_value(json!({
                "trigger": {"kind": "event", "event_kind": "room.member_joined"}
            })),
            vec!["$.trigger requires event_kinds", "not event_kind"],
        ),
        (
            "unknown job kind",
            spec_value(json!({
                "trigger": {
                    "kind": "job",
                    "job_kinds": ["worker_magic"],
                    "states": ["complete"]
                }
            })),
            vec!["$.trigger.job_kinds", "worker_magic"],
        ),
        (
            "singular job state",
            spec_value(json!({
                "trigger": {
                    "kind": "job",
                    "job_kinds": ["agent_task"],
                    "state": "complete"
                }
            })),
            vec!["$.trigger requires states", "not state"],
        ),
        (
            "empty all condition",
            spec_value(json!({
                "condition": {"kind": "all", "conditions": []}
            })),
            vec!["$.condition.conditions must be a non-empty array"],
        ),
        (
            "missing not condition body",
            spec_value(json!({
                "condition": {"kind": "not"}
            })),
            vec!["$.condition.condition is required"],
        ),
        (
            "unsupported predicate op",
            spec_value(json!({
                "condition": {
                    "kind": "predicate",
                    "path": "event.user_id",
                    "op": "equals",
                    "value": "blake"
                }
            })),
            vec!["$.condition.op `equals`", "eq, ne, gt"],
        ),
        (
            "predicate array scalar",
            spec_value(json!({
                "condition": {
                    "kind": "predicate",
                    "path": "event.user_id",
                    "op": "eq",
                    "value": ["blake"]
                }
            })),
            vec!["$.condition.value", "string, number, bool"],
        ),
        (
            "bad tagged scalar kind",
            spec_value(json!({
                "condition": {
                    "kind": "predicate",
                    "path": "event.confidence",
                    "op": "gte",
                    "value": {"kind": "decimal", "value": 0.9}
                }
            })),
            vec![
                "$.condition.value.kind `decimal`",
                "string, number, or bool",
            ],
        ),
        (
            "actions not array",
            spec_value(json!({
                "actions": {"kind": "response.send"}
            })),
            vec!["$.actions must be an array"],
        ),
        (
            "missing action kind",
            spec_value(json!({
                "actions": [{"content": "hello"}]
            })),
            vec!["$.actions[0].kind is required"],
        ),
        (
            "unknown action kind",
            spec_value(json!({
                "actions": [{"kind": "discord.post", "content": "hello"}]
            })),
            vec!["$.actions[0].kind `discord.post`", "response.send"],
        ),
        (
            "response missing sink",
            spec_value(json!({
                "actions": [{"kind": "response.send", "content": "hello"}]
            })),
            vec!["$.actions[0].sink is required"],
        ),
        (
            "sink string shorthand",
            spec_value(json!({
                "actions": [{
                    "kind": "response.send",
                    "content": "hello",
                    "sink": "agent_chat"
                }]
            })),
            vec!["$.actions[0].sink.kind parent must be an object"],
        ),
        (
            "unknown sink kind",
            spec_value(json!({
                "actions": [{
                    "kind": "response.send",
                    "content": "hello",
                    "sink": {"kind": "thread"}
                }]
            })),
            vec!["$.actions[0].sink.kind `thread`", "agent_chat, channel, dm"],
        ),
        (
            "channel sink missing id",
            spec_value(json!({
                "actions": [{
                    "kind": "response.send",
                    "content": "hello",
                    "sink": {"kind": "channel"}
                }]
            })),
            vec!["$.actions[0].sink.id is required"],
        ),
        (
            "zero max fires",
            spec_value(json!({
                "expiry": {"max_fires": 0}
            })),
            vec!["$.expiry.max_fires must be greater than 0"],
        ),
        (
            "bad expires at",
            spec_value(json!({
                "expiry": {"expires_at": "next thursday"}
            })),
            vec!["$.expiry.expires_at", "RFC3339"],
        ),
    ];

    for (name, value, expected_parts) in cases {
        let error = AutomationSpec::from_json(&value)
            .expect_err(name)
            .to_string();
        for expected in expected_parts {
            assert!(
                error.contains(expected),
                "{name}: expected error `{error}` to contain `{expected}`"
            );
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn automation_store_is_binary_idempotent_and_cancellable() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    insert_agent_source_job(&store).await;
    let spec = AutomationSpec::from_json(&json!({
        "schema": "clankcord.automation.v0",
        "name": "remind blake",
        "idempotency_key": "job_1:remind-blake",
        "owner": {"kind": "agent", "user_id": "user-a", "source_job_id": "job_1"},
        "scope": {"guild_id": "guild", "voice_channel_id": "code"},
        "trigger": {"kind": "event", "event_kinds": ["room.member_joined"]},
        "condition": {
            "kind": "predicate",
            "path": "event.user_id",
            "op": "eq",
            "value": "blake"
        },
        "actions": [{
            "kind": "response.send",
            "sink": {"kind": "agent_chat"},
            "content": "Blake joined."
        }]
    }))
    .unwrap();

    let first = store.create_automation(spec.clone()).await.unwrap();
    let second = store.create_automation(spec).await.unwrap();
    assert_eq!(first.automation_id, second.automation_id);
    let different_key_same_source = store
        .create_automation(reminder_spec("job_1:different-key-same-source"))
        .await
        .unwrap();
    assert_eq!(first.automation_id, different_key_same_source.automation_id);

    let active = store
        .list_automations(Some("guild"), Some("code"), Some(AutomationState::Active))
        .await
        .unwrap();
    assert_eq!(active.len(), 1);

    let cancelled = store.cancel_automation(&first.automation_id).await.unwrap();
    assert_eq!(cancelled.state, AutomationState::Cancelled);
    assert!(
        store
            .list_automations(Some("guild"), Some("code"), Some(AutomationState::Active))
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_loads_active_automations_after_restart() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    insert_agent_source_job(&store).await;
    let record = store
        .create_automation(reminder_spec("job_1:restart"))
        .await
        .unwrap();

    let mut restarted = test_runtime(store);
    restarted.load_automation_registry().await.unwrap();

    assert!(restarted.automations.contains_key(&record.automation_id));
}

#[tokio::test(flavor = "current_thread")]
async fn stored_event_automation_emits_response_job_once_and_expires() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    insert_agent_source_job(&store).await;
    let record = store
        .create_automation(reminder_spec("job_1:event-fire"))
        .await
        .unwrap();
    append_speech(
        &store,
        "room.member_joined",
        "blake",
        "Blake joined the room",
        1,
    )
    .await;
    let mut runtime = test_runtime(store);
    runtime.load_automation_registry().await.unwrap();

    let result = runtime.run_automations().await.unwrap().to_json();

    let created = result["createdJobs"].as_array().unwrap();
    assert_eq!(created.len(), 1);
    let job_id = created[0]["job"]["job_id"].as_str().unwrap();
    let job = runtime.timeline_store.get_job(job_id).await.unwrap();
    let payload = job.response_payload().unwrap();
    assert_eq!(payload.sink.kind, ResponseSinkKind::AgentChat);
    assert_eq!(payload.content, "Blake joined.");
    assert_eq!(payload.source_job_id, record.automation_id);
    assert!(!runtime.automations.contains_key(&record.automation_id));
    assert_eq!(
        runtime
            .timeline_store
            .get_automation(&record.automation_id)
            .await
            .unwrap()
            .state,
        AutomationState::Expired
    );
}

#[tokio::test(flavor = "current_thread")]
async fn participant_left_automation_fires_from_durable_voice_transition() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    insert_agent_source_job(&store).await;
    store
        .record_voice_state_update(None, voice_state("code", "blake", "Blake"))
        .await
        .unwrap();
    let record = store
        .create_automation(
            AutomationSpec::from_json(&spec_value(json!({
                "name": "left watcher",
                "idempotency_key": "job_1:left-watcher",
                "trigger": {"kind": "event", "event_kinds": ["participant_left"]},
                "condition": {
                    "kind": "predicate",
                    "path": "event.user_id",
                    "op": "eq",
                    "value": "blake"
                },
                "actions": [{
                    "kind": "response.send",
                    "sink": {"kind": "agent_chat"},
                    "content": "Blake left."
                }]
            })))
            .unwrap(),
        )
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    let transition_events = store
        .record_voice_state_update(None, voice_state("", "blake", "Blake"))
        .await
        .unwrap();
    assert_eq!(
        transition_events[0]["event_kind"],
        json!("participant_left")
    );
    let mut runtime = test_runtime(store);
    runtime.load_automation_registry().await.unwrap();

    let result = runtime.run_automations().await.unwrap().to_json();

    let created = result["createdJobs"].as_array().unwrap();
    assert_eq!(created.len(), 1);
    let job_id = created[0]["job"]["job_id"].as_str().unwrap();
    let job = runtime.timeline_store.get_job(job_id).await.unwrap();
    let payload = job.response_payload().unwrap();
    assert_eq!(payload.content, "Blake left.");
    assert!(!runtime.automations.contains_key(&record.automation_id));
}

#[tokio::test(flavor = "current_thread")]
async fn overlap_automation_can_match_current_room_participants() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    insert_agent_source_job(&store).await;
    store
        .record_voice_state_update(None, voice_state("code", "user-a", "Will"))
        .await
        .unwrap();
    let record = store
        .create_automation(
            AutomationSpec::from_json(&spec_value(json!({
                "name": "overlap watcher",
                "idempotency_key": "job_1:overlap-watcher",
                "trigger": {"kind": "event", "event_kinds": ["participant_joined"]},
                "condition": {
                    "kind": "all",
                    "conditions": [
                        {"kind": "predicate", "path": "room.participants.user-a.present", "op": "eq", "value": true},
                        {"kind": "predicate", "path": "room.participants.blake.present", "op": "eq", "value": true}
                    ]
                },
                "actions": [{
                    "kind": "response.send",
                    "sink": {"kind": "dm", "id": "user-a"},
                    "content": "Reminder: talk to Blake about Woven."
                }]
            })))
            .unwrap(),
        )
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    store
        .record_voice_state_update(None, voice_state("code", "blake", "Blake"))
        .await
        .unwrap();
    let mut runtime = test_runtime(store);
    runtime.load_automation_registry().await.unwrap();

    let result = runtime.run_automations().await.unwrap().to_json();

    let created = result["createdJobs"].as_array().unwrap();
    assert_eq!(created.len(), 1);
    let job_id = created[0]["job"]["job_id"].as_str().unwrap();
    let job = runtime.timeline_store.get_job(job_id).await.unwrap();
    let payload = job.response_payload().unwrap();
    assert_eq!(payload.sink.kind, ResponseSinkKind::Dm);
    assert_eq!(payload.sink.user_id, "user-a");
    assert_eq!(payload.content, "Reminder: talk to Blake about Woven.");
    assert!(!runtime.automations.contains_key(&record.automation_id));
}

#[tokio::test(flavor = "current_thread")]
async fn stored_event_automation_uses_compound_conditions_without_firing_on_noise() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    insert_agent_source_job(&store).await;
    let spec = AutomationSpec::from_json(&spec_value(json!({
        "name": "compound reminder",
        "idempotency_key": "job_1:compound",
        "condition": {
            "kind": "all",
            "conditions": [
                {
                    "kind": "predicate",
                    "path": "event.speaker_user_id",
                    "op": "eq",
                    "value": "blake"
                },
                {
                    "kind": "predicate",
                    "path": "event.text",
                    "op": "contains",
                    "value": "joined"
                }
            ]
        },
        "expiry": {"max_fires": 2}
    })))
    .unwrap();
    let record = store.create_automation(spec).await.unwrap();
    append_speech(&store, "room.member_joined", "vince", "Vince joined", 1).await;
    let mut runtime = test_runtime(store);
    runtime.load_automation_registry().await.unwrap();

    let first = runtime.run_automations().await.unwrap().to_json();
    assert!(first["createdJobs"].as_array().unwrap().is_empty());
    assert!(runtime.automations.contains_key(&record.automation_id));

    append_speech(
        &runtime.timeline_store,
        "room.member_joined",
        "blake",
        "Blake joined",
        2,
    )
    .await;
    let second = runtime.run_automations().await.unwrap().to_json();
    assert_eq!(second["createdJobs"].as_array().unwrap().len(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn stored_event_automation_does_not_replay_same_event_when_max_fires_allows_more() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    insert_agent_source_job(&store).await;
    let spec = AutomationSpec::from_json(&spec_value(json!({
        "name": "two fire reminder",
        "idempotency_key": "job_1:two-fire",
        "expiry": {"max_fires": 2}
    })))
    .unwrap();
    let record = store.create_automation(spec).await.unwrap();
    append_speech(&store, "room.member_joined", "blake", "Blake joined", 1).await;
    let mut runtime = test_runtime(store);
    runtime.load_automation_registry().await.unwrap();

    let first = runtime.run_automations().await.unwrap().to_json();
    let second = runtime.run_automations().await.unwrap().to_json();

    assert_eq!(first["createdJobs"].as_array().unwrap().len(), 1);
    assert!(second["createdJobs"].as_array().unwrap().is_empty());
    assert!(runtime.automations.contains_key(&record.automation_id));

    append_speech(
        &runtime.timeline_store,
        "room.member_joined",
        "blake",
        "Blake joined again",
        2,
    )
    .await;
    let third = runtime.run_automations().await.unwrap().to_json();
    assert_eq!(third["createdJobs"].as_array().unwrap().len(), 1);
    assert_eq!(
        runtime
            .timeline_store
            .get_automation(&record.automation_id)
            .await
            .unwrap()
            .state,
        AutomationState::Expired
    );
}

#[tokio::test(flavor = "current_thread")]
async fn stored_job_automation_emits_agent_task_job_from_completed_runtime_job() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    insert_agent_source_job(&store).await;
    let spec = AutomationSpec::from_json(&spec_value(json!({
        "name": "job follow-up",
        "idempotency_key": "job_1:job-followup",
        "trigger": {
            "kind": "job",
            "job_kinds": ["response"],
            "states": ["complete"]
        },
        "condition": {"kind": "true"},
        "actions": [{
            "kind": "agent_task.start",
            "prompt": "Summarize the completed response job."
        }]
    })))
    .unwrap();
    store.create_automation(spec).await.unwrap();
    let mut completed_response = Job::response(
        "guild",
        "code",
        "user-a",
        ResponsePayload::new(
            ResponseKind::Message,
            ResponseSink::default(),
            "done",
            "source-job",
            "user-a",
            false,
        ),
    );
    completed_response = store.create_job(completed_response).await.unwrap();
    completed_response.mark_complete();
    store.update_job(&completed_response).await.unwrap();
    let mut runtime = test_runtime(store);
    runtime.load_automation_registry().await.unwrap();

    let result = runtime.run_automations().await.unwrap().to_json();

    let created = result["createdJobs"].as_array().unwrap();
    assert_eq!(created.len(), 1);
    let job_id = created[0]["job"]["job_id"].as_str().unwrap();
    let job = runtime.timeline_store.get_job(job_id).await.unwrap();
    assert_eq!(job.kind, JobKind::AgentTask);
    assert_eq!(
        job.command().unwrap().arguments.request,
        "Summarize the completed response job."
    );
}

#[tokio::test(flavor = "current_thread")]
async fn automation_action_failures_are_audited_without_crashing_runner() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    insert_agent_source_job(&store).await;
    let spec = AutomationSpec::from_json(&spec_value(json!({
        "name": "sound request",
        "idempotency_key": "job_1:sound",
        "actions": [{
            "kind": "sound.play",
            "name": "fart"
        }]
    })))
    .unwrap();
    store.create_automation(spec).await.unwrap();
    append_speech(&store, "room.member_joined", "blake", "Blake joined", 1).await;
    let mut runtime = test_runtime(store);
    runtime.load_automation_registry().await.unwrap();

    let result = runtime.run_automations().await.unwrap().to_json();

    assert!(result["createdJobs"].as_array().unwrap().is_empty());
    let failures = runtime
        .timeline_store
        .load_events("guild", "code", None, None, None, None, false)
        .await
        .unwrap()
        .into_iter()
        .filter(|event| event["event_kind"] == json!("automation_action_failed"))
        .collect::<Vec<_>>();
    assert_eq!(failures.len(), 1);
    assert!(
        failures[0]["error"]
            .as_str()
            .unwrap()
            .contains("sound.play")
    );
}

fn reminder_spec(idempotency_key: &str) -> AutomationSpec {
    AutomationSpec::from_json(&json!({
        "schema": "clankcord.automation.v0",
        "name": "remind blake",
        "idempotency_key": idempotency_key,
        "owner": {"kind": "agent", "user_id": "user-a", "source_job_id": "job_1"},
        "scope": {"guild_id": "guild", "voice_channel_id": "code"},
        "trigger": {"kind": "event", "event_kinds": ["room.member_joined"]},
        "condition": {
            "kind": "predicate",
            "path": "event.speaker_user_id",
            "op": "eq",
            "value": "blake"
        },
        "actions": [{
            "kind": "response.send",
            "sink": {"kind": "agent_chat"},
            "content": "Blake joined."
        }]
    }))
    .unwrap()
}

fn spec_value(overrides: Value) -> Value {
    let mut base = json!({
        "schema": "clankcord.automation.v0",
        "name": "test automation",
        "idempotency_key": "test:auto",
        "owner": {"kind": "agent", "user_id": "user-a", "source_job_id": "job_1"},
        "scope": {"guild_id": "guild", "voice_channel_id": "code"},
        "trigger": {"kind": "event", "event_kinds": ["room.member_joined"]},
        "condition": {"kind": "true"},
        "actions": [{
            "kind": "response.send",
            "sink": {"kind": "agent_chat"},
            "content": "hello"
        }]
    });
    merge_json(&mut base, overrides);
    base
}

fn spec_value_replacing(key: &str, replacement: Value) -> Value {
    let mut value = spec_value(json!({}));
    value[key] = replacement;
    value
}

fn merge_json(base: &mut Value, overrides: Value) {
    match (base, overrides) {
        (Value::Object(base), Value::Object(overrides)) => {
            for (key, value) in overrides {
                if let Some(existing) = base.get_mut(&key) {
                    merge_json(existing, value);
                } else {
                    base.insert(key, value);
                }
            }
        }
        (base, value) => *base = value,
    }
}

fn test_runtime(timeline_store: TimelineStore) -> Runtime {
    Runtime {
        started_at: dt(2026, 5, 12, 15, 0, 0),
        guilds: BTreeMap::new(),
        rooms: BTreeMap::new(),
        control_config: ControlConfig::default(),
        room_controls: BTreeMap::new(),
        sessions: BTreeMap::new(),
        bots: BTreeMap::new(),
        agents: AgentRuntime::default(),
        automations: BTreeMap::new(),
        timeline_store,
        auto_join_enabled: true,
        manual_leave_cooldown_seconds: 20 * 60,
        manual_join_hold_seconds: 60 * 60,
        pause_release_seconds: 20 * 60,
    }
}

async fn insert_agent_source_job(store: &TimelineStore) {
    let mut job = Job::agent_task(
        "guild",
        "code",
        "user-a",
        CommandRequest::agent_task("guild", "code", "user-a", "source request"),
    );
    job.id = "job_1".to_string();
    job.root_job_id = "job_1".to_string();
    store.create_job(job).await.unwrap();
}

async fn append_speech(
    store: &TimelineStore,
    event_kind: &str,
    user_id: &str,
    text: &str,
    _segment_index: i64,
) {
    store
        .append_event(
            "guild",
            "code",
            json!({
                "event_kind": event_kind,
                "kind": event_kind,
                "speaker_user_id": user_id,
                "text": text,
            }),
        )
        .await
        .unwrap();
}

fn voice_state(voice_channel_id: &str, user_id: &str, display_name: &str) -> Value {
    json!({
        "guild_id": "guild",
        "voice_channel_id": voice_channel_id,
        "user_id": user_id,
        "display_name": display_name,
        "member_display_name": display_name,
        "username": display_name.to_lowercase(),
        "mute": false,
        "deaf": false,
        "self_mute": false,
        "self_deaf": false,
        "self_stream": false,
        "self_video": false,
        "suppress": false,
    })
}
