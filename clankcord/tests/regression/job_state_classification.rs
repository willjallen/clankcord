//! Pins for the single job-state classification: views re-derived
//! failed/active through a substring heuristic and hardcoded state lists
//! that dropped approved jobs from active counts on some panels (d9fb15d).

use crate::support::initialize_test_config;
use crate::support::test_store;
use chrono::Duration;
use chrono::Utc;
use clankcord::domain::Ctx;
use clankcord::model::job::{CommandRequest, Job, JobState};
use clankcord::model::scope::RuntimeScope;
use clankcord::views::{DashboardAgentsRequest, DashboardOverviewRequest};
use serde_json::json;

const ALL_STATES: [JobState; 12] = [
    JobState::Queued,
    JobState::Running,
    JobState::Waiting,
    JobState::Complete,
    JobState::Cancelled,
    JobState::CancelRequested,
    JobState::ConfirmationPending,
    JobState::Approved,
    JobState::ApprovalFailed,
    JobState::Failed,
    JobState::FailedTimeout,
    JobState::FailedDraftRetained,
];

#[test]
fn failed_states_are_exactly_the_terminal_failures() {
    let failed: Vec<JobState> = ALL_STATES
        .into_iter()
        .filter(|state| state.is_failed())
        .collect();
    assert_eq!(
        failed,
        vec![
            JobState::ApprovalFailed,
            JobState::Failed,
            JobState::FailedTimeout,
            JobState::FailedDraftRetained,
        ]
    );
    for state in failed {
        assert!(state.is_terminal(), "{state} is failed but not terminal");
    }
    assert!(!JobState::Approved.is_terminal());
    assert!(!JobState::Approved.is_cancellable());
}

fn agent_task(session: &str, request: &str) -> Job {
    Job::agent_task_for_session(
        session,
        RuntimeScope::voice_channel("guild", "code"),
        "operator",
        CommandRequest::agent_task("guild", "code", "operator", request),
    )
}

#[tokio::test(flavor = "current_thread")]
async fn approved_jobs_count_active_on_every_dashboard_panel() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(raw.path()).await;
    let now = Utc::now();

    let mut approved = agent_task("approved-session", "approved task");
    approved.set_state(JobState::Approved);
    approved.created_at = now - Duration::minutes(5);
    approved.updated_at = approved.created_at;
    store.create_job(approved).await.unwrap();

    let mut retained = agent_task("retained-session", "failed task");
    retained.set_state(JobState::FailedDraftRetained);
    retained.created_at = now - Duration::minutes(4);
    retained.updated_at = retained.created_at;
    retained.completed_at = Some(retained.created_at);
    store.create_job(retained).await.unwrap();

    let runtime = Ctx::new(store);
    let overview = clankcord::views::dashboard::dashboard_overview(
        &runtime,
        DashboardOverviewRequest { jobs_limit: 5 },
    )
    .await
    .unwrap();
    let summary = &overview["jobs"]["summary"];
    assert_eq!(summary["total"], json!(2), "summary: {summary}");
    assert_eq!(summary["active"], json!(1), "approved is active: {summary}");
    assert_eq!(summary["failed"], json!(1), "retained is failed: {summary}");
    assert_eq!(
        summary["cancellable"],
        json!(0),
        "approved is not cancellable: {summary}"
    );

    let agents = clankcord::views::dashboard::dashboard_agents(
        &runtime,
        DashboardAgentsRequest { limit: 5 },
    )
    .await
    .unwrap();
    let summary = &agents["agents"]["summary"];
    assert_eq!(
        summary["active"],
        json!(1),
        "agents panel agrees: {summary}"
    );
    assert_eq!(
        summary["failed"],
        json!(1),
        "agents panel agrees: {summary}"
    );
}
