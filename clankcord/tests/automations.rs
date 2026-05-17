use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use clankcord::runtime::automations::{
    AutomationAction, AutomationCondition, AutomationDelay, AutomationExpiry, AutomationOwner,
    AutomationPendingRecheck, AutomationRecord, AutomationSpec, AutomationState,
    AutomationTextTargetKind, AutomationTrigger,
};
use clankcord::runtime::timeline::TimelineStore;
use clankcord::runtime::{
    CommandRequest, Job, JobKind, JobState, Runtime, RuntimeScope, TextDeliveryKind,
    TextDeliveryPayload, TextTarget, TextTargetKind, VoiceBotStatus,
};

mod common;
use common::test_store;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PreV0_3_0AutomationScope {
    guild_id: String,
    voice_channel_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct PreV0_3_0AutomationSpec {
    schema: String,
    name: String,
    idempotency_key: String,
    owner: AutomationOwner,
    scope: PreV0_3_0AutomationScope,
    trigger: AutomationTrigger,
    condition: AutomationCondition,
    delay: Option<AutomationDelay>,
    expiry: AutomationExpiry,
    actions: Vec<AutomationAction>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct PreV0_3_0AutomationRecord {
    automation_id: String,
    state: AutomationState,
    created_at: String,
    updated_at: String,
    last_evaluated_at: String,
    last_fired_at: String,
    fire_count: u64,
    pending_recheck: Option<AutomationPendingRecheck>,
    spec: PreV0_3_0AutomationSpec,
}

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
async fn v0_3_0_schema_migration_rewrites_legacy_automation_scope_projection_and_blob() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    insert_agent_source_job(&store).await;
    let record = store
        .create_automation(reminder_spec("job_1:migrate-automation"))
        .await
        .unwrap();
    let legacy_blob = bincode::serialize(&PreV0_3_0AutomationRecord::from_current(&record))
        .expect("legacy automation record serializes");

    sqlx::raw_sql(
        r#"
        ALTER TABLE automations ADD COLUMN voice_channel_id TEXT NOT NULL DEFAULT '';
        UPDATE automations SET voice_channel_id = scope_id;
        ALTER TABLE automations DROP COLUMN scope_kind CASCADE;
        ALTER TABLE automations DROP COLUMN scope_id CASCADE;
        "#,
    )
    .execute(&store.pool)
    .await
    .unwrap();
    sqlx::query("UPDATE automations SET payload_blob = $1 WHERE automation_id = $2")
        .bind(legacy_blob)
        .bind(&record.automation_id)
        .execute(&store.pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM clankcord_schema_migrations WHERE version IN ('0.3.0', '0.4.0')")
        .execute(&store.pool)
        .await
        .unwrap();

    let applied = store.run_pending_schema_migrations().await.unwrap();

    assert_eq!(applied.len(), 2);
    assert_eq!(applied[0].version, "0.3.0");
    assert_eq!(applied[1].version, "0.4.0");
    assert!(!column_exists(&store.pool, "automations", "voice_channel_id").await);
    let row = sqlx::query("SELECT scope_kind, scope_id FROM automations WHERE automation_id = $1")
        .bind(&record.automation_id)
        .fetch_one(&store.pool)
        .await
        .unwrap();
    assert_eq!(
        sqlx::Row::try_get::<String, _>(&row, "scope_kind").unwrap(),
        "voice_channel"
    );
    assert_eq!(
        sqlx::Row::try_get::<String, _>(&row, "scope_id").unwrap(),
        "code"
    );
    let migrated = store.get_automation(&record.automation_id).await.unwrap();
    assert_eq!(migrated.spec.scope.scope_kind, "voice_channel");
    assert_eq!(migrated.spec.scope.guild_id, "guild");
    assert_eq!(migrated.spec.scope.scope_id, "code");
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
            .timeline_store
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
    let mut runtime = test_runtime(store);

    let result = runtime.run_automations().await.unwrap().to_json();

    let created = result["createdJobs"].as_array().unwrap();
    assert_eq!(created.len(), 1);
    let job_id = created[0]["job"]["job_id"].as_str().unwrap();
    let job = runtime.timeline_store.get_job(job_id).await.unwrap();
    let payload = job.text_delivery_payload().unwrap();
    assert_eq!(payload.target.kind, TextTargetKind::AgentChat);
    assert_eq!(payload.content, "Blake joined.");
    assert_eq!(payload.source_job_id, record.automation_id);
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
async fn room_placement_builtin_automation_is_disabled() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    store.upsert_voice_bot_state(&ready_bot()).await.unwrap();
    let mut runtime = test_runtime(store.clone());

    let result = runtime.run_automations().await.unwrap().to_json();

    assert_eq!(result["createdJobs"], json!([]));
    assert!(
        store
            .list_jobs_by_scope_kind("guild", "code", JobKind::RoomAgentPlacement)
            .await
            .unwrap()
            .is_empty()
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

    let result = runtime.run_automations().await.unwrap().to_json();

    let created = result["createdJobs"].as_array().unwrap();
    assert_eq!(created.len(), 1);
    let job_id = created[0]["job"]["job_id"].as_str().unwrap();
    let job = runtime.timeline_store.get_job(job_id).await.unwrap();
    let payload = job.text_delivery_payload().unwrap();
    assert_eq!(payload.content, "Blake left.");
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

    let result = runtime.run_automations().await.unwrap().to_json();

    let created = result["createdJobs"].as_array().unwrap();
    assert_eq!(created.len(), 1);
    let job_id = created[0]["job"]["job_id"].as_str().unwrap();
    let job = runtime.timeline_store.get_job(job_id).await.unwrap();
    let payload = job.text_delivery_payload().unwrap();
    assert_eq!(payload.target.kind, TextTargetKind::Dm);
    assert_eq!(payload.target.user_id, "user-a");
    assert_eq!(payload.content, "Reminder: talk to Blake about Woven.");
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
    let mut runtime = test_runtime(store);

    let result = runtime.run_automations().await.unwrap().to_json();

    let created = result["createdJobs"].as_array().unwrap();
    assert_eq!(created.len(), 1);
    let job_id = created[0]["job"]["job_id"].as_str().unwrap();
    let job = runtime.timeline_store.get_job(job_id).await.unwrap();
    let payload = job.text_delivery_payload().unwrap();
    assert_eq!(payload.content, "Blake joined while Will was present.");
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

    let first = runtime.run_automations().await.unwrap().to_json();
    assert!(first["createdJobs"].as_array().unwrap().is_empty());
    assert_eq!(
        runtime
            .timeline_store
            .get_automation(&record.automation_id)
            .await
            .unwrap()
            .state,
        AutomationState::Active
    );

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

    let first = runtime.run_automations().await.unwrap().to_json();
    let second = runtime.run_automations().await.unwrap().to_json();

    assert_eq!(first["createdJobs"].as_array().unwrap().len(), 1);
    assert!(second["createdJobs"].as_array().unwrap().is_empty());
    assert_eq!(
        runtime
            .timeline_store
            .get_automation(&record.automation_id)
            .await
            .unwrap()
            .state,
        AutomationState::Active
    );

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
    let mut runtime = test_runtime(store);

    let first = runtime.run_automations().await.unwrap().to_json();
    assert!(first["createdJobs"].as_array().unwrap().is_empty());
    assert!(
        runtime
            .timeline_store
            .get_automation(&record.automation_id)
            .await
            .unwrap()
            .pending_recheck
            .is_some()
    );

    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let second = runtime.run_automations().await.unwrap().to_json();

    let created = second["createdJobs"].as_array().unwrap();
    assert_eq!(created.len(), 1);
    let job_id = created[0]["job"]["job_id"].as_str().unwrap();
    let job = runtime.timeline_store.get_job(job_id).await.unwrap();
    let payload = job.text_delivery_payload().unwrap();
    assert_eq!(payload.content, "Blake is still away.");
    let updated = runtime
        .timeline_store
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
    let mut runtime = test_runtime(store);

    let first = runtime.run_automations().await.unwrap().to_json();
    let pending = runtime
        .timeline_store
        .get_automation(&record.automation_id)
        .await
        .unwrap()
        .pending_recheck
        .expect("first evaluation stores delayed recheck");
    let second = runtime.run_automations().await.unwrap().to_json();
    let after_second = runtime
        .timeline_store
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
    let mut runtime = test_runtime(store);

    let first = runtime.run_automations().await.unwrap().to_json();
    runtime
        .timeline_store
        .record_voice_state_update(None, voice_state("code", "blake", "Blake"))
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let second = runtime.run_automations().await.unwrap().to_json();
    let active_after_skip = runtime
        .timeline_store
        .get_automation(&record.automation_id)
        .await
        .unwrap();

    assert!(first["createdJobs"].as_array().unwrap().is_empty());
    assert!(second["createdJobs"].as_array().unwrap().is_empty());
    assert!(active_after_skip.pending_recheck.is_none());
    assert_eq!(active_after_skip.fire_count, 0);
    assert_eq!(active_after_skip.state, AutomationState::Active);

    runtime
        .timeline_store
        .record_voice_state_update(None, voice_state("", "blake", "Blake"))
        .await
        .unwrap();
    let third = runtime.run_automations().await.unwrap().to_json();
    assert!(third["createdJobs"].as_array().unwrap().is_empty());
    assert!(
        runtime
            .timeline_store
            .get_automation(&record.automation_id)
            .await
            .unwrap()
            .pending_recheck
            .is_some()
    );
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let fourth = runtime.run_automations().await.unwrap().to_json();

    let created = fourth["createdJobs"].as_array().unwrap();
    assert_eq!(created.len(), 1);
    let updated = runtime
        .timeline_store
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
    let mut first_runtime = test_runtime(store);
    first_runtime.run_automations().await.unwrap();
    let store = first_runtime.timeline_store.clone();
    drop(first_runtime);

    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let mut restarted = test_runtime(store);
    let result = restarted.run_automations().await.unwrap().to_json();

    let created = result["createdJobs"].as_array().unwrap();
    assert_eq!(created.len(), 1);
    let updated = restarted
        .timeline_store
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
    let mut runtime = test_runtime(store);

    let result = runtime.run_automations().await.unwrap().to_json();

    let created = result["createdJobs"].as_array().unwrap();
    assert_eq!(created.len(), 1);
    let job_id = created[0]["job"]["job_id"].as_str().unwrap();
    let job = runtime.timeline_store.get_job(job_id).await.unwrap();
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
    let mut runtime = test_runtime(store);

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
        "scope": {"scope_kind": "voice_channel", "guild_id": "guild", "scope_id": "code"},
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

fn spec_value(overrides: Value) -> Value {
    let mut base = json!({
        "schema": "clankcord.automation.v0",
        "name": "test automation",
        "idempotency_key": "test:auto",
        "owner": {"kind": "agent", "user_id": "user-a", "source_job_id": "job_1"},
        "scope": {"scope_kind": "voice_channel", "guild_id": "guild", "scope_id": "code"},
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

impl PreV0_3_0AutomationRecord {
    fn from_current(record: &AutomationRecord) -> Self {
        Self {
            automation_id: record.automation_id.clone(),
            state: record.state,
            created_at: record.created_at.clone(),
            updated_at: record.updated_at.clone(),
            last_evaluated_at: record.last_evaluated_at.clone(),
            last_fired_at: record.last_fired_at.clone(),
            fire_count: record.fire_count,
            pending_recheck: record.pending_recheck.clone(),
            spec: PreV0_3_0AutomationSpec {
                schema: record.spec.schema.clone(),
                name: record.spec.name.clone(),
                idempotency_key: record.spec.idempotency_key.clone(),
                owner: record.spec.owner.clone(),
                scope: PreV0_3_0AutomationScope {
                    guild_id: record.spec.scope.guild_id.clone(),
                    voice_channel_id: record.spec.scope.scope_id.clone(),
                },
                trigger: record.spec.trigger.clone(),
                condition: record.spec.condition.clone(),
                delay: record.spec.delay.clone(),
                expiry: record.spec.expiry.clone(),
                actions: record.spec.actions.clone(),
            },
        }
    }
}

async fn column_exists(pool: &sqlx::PgPool, table: &str, column: &str) -> bool {
    let row = sqlx::query(
        r#"
        SELECT EXISTS (
          SELECT 1
          FROM information_schema.columns
          WHERE table_schema = current_schema()
            AND table_name = $1
            AND column_name = $2
        ) AS exists
        "#,
    )
    .bind(table)
    .bind(column)
    .fetch_one(pool)
    .await
    .unwrap();
    sqlx::Row::try_get(&row, "exists").unwrap()
}

fn test_runtime(timeline_store: TimelineStore) -> Runtime {
    Runtime::from_store(timeline_store).unwrap()
}

async fn insert_agent_source_job(store: &TimelineStore) {
    let mut job = Job::agent_task_for_session(
        "ags_source",
        RuntimeScope::voice_channel("guild", "code"),
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

fn ready_bot() -> VoiceBotStatus {
    VoiceBotStatus {
        bot_id: "clanky-vc1".to_string(),
        ready: true,
        current_guild_id: String::new(),
        current_channel_id: String::new(),
        last_error: String::new(),
        pending_disconnect_events: 0,
        pending_disconnect_until: 0,
        user_id: "bot-user".to_string(),
        username: "Clanky".to_string(),
        gateway_running: true,
        receive_backend: "songbird".to_string(),
    }
}
