//! Durable operational outcomes: terminal recording, retry accounting,
//! retention windows, and pruning across ephemeral GC.

use crate::support::initialize_test_config;
use crate::support::test_store;
use chrono::Duration;
use chrono::Utc;
use clankcord::domain::Ctx;
use clankcord::model::job::CommandRequest;
use clankcord::model::job::Job;
use clankcord::model::job::JobState;
use clankcord::model::scope::RuntimeScope;
use clankcord::time::isoformat_z;
use clankcord::views::DashboardOverviewRequest;
use serde_json::json;

#[tokio::test(flavor = "current_thread")]
async fn operational_windows_keep_success_and_failure_outcomes_after_ephemeral_gc() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(raw.path()).await;
    let now = Utc::now();
    let created_at = isoformat_z(Some(now - Duration::minutes(12)));
    let started_at = isoformat_z(Some(now - Duration::minutes(11)));
    let observed_at = isoformat_z(Some(now - Duration::minutes(10)));

    let mut completed = Job::runtime_maintenance(15_000);
    completed.created_at = created_at.clone();
    completed.started_at = Some(started_at.clone());
    completed.mark_complete();
    completed.completed_at = Some(observed_at.clone());
    completed.updated_at = observed_at.clone();
    let completed = store.create_job(completed).await.unwrap();

    let mut failed = Job::runtime_maintenance(15_000);
    failed.created_at = created_at;
    failed.started_at = Some(started_at);
    failed.set_state(JobState::Failed);
    failed.updated_at = observed_at;
    failed.metadata.error = "maintenance snapshot provider timed out".to_string();
    let failed = store.create_job(failed).await.unwrap();

    sqlx::query(
        "UPDATE runtime_metadata SET value = $1, updated_at_ms = $1 WHERE key = 'operational_job_outcomes_coverage_start_ms'",
    )
    .bind((now - Duration::hours(2)).timestamp_millis())
    .execute(&store.pool)
    .await
    .unwrap();

    let gc = store.garbage_collect_ephemeral_jobs(100).await.unwrap();
    assert_eq!(gc["deleted"], json!(2));
    let remaining =
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM jobs WHERE job_id = ANY($1)")
            .bind(vec![completed.id.clone(), failed.id.clone()])
            .fetch_one(&store.pool)
            .await
            .unwrap();
    assert_eq!(remaining, 0);

    let runtime = Ctx::new(store.clone());
    let overview =
        clankcord::views::health::dashboard_health_payload(&runtime, json!({}), json!({}))
            .await
            .unwrap();
    let windows = overview["operations"]["windows"].as_array().unwrap();
    let five_minutes = windows.iter().find(|row| row["label"] == "5m").unwrap();
    let fifteen_minutes = windows.iter().find(|row| row["label"] == "15m").unwrap();
    let one_hour = windows.iter().find(|row| row["label"] == "1h").unwrap();
    assert_eq!(five_minutes["allJobs"]["total"], json!(0));
    assert_eq!(fifteen_minutes["allJobs"]["total"], json!(2));
    assert_eq!(fifteen_minutes["allJobs"]["completed"], json!(1));
    assert_eq!(fifteen_minutes["allJobs"]["failed"], json!(1));
    assert_eq!(one_hour["allJobs"]["total"], json!(2));
    assert_eq!(one_hour["coverage"]["complete"], json!(true));
    assert_eq!(overview["operations"]["failures"]["count"], json!(1));
    assert_eq!(overview["operations"]["failures"]["complete"], json!(true));
    let recent_failure = &overview["operations"]["failures"]["recent"][0];
    assert_eq!(recent_failure["jobId"], json!(failed.id));
    assert_eq!(recent_failure["category"], json!("background"));
    assert_eq!(recent_failure["scopeLabel"], json!("Ctx"));
    assert_eq!(
        recent_failure["reason"],
        json!("maintenance snapshot provider timed out")
    );

    let overview_page = clankcord::views::dashboard::dashboard_overview(
        &runtime,
        DashboardOverviewRequest::default(),
    )
    .await
    .unwrap();
    assert_eq!(overview_page["operations"]["failures"]["count"], json!(1));
    assert_eq!(
        overview_page["operations"]["failures"]["recent"][0]["jobId"],
        json!(failed.id)
    );
    assert_eq!(
        overview_page["operations"]["failures"]["recent"][0]["scopeLabel"],
        json!("Ctx")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn one_hour_failure_summary_excludes_and_clears_expired_outcomes() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(raw.path()).await;
    let now = Utc::now();

    let mut expired = Job::runtime_maintenance(15_000);
    expired.set_state(JobState::Failed);
    expired.updated_at = isoformat_z(Some(now - Duration::minutes(61)));
    expired.metadata.error = "expired maintenance failure".to_string();
    let expired = store.create_job(expired).await.unwrap();

    let mut current = Job::runtime_maintenance(15_000);
    current.set_state(JobState::Failed);
    current.updated_at = isoformat_z(Some(now - Duration::minutes(10)));
    current.metadata.error = "current maintenance failure".to_string();
    let current = store.create_job(current).await.unwrap();

    sqlx::query(
        "UPDATE runtime_metadata SET value = $1, updated_at_ms = $1 WHERE key = 'operational_job_outcomes_coverage_start_ms'",
    )
    .bind((now - Duration::hours(2)).timestamp_millis())
    .execute(&store.pool)
    .await
    .unwrap();
    let runtime = Ctx::new(store.clone());

    let overview =
        clankcord::views::health::dashboard_health_payload(&runtime, json!({}), json!({}))
            .await
            .unwrap();
    assert_eq!(overview["health"]["failures"]["count"], json!(1));
    let recent = overview["health"]["failures"]["recent"].as_array().unwrap();
    assert_eq!(recent.len(), 1);
    assert_eq!(recent[0]["jobId"], json!(current.id));
    assert_eq!(recent[0]["reason"], json!("current maintenance failure"));
    assert!(recent.iter().all(|row| row["jobId"] != expired.id));

    sqlx::query("UPDATE operational_job_outcomes SET observed_at_ms = $1 WHERE job_id = $2")
        .bind((now - Duration::minutes(61)).timestamp_millis())
        .bind(&current.id)
        .execute(&store.pool)
        .await
        .unwrap();
    let cleared =
        clankcord::views::health::dashboard_health_payload(&runtime, json!({}), json!({}))
            .await
            .unwrap();
    assert_eq!(cleared["health"]["failures"]["count"], json!(0));
    assert!(
        cleared["health"]["failures"]["recent"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn durable_job_retry_records_each_terminal_outcome() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(raw.path()).await;
    let mut job = Job::new(
        RuntimeScope::dm("user-a"),
        "user-a",
        JobState::Failed,
        clankcord::model::job::JobPayload::Command(clankcord::model::job::CommandPayload {
            command: CommandRequest::agent_task("", "user-a", "user-a", "retry outcome"),
        }),
    );
    job.metadata.error = "first attempt failed".to_string();
    let mut job = store.create_job(job).await.unwrap();
    job.set_state(JobState::Queued);
    job.metadata.error.clear();
    store.update_job(&job).await.unwrap();
    job.mark_complete();
    store.update_job(&job).await.unwrap();

    let rows = sqlx::query(
        "SELECT state, failed, error_text FROM operational_job_outcomes WHERE job_id = $1 ORDER BY observation_id",
    )
    .bind(&job.id)
    .fetch_all(&store.pool)
    .await
    .unwrap();

    assert_eq!(rows.len(), 2);
    assert_eq!(
        sqlx::Row::try_get::<String, _>(&rows[0], "state").unwrap(),
        "failed"
    );
    assert!(sqlx::Row::try_get::<bool, _>(&rows[0], "failed").unwrap());
    assert_eq!(
        sqlx::Row::try_get::<String, _>(&rows[0], "error_text").unwrap(),
        "first attempt failed"
    );
    assert_eq!(
        sqlx::Row::try_get::<String, _>(&rows[1], "state").unwrap(),
        "complete"
    );
    assert!(!sqlx::Row::try_get::<bool, _>(&rows[1], "failed").unwrap());
}

#[tokio::test(flavor = "current_thread")]
async fn concurrent_terminal_upserts_record_one_transition_outcome() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(raw.path()).await;
    let mut left_job = Job::runtime_maintenance(15_000);
    let mut right_job = left_job.clone();
    left_job.set_state(JobState::Failed);
    right_job.set_state(JobState::Failed);
    left_job.updated_at = isoformat_z(Some(Utc::now() - Duration::seconds(1)));
    right_job.updated_at = isoformat_z(Some(Utc::now()));
    left_job.metadata.error = "same terminal transition from left writer".to_string();
    right_job.metadata.error = "same terminal transition from right writer".to_string();

    let (left, right) = tokio::join!(store.update_job(&left_job), store.update_job(&right_job));
    left.unwrap();
    right.unwrap();

    let count = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM operational_job_outcomes WHERE job_id = $1",
    )
    .bind(&left_job.id)
    .fetch_one(&store.pool)
    .await
    .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test(flavor = "current_thread")]
async fn operational_outcome_retention_is_pruned_without_gc_candidates() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(raw.path()).await;
    let mut completed = Job::new(
        RuntimeScope::runtime(),
        "system",
        JobState::Complete,
        clankcord::model::job::JobPayload::Command(clankcord::model::job::CommandPayload {
            command: CommandRequest::agent_task("", "runtime", "system", "retention test"),
        }),
    );
    completed.mark_complete();
    let completed = store.create_job(completed).await.unwrap();
    sqlx::query("UPDATE operational_job_outcomes SET observed_at_ms = $1 WHERE job_id = $2")
        .bind((Utc::now() - Duration::hours(7)).timestamp_millis())
        .bind(&completed.id)
        .execute(&store.pool)
        .await
        .unwrap();

    let gc = store.garbage_collect_ephemeral_jobs(100).await.unwrap();

    assert_eq!(gc["deleted"], json!(0));
    assert_eq!(gc["prunedOperationalOutcomes"], json!(1));
    let outcomes = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM operational_job_outcomes WHERE job_id = $1",
    )
    .bind(&completed.id)
    .fetch_one(&store.pool)
    .await
    .unwrap();
    assert_eq!(outcomes, 0);
}
