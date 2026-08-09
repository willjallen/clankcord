use chrono::{Duration, Utc};
use serde_json::json;

mod common;

use clankcord::runtime::timeline::{SpeechEventInput, isoformat_z};
use clankcord::runtime::{
    CommandRequest, Ctx, DashboardFilter, DashboardOverviewRequest, DashboardTimelineRequest,
    DashboardTranscriptRequest, Job, JobState, RuntimeScope, VoiceBotStatus,
    VoiceCaptureSessionStatus,
};

use common::{initialize_test_config, test_store};

#[tokio::test(flavor = "current_thread")]
async fn dashboard_health_reports_postgres_diagnostics() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(raw.path()).await;
    let runtime = Ctx::new(store);

    let overview = clankcord::views::operations::dashboard_health_payload(
        &runtime,
        json!({}),
    )
    .await
    .unwrap();
    let database = &overview["database"];

    assert_eq!(database["ok"], json!(true));
    let components = overview["health"]["components"].as_array().unwrap();
    let postgres = components
        .iter()
        .find(|component| component["component"] == "postgres")
        .unwrap();
    assert_eq!(postgres["status"], json!("ok"));
    assert!(
        postgres["reason"]
            .as_str()
            .unwrap()
            .ends_with(" connections")
    );
    assert!(!postgres["reason"].as_str().unwrap().contains("Reachable"));
    assert_eq!(postgres["details"]["queryErrorCount"], json!(0));
    assert!(postgres["details"].get("diagnosticErrorCount").is_none());
    let wake_provider = components
        .iter()
        .find(|component| component["component"] == "wake_provider")
        .unwrap();
    assert_eq!(wake_provider["status"], json!("ok"));
    assert_eq!(wake_provider["details"]["status"], json!("closed"));
    assert_eq!(wake_provider["details"]["available"], json!(true));
    assert!(
        database["statistics"]["databaseSizeBytes"]
            .as_i64()
            .is_some_and(|bytes| bytes > 0),
        "database diagnostics payload: {database}"
    );
    assert!(
        database["pool"]["configuredMaxConnections"]
            .as_u64()
            .is_some_and(|connections| connections > 0)
    );
    assert!(
        database["activity"]
            .as_array()
            .is_some_and(|rows| !rows.is_empty())
    );
    assert!(
        database["tableActivity"]
            .as_array()
            .is_some_and(|rows| !rows.is_empty())
    );
    assert!(
        database["tables"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["table"] == "jobs" && row["totalBytes"].as_i64().unwrap() > 0)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dashboard_health_reasons_are_terse_measured_operator_state() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(raw.path()).await;
    let mut heartbeat = Job::runtime_maintenance(15_000);
    heartbeat.mark_complete();
    store.create_job(heartbeat).await.unwrap();
    store
        .record_voice_adapter_snapshot(0, 0, 0, 0)
        .await
        .unwrap();
    let runtime = Ctx::new(store);

    let payload =
        clankcord::views::operations::dashboard_summary_payload(&runtime)
            .await
            .unwrap();
    let components = payload["health"]["components"].as_array().unwrap();
    let scheduler = components
        .iter()
        .find(|component| component["component"] == "scheduler")
        .unwrap();
    assert!(
        scheduler["reason"]
            .as_str()
            .unwrap()
            .starts_with("Heartbeat ")
    );
    assert!(
        scheduler["reason"]
            .as_str()
            .unwrap()
            .ends_with(" · no due backlog")
    );
    let voice = components
        .iter()
        .find(|component| component["component"] == "voice_gateway")
        .unwrap();
    assert!(
        voice["reason"]
            .as_str()
            .unwrap()
            .starts_with("0/0 ready · snapshot ")
    );

    for component in components {
        let reason = component["reason"].as_str().unwrap();
        assert!(
            reason.chars().count() <= 100,
            "verbose health reason: {reason}"
        );
        let normalized = reason.to_lowercase();
        for rejected in [
            "dashboard",
            "diagnostic",
            "reachable",
            "unreachable",
            "available",
            "unavailable",
            "one or more",
            "has been",
            "is current",
            "are current",
            "progressing",
            "freshness threshold",
        ] {
            assert!(
                !normalized.contains(rejected),
                "health reason contains `{rejected}`: {reason}"
            );
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn dashboard_health_includes_http_request_snapshot() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(raw.path()).await;
    let runtime = Ctx::new(store);
    let requests = json!({
        "totalStarted": 9,
        "completed": 8,
        "inFlight": 1,
        "routes": [{"route": "GET /dashboard", "totalStarted": 3}]
    });

    let overview = clankcord::views::operations::dashboard_health_payload(
        &runtime,
        requests.clone(),
    )
    .await
    .unwrap();

    assert_eq!(overview["requests"], requests);
}

#[tokio::test(flavor = "current_thread")]
async fn dashboard_summary_uses_active_projection_aggregates_and_bounded_failures() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(raw.path()).await;
    let queued = Job::new(
        RuntimeScope::dm("user-a"),
        "user-a",
        JobState::Queued,
        clankcord::runtime::JobPayload::Command(clankcord::runtime::CommandPayload {
            command: CommandRequest::agent_task("", "user-a", "user-a", "summary projection"),
        }),
    );
    store.create_job(queued).await.unwrap();
    let mut failed = Job::runtime_maintenance(15_000);
    failed.set_state(JobState::Failed);
    failed.metadata.error = "summary maintenance failure".to_string();
    store.create_job(failed).await.unwrap();
    let coverage_start = (Utc::now() - Duration::hours(2)).timestamp_millis();
    sqlx::query(
        "UPDATE runtime_metadata SET value = $1, updated_at_ms = $1 WHERE key = 'operational_job_outcomes_coverage_start_ms'",
    )
    .bind(coverage_start)
    .execute(&store.pool)
    .await
    .unwrap();
    let runtime = Ctx::new(store);

    let summary =
        clankcord::views::operations::dashboard_summary_payload(&runtime)
            .await
            .unwrap();

    let keys = summary
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        keys,
        std::collections::BTreeSet::from(["generatedAt", "health", "jobs", "operations"])
    );
    assert_eq!(summary["jobs"]["summary"]["total"], json!(1));
    assert_eq!(summary["jobs"]["summary"]["active"], json!(1));
    assert_eq!(summary["jobs"]["summary"]["terminal"], json!(0));
    assert_eq!(summary["jobs"]["summary"]["failed"], json!(0));
    assert_eq!(summary["operations"]["backlog"]["total"], json!(1));
    assert_eq!(summary["health"]["failures"]["count"], json!(1));
    assert_eq!(summary["health"]["failures"]["complete"], json!(true));
    assert!(
        summary["health"]["failures"]["recent"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dashboard_transcript_channel_filter_applies_before_limit() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(raw.path()).await;
    let base = Utc::now() - Duration::minutes(30);
    append_dashboard_speech(
        &store,
        raw.path(),
        "code",
        "Code Lounge",
        "code-lounge",
        base,
        "needle code transcript",
        1,
    )
    .await;
    for index in 0..15 {
        append_dashboard_speech(
            &store,
            raw.path(),
            "art",
            "Art Lounge",
            "art-lounge",
            base + Duration::minutes(index + 1),
            "newer art transcript",
            index + 2,
        )
        .await;
    }
    let runtime = Ctx::new(store);

    let overview = clankcord::views::dashboard::dashboard_transcript(
        &runtime,
        DashboardTranscriptRequest {
            limit: 10,
            channel: "code".to_string(),
            search: "needle".to_string(),
            ..DashboardTranscriptRequest::default()
        },
    )
    .await
    .unwrap();
    let events = overview["transcript"]["events"].as_array().unwrap();

    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["voice_channel_id"], json!("code"));
    assert_eq!(events[0]["text"], json!("needle code transcript"));
}

#[tokio::test(flavor = "current_thread")]
async fn dashboard_timeline_limit_returns_newest_events_across_scopes() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(raw.path()).await;
    let base = Utc::now() - Duration::minutes(30);
    for index in 0..20 {
        let (channel_id, channel_name, channel_slug) = if index % 2 == 0 {
            ("code", "Code Lounge", "code-lounge")
        } else {
            ("art", "Art Lounge", "art-lounge")
        };
        append_dashboard_speech(
            &store,
            raw.path(),
            channel_id,
            channel_name,
            channel_slug,
            base + Duration::minutes(index),
            &format!("bounded timeline event {index}"),
            index + 1,
        )
        .await;
    }
    let runtime = Ctx::new(store);

    let overview = clankcord::views::dashboard::dashboard_timeline(
        &runtime,
        DashboardTimelineRequest {
            record_types: DashboardFilter::Values(std::collections::BTreeSet::from([
                "event".to_string()
            ])),
            categories: DashboardFilter::All,
            job_kinds: DashboardFilter::None,
            metadata: "none".to_string(),
            limit: 10,
            ..DashboardTimelineRequest::default()
        },
    )
    .await
    .unwrap();
    let events = overview["records"]
        .as_array()
        .unwrap()
        .iter()
        .map(|record| &record["event"])
        .collect::<Vec<_>>();

    assert_eq!(events.len(), 10);
    assert_eq!(events[0]["text"], json!("bounded timeline event 19"));
    assert_eq!(events[9]["text"], json!("bounded timeline event 10"));
    assert!(
        events
            .iter()
            .all(|event| event["text"] != json!("bounded timeline event 9"))
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dashboard_timeline_search_applies_before_limit() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(raw.path()).await;
    let base = Utc::now() - Duration::minutes(30);
    append_dashboard_speech(
        &store,
        raw.path(),
        "code",
        "Code Lounge",
        "code-lounge",
        base,
        "needle timeline event",
        1,
    )
    .await;
    for index in 0..15 {
        append_dashboard_speech(
            &store,
            raw.path(),
            "art",
            "Art Lounge",
            "art-lounge",
            base + Duration::minutes(index + 1),
            "newer unrelated timeline event",
            index + 2,
        )
        .await;
    }
    let runtime = Ctx::new(store);

    let overview = clankcord::views::dashboard::dashboard_timeline(
        &runtime,
        DashboardTimelineRequest {
            record_types: DashboardFilter::Values(std::collections::BTreeSet::from([
                "event".to_string()
            ])),
            categories: DashboardFilter::All,
            job_kinds: DashboardFilter::None,
            search: "needle".to_string(),
            search_field: "detail".to_string(),
            metadata: "none".to_string(),
            limit: 10,
            ..DashboardTimelineRequest::default()
        },
    )
    .await
    .unwrap();
    let events = overview["records"]
        .as_array()
        .unwrap()
        .iter()
        .map(|record| &record["event"])
        .collect::<Vec<_>>();

    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["voice_channel_id"], json!("code"));
    assert_eq!(events[0]["text"], json!("needle timeline event"));
}

