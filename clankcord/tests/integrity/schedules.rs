//! The job_schedules clock: windowed firing, disable semantics, and the
//! ephemeral runtime-maintenance jobs it mints.

use std::collections::BTreeSet;

use chrono::{Duration, Utc};
use serde_json::json;

use clankcord::domain::Ctx;
use clankcord::model::job::{Job, JobKind, JobState};
use clankcord::store::{JobVisibility, isoformat_z};

use crate::support::{initialize_test_config, test_store};

#[tokio::test(flavor = "current_thread")]
async fn runtime_maintenance_job_is_ephemeral_and_round_trips() {
    let job = Job::runtime_maintenance(500);
    let decoded = Job::decode(&job.encode().unwrap()).unwrap();

    assert_eq!(decoded.kind, JobKind::RuntimeMaintenance);
    assert!(decoded.kind.is_ephemeral());
    assert_eq!(
        decoded.runtime_maintenance_payload().unwrap().interval_ms,
        500
    );
    assert_eq!(decoded.payload_value()["interval_ms"], json!(500));
}
#[tokio::test(flavor = "current_thread")]
async fn job_schedules_fire_once_per_window_and_mint_typed_jobs() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(&raw.path().join("voice")).await;
    clankcord::engine::schedules::ensure_default_schedules(&store)
        .await
        .unwrap();
    let declared = store.list_job_schedules().await.unwrap();
    assert_eq!(declared.len(), 6);
    assert!(declared.iter().all(|schedule| schedule.enabled));

    let bus = clankcord::engine::JobBus::new(store.clone());
    let submitted = clankcord::engine::schedules::run_due_schedules(&store, &bus)
        .await
        .unwrap();
    assert_eq!(submitted.len(), 6, "all default schedules fire when due");

    // The same window does not fire twice: every row advanced by interval.
    let repeat = clankcord::engine::schedules::run_due_schedules(&store, &bus)
        .await
        .unwrap();
    assert!(
        repeat.is_empty(),
        "claimed schedules advance their due time"
    );

    let jobs = store
        .list_jobs_with_visibility(None, None, JobVisibility::IncludeEphemeral)
        .await
        .unwrap();
    let kinds = jobs.iter().map(|job| job.kind).collect::<BTreeSet<_>>();
    for kind in [
        JobKind::RuntimeMaintenance,
        JobKind::VoiceStatusSync,
        JobKind::AutomationEvaluation,
        JobKind::AgentSessionRetirement,
        JobKind::StaleWakeProbeSweep,
        JobKind::EphemeralJobGc,
    ] {
        assert!(kinds.contains(&kind), "schedule minted {kind}");
    }
}
#[tokio::test(flavor = "current_thread")]
async fn disabled_job_schedules_do_not_fire() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(&raw.path().join("voice")).await;
    store
        .upsert_job_schedule(
            "member_refresh",
            "member_sync",
            &json!({"guild_id": "guild"}),
            60_000,
            false,
        )
        .await
        .unwrap();
    let bus = clankcord::engine::JobBus::new(store.clone());
    let submitted = clankcord::engine::schedules::run_due_schedules(&store, &bus)
        .await
        .unwrap();
    assert!(submitted.is_empty());

    assert!(
        store
            .set_job_schedule_enabled("member_refresh", true)
            .await
            .unwrap()
    );
    let submitted = clankcord::engine::schedules::run_due_schedules(&store, &bus)
        .await
        .unwrap();
    assert_eq!(submitted.len(), 1);
    assert_eq!(submitted[0]["kind"], json!("member_sync"));
}
#[tokio::test(flavor = "current_thread")]
async fn runtime_maintenance_submits_background_work_jobs() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(&raw.path().join("voice")).await;
    let runtime = Ctx::new(store.clone());
    let created = store
        .create_job(Job::runtime_maintenance(500))
        .await
        .unwrap();
    let mut running = created.clone();
    running.mark_running();
    store.update_job(&running).await.unwrap();

    clankcord::engine::dispatcher::dispatch_claimed_runtime_job(
        &runtime,
        &clankcord::ports::discord::DiscordApiUnavailable,
        running,
    )
    .await
    .unwrap();

    let completed = store.get_job(&created.id).await.unwrap();
    assert_eq!(completed.state, JobState::Complete);
    let output = completed.metadata.output.unwrap().to_json();
    assert_eq!(output["kind"], json!("runtime_maintenance"));
    // Recurring fan-out now belongs to job_schedules; a maintenance pass
    // submits only evaluated work (no sessions here, so none).
    assert_eq!(
        output["submitted_jobs"]
            .as_array()
            .map(|values| values.len())
            .unwrap(),
        0
    );
}
#[tokio::test(flavor = "current_thread")]
async fn runtime_maintenance_times_out_stale_running_jobs() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(&raw.path().join("voice")).await;
    let old_timestamp = isoformat_z(Some(Utc::now() - Duration::minutes(31)));
    let mut running_stale = Job::discord_voice_status_snapshot("job_source");
    running_stale.mark_running();
    running_stale.created_at = old_timestamp.clone();
    running_stale.updated_at = old_timestamp;
    let stale = store.create_job(running_stale).await.unwrap();
    let mut running_maintenance = Job::runtime_maintenance(500);
    running_maintenance.mark_running();
    store.create_job(running_maintenance.clone()).await.unwrap();
    let runtime = Ctx::new(store.clone());

    let timed_out_jobs =
        clankcord::domain::maintenance::execution::recover_stale_running_jobs_for_maintenance_pass(
            &runtime,
        )
        .await
        .unwrap();
    assert_eq!(timed_out_jobs.len(), 1);
    let timed_out = store.get_job(&stale.id).await.unwrap();
    assert_eq!(timed_out.state, JobState::FailedTimeout);
    assert_eq!(
        timed_out.metadata.error,
        "job exceeded stale running-job timeout"
    );
}
#[tokio::test(flavor = "current_thread")]
async fn maintenance_work_jobs_are_typed_ephemeral_jobs() {
    let jobs = [
        Job::voice_status_sync("job_source"),
        Job::discord_voice_status_snapshot("job_source"),
        Job::automation_evaluation("job_source"),
        Job::agent_session_retirement("job_source"),
        Job::agent_thread_title_refresh(
            "job_source",
            "ags_1",
            "guild",
            "code",
            "thread-1",
            "agent code ags_1",
            2,
        ),
        Job::stale_wake_probe_sweep("job_source", 15),
        Job::ephemeral_job_gc("job_source", 500),
    ];

    for job in jobs {
        let decoded = Job::decode(&job.encode().unwrap()).unwrap();
        assert_eq!(decoded.kind, job.kind);
        assert!(decoded.kind.is_ephemeral());
        assert_eq!(
            decoded.payload_value()["source_job_id"],
            json!("job_source")
        );
    }
}
