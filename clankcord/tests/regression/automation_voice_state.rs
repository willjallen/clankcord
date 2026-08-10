//! Pins for automation voice-state fixes: durable transitions fire
//! participant automations, and overlap conditions match current
//! participants (41887e6, 3fe13e6).

use crate::support::automations::insert_agent_source_job;
use crate::support::automations::spec_value;
use crate::support::automations::test_runtime;
use crate::support::automations::voice_state;
use crate::support::test_store;
use clankcord::domain::automations::AutomationSpec;
use clankcord::domain::automations::AutomationState;
use clankcord::model::job::TextTargetKind;
use serde_json::json;

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
    assert_eq!(payload.content, "Blake left.");
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
    assert_eq!(payload.target.kind, TextTargetKind::Dm);
    assert_eq!(payload.target.user_id, "user-a");
    assert_eq!(payload.content, "Reminder: talk to Blake about Woven.");
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