#[tokio::test(flavor = "current_thread")]
async fn dashboard_job_summary_groups_by_runtime_scope() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(raw.path()).await;
    store
        .create_job(Job::new(
            RuntimeScope::voice_channel("guild", "voice"),
            "system",
            JobState::Queued,
            clankcord::runtime::JobPayload::Command(clankcord::runtime::CommandPayload {
                command: CommandRequest::agent_task("guild", "voice", "system", "voice"),
            }),
        ))
        .await
        .unwrap();
    store
        .create_job(Job::new(
            RuntimeScope::dm("user"),
            "system",
            JobState::Failed,
            clankcord::runtime::JobPayload::Command(clankcord::runtime::CommandPayload {
                command: CommandRequest::agent_task("", "user", "system", "dm"),
            }),
        ))
        .await
        .unwrap();
    let runtime = Ctx::new(store);

    let overview = clankcord::views::dashboard::dashboard_overview(
        &runtime,
        DashboardOverviewRequest::default(),
    )
    .await
    .unwrap();
    let summary = &overview["jobs"]["summary"];
    let scopes = summary["byScope"].as_array().unwrap();
    let timeline = clankcord::views::dashboard::dashboard_timeline(
        &runtime,
        DashboardTimelineRequest {
            record_types: DashboardFilter::Values(std::collections::BTreeSet::from([
                "event".to_string()
            ])),
            categories: DashboardFilter::All,
            job_kinds: DashboardFilter::None,
            metadata: "none".to_string(),
            ..DashboardTimelineRequest::default()
        },
    )
    .await
    .unwrap();
    let events = timeline["records"]
        .as_array()
        .unwrap()
        .iter()
        .map(|record| &record["event"])
        .collect::<Vec<_>>();

    assert!(summary.get("byRoom").is_none());
    assert!(scopes.iter().any(|scope| {
        scope["scope_kind"] == "voice_channel"
            && scope["guild_id"] == "guild"
            && scope["scope_id"] == "voice"
            && scope["total"] == 1
    }));
    assert!(scopes.iter().any(|scope| {
        scope["scope_kind"] == "dm" && scope["scope_id"] == "user" && scope["failed"] == 1
    }));
    assert!(events.iter().any(|event| {
        event["kind"] == "job_created"
            && event["scope_kind"] == "dm"
            && event["scope_id"] == "user"
            && event.get("voice_channel_id").is_none()
    }));
}

