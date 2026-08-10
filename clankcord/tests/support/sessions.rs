//! Agent-session fixtures shared across category binaries.

use chrono::SecondsFormat;
use clankcord::domain::agents::AgentSessionRecord;
use clankcord::model::job::CommandRequest;
use clankcord::model::job::Job;
use clankcord::model::job::JobKind;
use clankcord::model::job::JobOutput;
use clankcord::model::job::TextDeliveryKind;
use clankcord::model::job::TextDeliveryOutput;
use clankcord::model::job::TextDeliveryPayload;
use clankcord::model::job::TextTarget;
use clankcord::model::job::TextTargetKind;
use clankcord::model::scope::RuntimeScope;
use clankcord::store::JobVisibility;
use clankcord::store::TimelineStore;

pub async fn insert_active_thread_session(store: &TimelineStore, id: &str) {
    let created_at = crate::support::dt(2026, 5, 17, 3, 28, 0);
    let max_active_until = created_at + chrono::Duration::hours(8);
    store
        .create_agent_session_record(AgentSessionRecord::new_voice(
            id,
            "guild",
            "code",
            "agent-threads",
            "thread-1",
            created_at.to_rfc3339_opts(SecondsFormat::Millis, true),
            max_active_until.to_rfc3339_opts(SecondsFormat::Millis, true),
        ))
        .await
        .unwrap();
}

pub async fn insert_completed_agent_response(
    store: &TimelineStore,
    agent_session_id: &str,
    request: &str,
    response: &str,
    requested_by_user_id: &str,
) {
    let mut task = Job::agent_task_for_session(
        agent_session_id,
        RuntimeScope::voice_channel("guild", "code"),
        requested_by_user_id,
        CommandRequest::agent_task("guild", "code", requested_by_user_id, request),
    );
    task.mark_complete();
    let task = store.create_job(task).await.unwrap();
    let target = TextTarget {
        kind: TextTargetKind::Channel,
        channel_id: "thread-1".to_string(),
        user_id: String::new(),
    };
    let mut delivery = Job::text_delivery(
        RuntimeScope::voice_channel("guild", "code"),
        requested_by_user_id,
        TextDeliveryPayload::new(
            TextDeliveryKind::Message,
            target.clone(),
            response,
            task.id.clone(),
            requested_by_user_id,
            false,
        ),
    );
    delivery.mark_complete();
    delivery.metadata.output = Some(JobOutput::TextDelivery(TextDeliveryOutput {
        intent: TextDeliveryKind::Message.as_str().to_string(),
        target,
        source_job_id: task.id,
        discord_post: None,
    }));
    store.create_job(delivery).await.unwrap();
}

pub async fn agent_thread_title_refresh_jobs(store: &TimelineStore) -> Vec<Job> {
    store
        .list_jobs_with_visibility(None, None, JobVisibility::IncludeEphemeral)
        .await
        .unwrap()
        .into_iter()
        .filter(|job| job.kind == JobKind::AgentThreadTitleRefresh)
        .collect()
}
