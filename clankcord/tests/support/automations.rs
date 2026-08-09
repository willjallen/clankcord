//! Automation fixtures shared across category binaries.

use serde_json::{Value, json};

use clankcord::domain::Ctx;
use clankcord::domain::automations::AutomationSpec;
use clankcord::model::job::{
    CommandRequest, Job,
};
use clankcord::model::scope::RuntimeScope;
use clankcord::store::TimelineStore;


pub fn reminder_spec(idempotency_key: &str) -> AutomationSpec {
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
pub fn test_runtime(timeline_store: TimelineStore) -> Ctx {
    Ctx::new(timeline_store)
}
pub async fn insert_agent_source_job(store: &TimelineStore) {
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
pub fn voice_state(voice_channel_id: &str, user_id: &str, display_name: &str) -> Value {
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
pub fn voice_state_with_flags(
    voice_channel_id: &str,
    user_id: &str,
    display_name: &str,
    mute: bool,
    deaf: bool,
    self_mute: bool,
    self_deaf: bool,
) -> Value {
    let mut state = voice_state(voice_channel_id, user_id, display_name);
    state["mute"] = json!(mute);
    state["deaf"] = json!(deaf);
    state["self_mute"] = json!(self_mute);
    state["self_deaf"] = json!(self_deaf);
    state
}