async fn append_dashboard_speech(
    store: &clankcord::runtime::timeline::TimelineStore,
    raw_root: &std::path::Path,
    voice_channel_id: &str,
    voice_channel_name: &str,
    voice_channel_slug: &str,
    start: chrono::DateTime<Utc>,
    text: &str,
    segment_index: i64,
) {
    store
        .append_speech_event(SpeechEventInput {
            guild_id: "guild".to_string(),
            guild_slug: "guild".to_string(),
            voice_channel_id: voice_channel_id.to_string(),
            voice_channel_name: voice_channel_name.to_string(),
            voice_channel_slug: voice_channel_slug.to_string(),
            capture_run_id: format!("cap_{voice_channel_id}"),
            voice_bot_id: "clanky-vc1".to_string(),
            voice_bot_discord_user_id: "bot-user".to_string(),
            speaker_user_id: "user-a".to_string(),
            speaker_label: "Will".to_string(),
            speaker_username: "will".to_string(),
            segment_start_time: start,
            segment_end_time: start + Duration::seconds(1),
            text_draft: text.to_string(),
            source_audio_path: raw_root.join(format!("dashboard-{segment_index}.wav")),
            audio_checksum: "sha256:test".to_string(),
            segment_index,
            duration_ms: 1000,
            ..Default::default()
        })
        .await
        .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn dashboard_latency_stats_exclude_phase_contaminated_intervals() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(raw.path()).await;
    let base_ms = Utc::now().timestamp_millis() - 30_000;

    sqlx::query(
        r#"
        INSERT INTO operational_job_outcomes(
          job_id, scope_kind, guild_id, scope_id, kind, state, failed,
          lane, ready_at_ms, created_at_ms, observed_at_ms, started_at_ms,
          completed_at_ms, error_text
        )
        VALUES
          ('job_latency_clean', 'voice_channel', 'guild', 'code', 'wake_activation', 'complete', FALSE,
           'voice_control', $1, $2, $5, $3, $4, ''),
          ('job_latency_phase', 'voice_channel', 'guild', 'code', 'wake_activation', 'complete', FALSE,
           'voice_control', $8, $6, $10, $7, $9, '')
        "#,
    )
    .bind(base_ms + 250)
    .bind(base_ms)
    .bind(base_ms + 500)
    .bind(base_ms + 1000)
    .bind(base_ms + 1000)
    .bind(base_ms + 2000)
    .bind(base_ms + 3000)
    .bind(base_ms + 5000)
    .bind(base_ms + 6000)
    .bind(base_ms + 6000)
    .execute(&store.pool)
    .await
    .unwrap();

    let runtime = Ctx::new(store);
    let overview = clankcord::views::operations::dashboard_health_payload(
        &runtime,
        json!({}),
    )
    .await
    .unwrap();
    let latency_rows = overview["operations"]["latencies"]["byKind"]
        .as_array()
        .unwrap();
    let wake_activation = latency_rows
        .iter()
        .find(|row| row["kind"].as_str() == Some("wake_activation"))
        .unwrap();

    assert_eq!(wake_activation["count"], json!(2));
    assert_eq!(wake_activation["totalMs"]["count"], json!(2));
    assert_eq!(wake_activation["totalMs"]["max"], json!(4000));
    assert_eq!(wake_activation["readyDelayMs"]["count"], json!(1));
    assert_eq!(wake_activation["readyDelayMs"]["p50"], json!(250));
    assert_eq!(wake_activation["queueMs"]["count"], json!(1));
    assert_eq!(wake_activation["queueMs"]["p50"], json!(250));
    assert_eq!(wake_activation["runMs"]["count"], json!(2));
    assert_eq!(wake_activation["runMs"]["max"], json!(3000));
    assert_eq!(wake_activation["excluded"]["phaseContaminated"], json!(1));
    assert_eq!(wake_activation["excluded"]["readyDelayMs"], json!(1));
    assert_eq!(wake_activation["excluded"]["queueMs"], json!(1));
}

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
    let overview = clankcord::views::operations::dashboard_health_payload(
        &runtime,
        json!({}),
    )
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

    let overview = clankcord::views::operations::dashboard_health_payload(
        &runtime,
        json!({}),
    )
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
    let cleared = clankcord::views::operations::dashboard_health_payload(
        &runtime,
        json!({}),
    )
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
        clankcord::runtime::JobPayload::Command(clankcord::runtime::CommandPayload {
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
        clankcord::runtime::JobPayload::Command(clankcord::runtime::CommandPayload {
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

#[tokio::test(flavor = "current_thread")]
async fn stale_voice_rows_are_separated_from_current_dashboard_status() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(raw.path()).await;
    store
        .upsert_voice_bot_state(&VoiceBotStatus {
            bot_id: "voice-1".to_string(),
            ready: true,
            gateway_running: true,
            ..VoiceBotStatus::default()
        })
        .await
        .unwrap();
    store
        .upsert_capture_session_status(&VoiceCaptureSessionStatus {
            session_id: "session-1".to_string(),
            guild_id: "guild".to_string(),
            voice_channel_id: "code".to_string(),
            bot_id: "voice-1".to_string(),
            active: true,
            ..VoiceCaptureSessionStatus::default()
        })
        .await
        .unwrap();
    store
        .record_voice_adapter_snapshot(1, 1, 1, 1)
        .await
        .unwrap();
    let stale_at_ms = (Utc::now() - Duration::minutes(2)).timestamp_millis();
    sqlx::query("UPDATE bot_states SET updated_at_ms = $1 WHERE bot_id = 'voice-1'")
        .bind(stale_at_ms)
        .execute(&store.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE capture_sessions SET updated_at_ms = $1 WHERE session_id = 'session-1'")
        .bind(stale_at_ms)
        .execute(&store.pool)
        .await
        .unwrap();
    let runtime = Ctx::new(store);

    let rooms = clankcord::views::operations::dashboard_rooms_payload(&runtime)
        .await
        .unwrap();
    let overview =
        clankcord::views::operations::dashboard_summary_payload(&runtime)
            .await
            .unwrap();

    assert!(rooms["status"]["bots"].as_array().unwrap().is_empty());
    assert!(rooms["status"]["sessions"].as_array().unwrap().is_empty());
    assert_eq!(
        rooms["status"]["staleObservations"]["bots"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        rooms["status"]["staleObservations"]["sessions"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(rooms["status"]["pool"]["observedBots"], json!(0));
    let components = overview["health"]["components"].as_array().unwrap();
    let voice = components
        .iter()
        .find(|component| component["component"] == "voice_gateway")
        .unwrap();
    let capture = components
        .iter()
        .find(|component| component["component"] == "capture")
        .unwrap();
    assert_eq!(voice["status"], json!("down"));
    assert_eq!(capture["status"], json!("stale"));
}
