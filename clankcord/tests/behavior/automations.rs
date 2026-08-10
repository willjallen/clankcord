//! Automation specs, triggers, delayed rechecks, and action auditing against the store.

use serde_json::{Value, json};

use clankcord::domain::automations::{
    AutomationAction, AutomationCondition, AutomationSpec, AutomationState,
    AutomationTextTargetKind, AutomationTrigger,
};
use clankcord::model::job::{
    Job, JobKind, JobState, TextDeliveryKind, TextDeliveryPayload, TextTarget, TextTargetKind,
};
use clankcord::model::scope::RuntimeScope;
use clankcord::store::TimelineStore;

use crate::support::automations::spec_value;
use crate::support::automations::{
    insert_agent_source_job, reminder_spec, test_runtime, voice_state, voice_state_with_flags,
};
use crate::support::test_store;

#[tokio::test(flavor = "current_thread")]
async fn automation_spec_lowers_boundary_json_to_typed_structs() {
    let spec = AutomationSpec::from_json(&json!({
        "schema": "clankcord.automation.v0",
        "name": "alarm",
        "idempotency_key": "job_1:alarm",
        "owner": {"kind": "agent", "user_id": "user-a", "source_job_id": "job_1"},
        "scope": {"scope_kind": "voice_channel", "guild_id": "guild", "scope_id": "code"},
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
    assert_eq!(spec.scope.scope_kind, "voice_channel");
    assert_eq!(spec.scope.guild_id, "guild");
    assert_eq!(spec.scope.scope_id, "code");
    let AutomationAction::TextSend { sink, content } = &spec.actions[0] else {
        panic!("expected response action");
    };
    assert_eq!(sink.kind, AutomationTextTargetKind::AgentChat);
    assert_eq!(content, "Timer done.");
}

#[tokio::test(flavor = "current_thread")]
async fn automation_job_trigger_accepts_runtime_job_names() {
    let spec = AutomationSpec::from_json(&json!({
        "schema": "clankcord.automation.v0",
        "name": "job watcher",
        "owner": {"kind": "system"},
        "scope": {"scope_kind": "voice_channel", "guild_id": "guild", "scope_id": "code"},
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
async fn automation_spec_accepts_delayed_recheck_condition() {
    let spec = AutomationSpec::from_json(&json!({
        "schema": "clankcord.automation.v0",
        "name": "delayed away watcher",
        "owner": {"kind": "system"},
        "scope": {"scope_kind": "voice_channel", "guild_id": "guild", "scope_id": "code"},
        "trigger": {"kind": "event", "event_kinds": ["participant_left"]},
        "condition": {
            "kind": "predicate",
            "path": "event.user_id",
            "op": "eq",
            "value": "blake"
        },
        "delay": {
            "seconds": 300,
            "condition": {
                "kind": "predicate",
                "path": "room.participants.blake.present",
                "op": "empty"
            }
        },
        "actions": [{
            "kind": "response.send",
            "sink": {"kind": "agent_chat"},
            "content": "Blake is still away."
        }]
    }))
    .unwrap();

    let delay = spec.delay.unwrap();
    assert_eq!(delay.seconds, 300);
    assert!(matches!(
        delay.condition.as_ref().unwrap(),
        AutomationCondition::Predicate { path, .. } if path == "room.participants.blake.present"
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn automation_spec_accepts_camel_case_agent_json_at_the_boundary() {
    let spec = AutomationSpec::from_json(&json!({
        "schema": "clankcord.automation.v0",
        "name": "camel case reminder",
        "idempotencyKey": "job_1:camel",
        "owner": {"kind": "agent", "userId": "user-a", "sourceJobId": "job_1"},
        "scope": {"scope_kind": "voice_channel", "guild_id": "guild", "scope_id": "code"},
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
            "textTarget": {"kind": "channel", "channelId": "agent-thread"}
        }]
    }))
    .unwrap();

    assert_eq!(spec.idempotency_key, "job_1:camel");
    assert_eq!(spec.expiry.max_fires, Some(2));
    let AutomationAction::AgentTaskStart {
        text_target: Some(sink),
        ..
    } = &spec.actions[0]
    else {
        panic!("expected agent task action with response sink");
    };
    assert_eq!(sink.kind, AutomationTextTargetKind::Channel);
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
            spec_value_replacing(
                "scope",
                json!({"scope_kind": "voice_channel", "guild_id": "guild", "channel": "code"}),
            ),
            vec![
                "$.scope requires scope_id",
                "use scope_id instead of channel",
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
        (
            "zero delay seconds",
            spec_value(json!({
                "delay": {"seconds": 0}
            })),
            vec!["$.delay.seconds must be a positive integer"],
        ),
        (
            "bad delay condition",
            spec_value(json!({
                "delay": {"seconds": 60, "condition": {"kind": "all", "conditions": []}}
            })),
            vec!["$.delay.condition.conditions must be a non-empty array"],
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
        "scope": {"scope_kind": "voice_channel", "guild_id": "guild", "scope_id": "code"},
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
async fn automation_payload_blob_uses_current_envelope() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    insert_agent_source_job(&store).await;
    let record = store
        .create_automation(reminder_spec("job_1:blob-envelope"))
        .await
        .unwrap();

    let row = sqlx::query("SELECT payload_blob FROM automations WHERE automation_id = $1")
        .bind(&record.automation_id)
        .fetch_one(&store.pool)
        .await
        .unwrap();
    let payload_blob: Vec<u8> = sqlx::Row::try_get(&row, "payload_blob").unwrap();
    assert_eq!(&payload_blob[..8], b"CLANKAUT");
    assert_eq!(u16::from_le_bytes([payload_blob[8], payload_blob[9]]), 1);

    sqlx::query("UPDATE automations SET payload_blob = $1 WHERE automation_id = $2")
        .bind(bincode::serialize(&record).unwrap())
        .bind(&record.automation_id)
        .execute(&store.pool)
        .await
        .unwrap();
    let error = store
        .get_automation(&record.automation_id)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("invalid blob envelope"));
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

    let restarted = test_runtime(store);

    assert_eq!(
        restarted
            .store
            .get_automation(&record.automation_id)
            .await
            .unwrap()
            .state,
        AutomationState::Active
    );
}

#[tokio::test(flavor = "current_thread")]
async fn stored_event_automation_emits_text_delivery_job_once_and_expires() {
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
    let runtime = test_runtime(store);

    let result = clankcord::domain::automations::engine::run_automations(&runtime)
        .await
        .unwrap()
        .to_json();

    let created = result["createdJobs"].as_array().unwrap();
    assert_eq!(created.len(), 1);
    let job_id = created[0]["job"]["job_id"].as_str().unwrap();
    let job = runtime.store.get_job(job_id).await.unwrap();
    let payload = job.text_delivery_payload().unwrap();
    assert_eq!(payload.target.kind, TextTargetKind::AgentChat);
    assert_eq!(payload.content, "Blake joined.");
    assert_eq!(payload.source_job_id, record.automation_id);
    assert_eq!(
        runtime
            .store
            .get_automation(&record.automation_id)
            .await
            .unwrap()
            .state,
        AutomationState::Expired
    );
}

#[tokio::test(flavor = "current_thread")]
async fn participant_left_event_room_snapshot_records_before_and_after_presence() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    store
        .record_voice_state_update(None, voice_state("code", "blake", "Blake"))
        .await
        .unwrap();
    store
        .record_voice_state_update(None, voice_state("code", "user-a", "Will"))
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;

    let transition_events = store
        .record_voice_state_update(None, voice_state("", "blake", "Blake"))
        .await
        .unwrap();

    assert_eq!(transition_events.len(), 1);
    let event = &transition_events[0];
    assert_eq!(event["event_kind"], json!("participant_left"));
    assert_eq!(
        event["event_room"]["before"]["participants"]["blake"]["present"],
        json!(true)
    );
    assert_eq!(
        event["event_room"]["before"]["participants"]["user-a"]["present"],
        json!(true)
    );
    assert!(event["event_room"]["after"]["participants"]["blake"].is_null());
    assert_eq!(
        event["event_room"]["after"]["participants"]["user-a"]["present"],
        json!(true)
    );
    assert_eq!(
        event["event_room"]["before"]["liveOccupants"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        event["event_room"]["after"]["liveOccupants"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test(flavor = "current_thread")]
async fn room_participants_exposes_voice_state_flags_for_conditions() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    insert_agent_source_job(&store).await;
    let record = store
        .create_automation(
            AutomationSpec::from_json(&spec_value(json!({
                "name": "undeafened overlap watcher",
                "idempotency_key": "job_1:participant-voice-flags",
                "trigger": {"kind": "event", "event_kinds": ["participant_joined"]},
                "condition": {
                    "kind": "all",
                    "conditions": [
                        {"kind": "predicate", "path": "event.user_id", "op": "eq", "value": "blake"},
                        {"kind": "predicate", "path": "room.participants.blake.present", "op": "eq", "value": true},
                        {"kind": "predicate", "path": "room.participants.blake.deaf", "op": "eq", "value": false},
                        {"kind": "predicate", "path": "room.participants.blake.self_deaf", "op": "eq", "value": false},
                        {"kind": "predicate", "path": "room.participants.blake.mute", "op": "eq", "value": false},
                        {"kind": "predicate", "path": "room.participants.blake.self_mute", "op": "eq", "value": false}
                    ]
                },
                "actions": [{
                    "kind": "response.send",
                    "sink": {"kind": "agent_chat"},
                    "content": "Blake is present and can hear."
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
    let runtime = test_runtime(store);

    let result = clankcord::domain::automations::engine::run_automations(&runtime)
        .await
        .unwrap()
        .to_json();

    let created = result["createdJobs"].as_array().unwrap();
    assert_eq!(created.len(), 1);
    let job_id = created[0]["job"]["job_id"].as_str().unwrap();
    let job = runtime.store.get_job(job_id).await.unwrap();
    assert_eq!(
        job.text_delivery_payload().unwrap().content,
        "Blake is present and can hear."
    );
    assert_eq!(
        runtime
            .store
            .get_automation(&record.automation_id)
            .await
            .unwrap()
            .state,
        AutomationState::Expired
    );
}

#[tokio::test(flavor = "current_thread")]
async fn event_room_participants_exposes_voice_state_flags_for_transition_conditions() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    insert_agent_source_job(&store).await;
    let record = store
        .create_automation(
            AutomationSpec::from_json(&spec_value(json!({
                "name": "undeafened join watcher",
                "idempotency_key": "job_1:event-room-participant-flags",
                "trigger": {"kind": "event", "event_kinds": ["participant_joined"]},
                "condition": {
                    "kind": "all",
                    "conditions": [
                        {"kind": "predicate", "path": "event.user_id", "op": "eq", "value": "blake"},
                        {"kind": "predicate", "path": "event_room.after.participants.blake.present", "op": "eq", "value": true},
                        {"kind": "predicate", "path": "event_room.after.participants.blake.deaf", "op": "eq", "value": false},
                        {"kind": "predicate", "path": "event_room.after.participants.blake.self_deaf", "op": "eq", "value": false}
                    ]
                },
                "actions": [{
                    "kind": "response.send",
                    "sink": {"kind": "agent_chat"},
                    "content": "Blake joined undeafened."
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
    let runtime = test_runtime(store);

    let result = clankcord::domain::automations::engine::run_automations(&runtime)
        .await
        .unwrap()
        .to_json();

    let created = result["createdJobs"].as_array().unwrap();
    assert_eq!(created.len(), 1);
    let job_id = created[0]["job"]["job_id"].as_str().unwrap();
    let job = runtime.store.get_job(job_id).await.unwrap();
    assert_eq!(
        job.text_delivery_payload().unwrap().content,
        "Blake joined undeafened."
    );
    assert_eq!(
        runtime
            .store
            .get_automation(&record.automation_id)
            .await
            .unwrap()
            .state,
        AutomationState::Expired
    );
}

#[tokio::test(flavor = "current_thread")]
async fn room_state_changed_trigger_fires_for_participant_deafen_changes() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    insert_agent_source_job(&store).await;
    store
        .record_voice_state_update(
            None,
            voice_state_with_flags("code", "blake", "Blake", false, false, false, true),
        )
        .await
        .unwrap();
    let record = store
        .create_automation(
            AutomationSpec::from_json(&spec_value(json!({
                "name": "undeafen watcher",
                "idempotency_key": "job_1:room-state-deafen",
                "trigger": {"kind": "room_state_changed"},
                "condition": {
                    "kind": "all",
                    "conditions": [
                        {"kind": "predicate", "path": "event.kind", "op": "eq", "value": "participant_deafen_changed"},
                        {"kind": "predicate", "path": "event.user_id", "op": "eq", "value": "blake"},
                        {"kind": "predicate", "path": "event.self_deaf", "op": "eq", "value": false}
                    ]
                },
                "actions": [{
                    "kind": "response.send",
                    "sink": {"kind": "agent_chat"},
                    "content": "Blake undeafened."
                }]
            })))
            .unwrap(),
        )
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    store
        .record_voice_state_update(
            None,
            voice_state_with_flags("code", "blake", "Blake", false, false, false, false),
        )
        .await
        .unwrap();
    let runtime = test_runtime(store);

    let result = clankcord::domain::automations::engine::run_automations(&runtime)
        .await
        .unwrap()
        .to_json();

    let created = result["createdJobs"].as_array().unwrap();
    assert_eq!(created.len(), 1);
    let job_id = created[0]["job"]["job_id"].as_str().unwrap();
    let job = runtime.store.get_job(job_id).await.unwrap();
    assert_eq!(
        job.text_delivery_payload().unwrap().content,
        "Blake undeafened."
    );
    assert_eq!(
        runtime
            .store
            .get_automation(&record.automation_id)
            .await
            .unwrap()
            .state,
        AutomationState::Expired
    );
}

#[tokio::test(flavor = "current_thread")]
async fn room_state_changed_trigger_fires_for_participant_mute_changes() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    insert_agent_source_job(&store).await;
    store
        .record_voice_state_update(
            None,
            voice_state_with_flags("code", "blake", "Blake", false, false, true, false),
        )
        .await
        .unwrap();
    let record = store
        .create_automation(
            AutomationSpec::from_json(&spec_value(json!({
                "name": "unmute watcher",
                "idempotency_key": "job_1:room-state-mute",
                "trigger": {"kind": "room_state_changed"},
                "condition": {
                    "kind": "all",
                    "conditions": [
                        {"kind": "predicate", "path": "event.kind", "op": "eq", "value": "participant_mute_changed"},
                        {"kind": "predicate", "path": "event.user_id", "op": "eq", "value": "blake"},
                        {"kind": "predicate", "path": "event.self_mute", "op": "eq", "value": false}
                    ]
                },
                "actions": [{
                    "kind": "response.send",
                    "sink": {"kind": "agent_chat"},
                    "content": "Blake unmuted."
                }]
            })))
            .unwrap(),
        )
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    store
        .record_voice_state_update(
            None,
            voice_state_with_flags("code", "blake", "Blake", false, false, false, false),
        )
        .await
        .unwrap();
    let runtime = test_runtime(store);

    let result = clankcord::domain::automations::engine::run_automations(&runtime)
        .await
        .unwrap()
        .to_json();

    let created = result["createdJobs"].as_array().unwrap();
    assert_eq!(created.len(), 1);
    let job_id = created[0]["job"]["job_id"].as_str().unwrap();
    let job = runtime.store.get_job(job_id).await.unwrap();
    assert_eq!(
        job.text_delivery_payload().unwrap().content,
        "Blake unmuted."
    );
    assert_eq!(
        runtime
            .store
            .get_automation(&record.automation_id)
            .await
            .unwrap()
            .state,
        AutomationState::Expired
    );
}

#[tokio::test(flavor = "current_thread")]
async fn recurring_event_automation_processes_all_matching_events_seen_in_one_pass() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    insert_agent_source_job(&store).await;
    let record = store
        .create_automation(
            AutomationSpec::from_json(&spec_value(json!({
                "name": "multi event watcher",
                "idempotency_key": "job_1:multi-event",
                "trigger": {"kind": "event", "event_kinds": ["room.member_joined"]},
                "condition": {
                    "kind": "predicate",
                    "path": "event.speaker_user_id",
                    "op": "eq",
                    "value": "blake"
                },
                "expiry": {"max_fires": 2},
                "actions": [{
                    "kind": "response.send",
                    "sink": {"kind": "agent_chat"},
                    "content": "Blake joined."
                }]
            })))
            .unwrap(),
        )
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    append_speech(&store, "room.member_joined", "blake", "Blake joined", 1).await;
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    append_speech(
        &store,
        "room.member_joined",
        "blake",
        "Blake joined again",
        2,
    )
    .await;
    let runtime = test_runtime(store);

    let result = clankcord::domain::automations::engine::run_automations(&runtime)
        .await
        .unwrap()
        .to_json();

    let created = result["createdJobs"].as_array().unwrap();
    assert_eq!(created.len(), 2);
    let updated = runtime
        .store
        .get_automation(&record.automation_id)
        .await
        .unwrap();
    assert_eq!(updated.fire_count, 2);
    assert_eq!(updated.state, AutomationState::Expired);
}

#[tokio::test(flavor = "current_thread")]
async fn recurring_job_automation_processes_all_matching_jobs_seen_in_one_pass() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    insert_agent_source_job(&store).await;
    let record = store
        .create_automation(
            AutomationSpec::from_json(&spec_value(json!({
                "name": "multi job watcher",
                "idempotency_key": "job_1:multi-job",
                "trigger": {
                    "kind": "job",
                    "job_kinds": ["text_delivery"],
                    "states": ["complete"]
                },
                "condition": {"kind": "true"},
                "expiry": {"max_fires": 2},
                "actions": [{
                    "kind": "response.send",
                    "sink": {"kind": "agent_chat"},
                    "content": "A delivery completed."
                }]
            })))
            .unwrap(),
        )
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    create_completed_text_delivery(&store, "source-delivery-1", "first").await;
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    create_completed_text_delivery(&store, "source-delivery-2", "second").await;
    let runtime = test_runtime(store);

    let result = clankcord::domain::automations::engine::run_automations(&runtime)
        .await
        .unwrap()
        .to_json();

    let created = result["createdJobs"].as_array().unwrap();
    assert_eq!(created.len(), 2);
    let updated = runtime
        .store
        .get_automation(&record.automation_id)
        .await
        .unwrap();
    assert_eq!(updated.fire_count, 2);
    assert_eq!(updated.state, AutomationState::Expired);
}

#[tokio::test(flavor = "current_thread")]
async fn event_room_snapshot_matches_presence_at_transition_time() {
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
                "name": "joined while present watcher",
                "idempotency_key": "job_1:event-room-snapshot",
                "trigger": {"kind": "event", "event_kinds": ["participant_joined"]},
                "condition": {
                    "kind": "all",
                    "conditions": [
                        {"kind": "predicate", "path": "event.user_id", "op": "eq", "value": "blake"},
                        {"kind": "predicate", "path": "event_room.before.participants.user-a.present", "op": "eq", "value": true}
                    ]
                },
                "actions": [{
                    "kind": "response.send",
                    "sink": {"kind": "agent_chat"},
                    "content": "Blake joined while Will was present."
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
    store
        .record_voice_state_update(None, voice_state("", "user-a", "Will"))
        .await
        .unwrap();
    let runtime = test_runtime(store);

    let result = clankcord::domain::automations::engine::run_automations(&runtime)
        .await
        .unwrap()
        .to_json();

    let created = result["createdJobs"].as_array().unwrap();
    assert_eq!(created.len(), 1);
    let job_id = created[0]["job"]["job_id"].as_str().unwrap();
    let job = runtime.store.get_job(job_id).await.unwrap();
    let payload = job.text_delivery_payload().unwrap();
    assert_eq!(payload.content, "Blake joined while Will was present.");
    assert_eq!(
        runtime
            .store
            .get_automation(&record.automation_id)
            .await
            .unwrap()
            .state,
        AutomationState::Expired
    );
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
    let runtime = test_runtime(store);

    let first = clankcord::domain::automations::engine::run_automations(&runtime)
        .await
        .unwrap()
        .to_json();
    assert!(first["createdJobs"].as_array().unwrap().is_empty());
    assert_eq!(
        runtime
            .store
            .get_automation(&record.automation_id)
            .await
            .unwrap()
            .state,
        AutomationState::Active
    );

    append_speech(
        &runtime.store,
        "room.member_joined",
        "blake",
        "Blake joined",
        2,
    )
    .await;
    let second = clankcord::domain::automations::engine::run_automations(&runtime)
        .await
        .unwrap()
        .to_json();
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
    let runtime = test_runtime(store);

    let first = clankcord::domain::automations::engine::run_automations(&runtime)
        .await
        .unwrap()
        .to_json();
    let second = clankcord::domain::automations::engine::run_automations(&runtime)
        .await
        .unwrap()
        .to_json();

    assert_eq!(first["createdJobs"].as_array().unwrap().len(), 1);
    assert!(second["createdJobs"].as_array().unwrap().is_empty());
    assert_eq!(
        runtime
            .store
            .get_automation(&record.automation_id)
            .await
            .unwrap()
            .state,
        AutomationState::Active
    );

    append_speech(
        &runtime.store,
        "room.member_joined",
        "blake",
        "Blake joined again",
        2,
    )
    .await;
    let third = clankcord::domain::automations::engine::run_automations(&runtime)
        .await
        .unwrap()
        .to_json();
    assert_eq!(third["createdJobs"].as_array().unwrap().len(), 1);
    assert_eq!(
        runtime
            .store
            .get_automation(&record.automation_id)
            .await
            .unwrap()
            .state,
        AutomationState::Expired
    );
}

#[tokio::test(flavor = "current_thread")]
async fn delayed_recheck_waits_and_fires_when_condition_still_matches() {
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
                "name": "still away watcher",
                "idempotency_key": "job_1:delayed-recheck",
                "trigger": {"kind": "event", "event_kinds": ["participant_left"]},
                "condition": {
                    "kind": "predicate",
                    "path": "event.user_id",
                    "op": "eq",
                    "value": "blake"
                },
                "delay": {
                    "seconds": 1,
                    "condition": {
                        "kind": "predicate",
                        "path": "room.participants.blake.present",
                        "op": "empty"
                    }
                },
                "actions": [{
                    "kind": "response.send",
                    "sink": {"kind": "agent_chat"},
                    "content": "Blake is still away."
                }]
            })))
            .unwrap(),
        )
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    store
        .record_voice_state_update(None, voice_state("", "blake", "Blake"))
        .await
        .unwrap();
    let runtime = test_runtime(store);

    let first = clankcord::domain::automations::engine::run_automations(&runtime)
        .await
        .unwrap()
        .to_json();
    assert!(first["createdJobs"].as_array().unwrap().is_empty());
    assert!(
        runtime
            .store
            .get_automation(&record.automation_id)
            .await
            .unwrap()
            .pending_recheck
            .is_some()
    );

    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let second = clankcord::domain::automations::engine::run_automations(&runtime)
        .await
        .unwrap()
        .to_json();

    let created = second["createdJobs"].as_array().unwrap();
    assert_eq!(created.len(), 1);
    let job_id = created[0]["job"]["job_id"].as_str().unwrap();
    let job = runtime.store.get_job(job_id).await.unwrap();
    let payload = job.text_delivery_payload().unwrap();
    assert_eq!(payload.content, "Blake is still away.");
    let updated = runtime
        .store
        .get_automation(&record.automation_id)
        .await
        .unwrap();
    assert!(updated.pending_recheck.is_none());
    assert_eq!(updated.state, AutomationState::Expired);
}

#[tokio::test(flavor = "current_thread")]
async fn delayed_recheck_does_not_duplicate_work_before_due() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    insert_agent_source_job(&store).await;
    store
        .record_voice_state_update(None, voice_state("code", "blake", "Blake"))
        .await
        .unwrap();
    let record = store
        .create_automation(still_away_spec("job_1:delayed-before-due", 1))
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    store
        .record_voice_state_update(None, voice_state("", "blake", "Blake"))
        .await
        .unwrap();
    let runtime = test_runtime(store);

    let first = clankcord::domain::automations::engine::run_automations(&runtime)
        .await
        .unwrap()
        .to_json();
    let pending = runtime
        .store
        .get_automation(&record.automation_id)
        .await
        .unwrap()
        .pending_recheck
        .expect("first evaluation stores delayed recheck");
    let second = clankcord::domain::automations::engine::run_automations(&runtime)
        .await
        .unwrap()
        .to_json();
    let after_second = runtime
        .store
        .get_automation(&record.automation_id)
        .await
        .unwrap();

    assert!(first["createdJobs"].as_array().unwrap().is_empty());
    assert!(second["createdJobs"].as_array().unwrap().is_empty());
    let still_pending = after_second
        .pending_recheck
        .expect("second evaluation before due keeps delayed recheck");
    assert_eq!(pending.due_at, still_pending.due_at);
    assert!(
        still_pending
            .event_json
            .as_deref()
            .unwrap()
            .contains("participant_left")
    );
    assert_eq!(after_second.fire_count, 0);
    assert_eq!(after_second.state, AutomationState::Active);
}

#[tokio::test(flavor = "current_thread")]
async fn delayed_recheck_skips_when_condition_changes_and_allows_future_trigger() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    insert_agent_source_job(&store).await;
    store
        .record_voice_state_update(None, voice_state("code", "blake", "Blake"))
        .await
        .unwrap();
    let record = store
        .create_automation(still_away_spec("job_1:delayed-returned", 1))
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    store
        .record_voice_state_update(None, voice_state("", "blake", "Blake"))
        .await
        .unwrap();
    let runtime = test_runtime(store);

    let first = clankcord::domain::automations::engine::run_automations(&runtime)
        .await
        .unwrap()
        .to_json();
    runtime
        .store
        .record_voice_state_update(None, voice_state("code", "blake", "Blake"))
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let second = clankcord::domain::automations::engine::run_automations(&runtime)
        .await
        .unwrap()
        .to_json();
    let active_after_skip = runtime
        .store
        .get_automation(&record.automation_id)
        .await
        .unwrap();

    assert!(first["createdJobs"].as_array().unwrap().is_empty());
    assert!(second["createdJobs"].as_array().unwrap().is_empty());
    assert!(active_after_skip.pending_recheck.is_none());
    assert_eq!(active_after_skip.fire_count, 0);
    assert_eq!(active_after_skip.state, AutomationState::Active);

    runtime
        .store
        .record_voice_state_update(None, voice_state("", "blake", "Blake"))
        .await
        .unwrap();
    let third = clankcord::domain::automations::engine::run_automations(&runtime)
        .await
        .unwrap()
        .to_json();
    assert!(third["createdJobs"].as_array().unwrap().is_empty());
    assert!(
        runtime
            .store
            .get_automation(&record.automation_id)
            .await
            .unwrap()
            .pending_recheck
            .is_some()
    );
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let fourth = clankcord::domain::automations::engine::run_automations(&runtime)
        .await
        .unwrap()
        .to_json();

    let created = fourth["createdJobs"].as_array().unwrap();
    assert_eq!(created.len(), 1);
    let updated = runtime
        .store
        .get_automation(&record.automation_id)
        .await
        .unwrap();
    assert_eq!(updated.fire_count, 1);
    assert_eq!(updated.state, AutomationState::Expired);
}

#[tokio::test(flavor = "current_thread")]
async fn delayed_recheck_survives_fresh_runtime_context() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    insert_agent_source_job(&store).await;
    store
        .record_voice_state_update(None, voice_state("code", "blake", "Blake"))
        .await
        .unwrap();
    let record = store
        .create_automation(still_away_spec("job_1:delayed-restart", 1))
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    store
        .record_voice_state_update(None, voice_state("", "blake", "Blake"))
        .await
        .unwrap();
    let first_runtime = test_runtime(store);
    clankcord::domain::automations::engine::run_automations(&first_runtime)
        .await
        .unwrap();
    let store = first_runtime.store.clone();
    drop(first_runtime);

    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let restarted = test_runtime(store);
    let result = clankcord::domain::automations::engine::run_automations(&restarted)
        .await
        .unwrap()
        .to_json();

    let created = result["createdJobs"].as_array().unwrap();
    assert_eq!(created.len(), 1);
    let updated = restarted
        .store
        .get_automation(&record.automation_id)
        .await
        .unwrap();
    assert!(updated.pending_recheck.is_none());
    assert_eq!(updated.state, AutomationState::Expired);
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
            "job_kinds": ["text_delivery"],
            "states": ["complete"]
        },
        "condition": {"kind": "true"},
        "actions": [{
            "kind": "agent_task.start",
            "prompt": "Summarize the completed text delivery job."
        }]
    })))
    .unwrap();
    store.create_automation(spec).await.unwrap();
    let mut completed_delivery = Job::text_delivery(
        RuntimeScope::voice_channel("guild", "code"),
        "user-a",
        TextDeliveryPayload::new(
            TextDeliveryKind::Message,
            TextTarget::default(),
            "done",
            "source-job",
            "user-a",
            false,
        ),
    );
    completed_delivery = store.create_job(completed_delivery).await.unwrap();
    completed_delivery.mark_complete();
    store.update_job(&completed_delivery).await.unwrap();
    let runtime = test_runtime(store);

    let result = clankcord::domain::automations::engine::run_automations(&runtime)
        .await
        .unwrap()
        .to_json();

    let created = result["createdJobs"].as_array().unwrap();
    assert_eq!(created.len(), 1);
    let job_id = created[0]["job"]["job_id"].as_str().unwrap();
    let job = runtime.store.get_job(job_id).await.unwrap();
    assert_eq!(job.kind, JobKind::Command);
    assert_eq!(
        job.command().unwrap().arguments.request,
        "Summarize the completed text delivery job."
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
    let runtime = test_runtime(store);

    let result = clankcord::domain::automations::engine::run_automations(&runtime)
        .await
        .unwrap()
        .to_json();

    assert!(result["createdJobs"].as_array().unwrap().is_empty());
    let failures = runtime
        .store
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

fn still_away_spec(idempotency_key: &str, delay_seconds: u64) -> AutomationSpec {
    AutomationSpec::from_json(&spec_value(json!({
        "name": "still away watcher",
        "idempotency_key": idempotency_key,
        "trigger": {"kind": "event", "event_kinds": ["participant_left"]},
        "condition": {
            "kind": "predicate",
            "path": "event.user_id",
            "op": "eq",
            "value": "blake"
        },
        "delay": {
            "seconds": delay_seconds,
            "condition": {
                "kind": "predicate",
                "path": "room.participants.blake.present",
                "op": "empty"
            }
        },
        "actions": [{
            "kind": "response.send",
            "sink": {"kind": "agent_chat"},
            "content": "Blake is still away."
        }]
    })))
    .unwrap()
}

fn spec_value_replacing(key: &str, replacement: Value) -> Value {
    let mut value = spec_value(json!({}));
    value[key] = replacement;
    value
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

async fn create_completed_text_delivery(store: &TimelineStore, source_job_id: &str, content: &str) {
    let mut delivery = Job::text_delivery(
        RuntimeScope::voice_channel("guild", "code"),
        "user-a",
        TextDeliveryPayload::new(
            TextDeliveryKind::Message,
            TextTarget::default(),
            content,
            source_job_id,
            "user-a",
            false,
        ),
    );
    delivery = store.create_job(delivery).await.unwrap();
    delivery.mark_complete();
    store.update_job(&delivery).await.unwrap();
}
