use std::collections::BTreeSet;

use chrono::{Duration, Utc};
use serde_json::json;


use clankcord::domain::Ctx;
use clankcord::domain::automations::AutomationSpec;
use clankcord::model::job::{CommandRequest, Job};
use clankcord::model::scope::RuntimeScope;
use clankcord::store::{instant_ms_dt, isoformat_z};
use clankcord::views::{
    DashboardAgentsRequest, DashboardFilter, DashboardJobsRequest, DashboardOverviewRequest,
    DashboardTimelineRequest, DashboardTranscriptRequest, default_dashboard_categories,
    parse_dashboard_filter,
};

use crate::support::{initialize_test_config, test_store};

#[test]
fn dashboard_filter_parser_preserves_all_none_and_subset() {
    assert_eq!(
        parse_dashboard_filter(None, "kinds").unwrap(),
        DashboardFilter::All
    );
    assert_eq!(
        parse_dashboard_filter(Some(""), "kinds").unwrap(),
        DashboardFilter::None
    );
    assert_eq!(
        parse_dashboard_filter(Some("none"), "kinds").unwrap(),
        DashboardFilter::None
    );
    assert_eq!(
        parse_dashboard_filter(Some("feedback, speech_segment,feedback"), "kinds").unwrap(),
        DashboardFilter::Values(BTreeSet::from([
            "feedback".to_string(),
            "speech_segment".to_string(),
        ]))
    );
    assert!(parse_dashboard_filter(Some("all,feedback"), "kinds").is_err());
    assert_eq!(
        default_dashboard_categories(),
        values([
            "conversation",
            "agent",
            "messaging_control",
            "automation",
            "operations",
            "other",
        ])
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dashboard_timeline_defaults_exclude_background_jobs_before_limit() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(raw.path()).await;
    let now = Utc::now() - Duration::minutes(2);
    let jobs = [
        Job::runtime_maintenance(500),
        Job::stale_wake_probe_sweep("maintenance", 120),
        Job::automation_evaluation("maintenance"),
        Job::ephemeral_job_gc("maintenance", 100),
        Job::discord_voice_status_snapshot("maintenance"),
        Job::voice_status_sync("maintenance"),
        Job::agent_session_retirement("maintenance"),
    ];
    let expected_kinds = jobs
        .iter()
        .map(|job| job.kind.as_str().to_string())
        .collect::<BTreeSet<_>>();
    for (index, mut job) in jobs.into_iter().enumerate() {
        job.created_at = isoformat_z(Some(now + Duration::seconds(index as i64)));
        job.updated_at = job.created_at.clone();
        store.create_job(job).await.unwrap();
    }
    let runtime = Ctx::new(store);

    let default_page = clankcord::views::dashboard::dashboard_jobs(
        &runtime,
        DashboardJobsRequest {
            limit: 3,
            ..DashboardJobsRequest::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(default_page["matched"], json!(0));
    assert_eq!(default_page["returned"], json!(0));
    assert_eq!(
        default_page["facets"]["defaultCategories"],
        json!([
            "conversation",
            "agent",
            "messaging_control",
            "automation",
            "operations",
            "other"
        ])
    );
    let background_facet = default_page["facets"]["categories"]
        .as_array()
        .unwrap()
        .iter()
        .find(|facet| facet["id"] == "background")
        .unwrap();
    assert_eq!(background_facet["label"], json!("Background"));
    assert_eq!(background_facet["count"], json!(7));
    assert_eq!(
        background_facet["kinds"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(serde_json::Value::as_str)
            .map(ToString::to_string)
            .collect::<BTreeSet<_>>(),
        expected_kinds
    );

    let background_page = clankcord::views::dashboard::dashboard_jobs(
        &runtime,
        DashboardJobsRequest {
            categories: values(["background"]),
            limit: 3,
            ..DashboardJobsRequest::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(background_page["matched"], json!(7));
    assert_eq!(background_page["returned"], json!(3));
    assert_eq!(background_page["hasMore"], json!(true));
    assert!(
        background_page["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .all(|job| job["category"] == "background")
    );

    let all_page = clankcord::views::dashboard::dashboard_jobs(
        &runtime,
        DashboardJobsRequest {
            categories: DashboardFilter::All,
            limit: 10,
            ..DashboardJobsRequest::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(all_page["matched"], json!(7));

    let none_page = clankcord::views::dashboard::dashboard_jobs(
        &runtime,
        DashboardJobsRequest {
            categories: DashboardFilter::None,
            limit: 10,
            ..DashboardJobsRequest::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(none_page["matched"], json!(0));
    assert_eq!(none_page["returned"], json!(0));
}

#[tokio::test(flavor = "current_thread")]
async fn dashboard_timeline_taxonomy_covers_known_event_kinds_and_keeps_other_visible() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(raw.path()).await;
    let known_kinds = [
        "agent_session_created",
        "agent_session_resumed",
        "agent_session_retired",
        "agent_session_thread_created",
        "agent_session_thread_unavailable",
        "agent_task_result_suppressed",
        "agent_thread_title_refresh_attempted",
        "agent_thread_title_skipped",
        "agent_thread_titled",
        "automation_action_failed",
        "automation_cancelled",
        "automation_created",
        "automation_evaluation",
        "automation_fired",
        "conversation_started",
        "discord_slash_command",
        "discord_text_message",
        "discord_typing_indicator",
        "discord_voice_status_snapshot",
        "ephemeral_job_gc",
        "feedback",
        "forget_applied",
        "job_created",
        "listening_paused",
        "listening_resumed",
        "participant_deafen_changed",
        "participant_joined",
        "participant_left",
        "participant_moved",
        "participant_mute_changed",
        "participant_stream_changed",
        "participant_video_changed",
        "publication_created",
        "retention_retired",
        "room_auto_join_suppressed",
        "room_manual_hold_set",
        "runtime_maintenance",
        "speech_segment",
        "stale_running_job_sweep",
        "stale_wake_probe_sweep",
        "text_delivered",
        "transcript",
        "transcription_mux",
        "transcription_mux_plan",
        "voice_adapter_snapshot",
        "voice_bot_assigned",
        "voice_bot_released",
        "voice_status",
        "voice_status_sync",
        "wake_activation_amended",
        "wake_activation_dispatched",
        "wake_activation_no_request",
        "wake_activation_replaced",
        "wake_activation_transcription_failed",
        "wake_activation_window_closed",
        "wake_detected",
        "wake_probe",
    ];
    let now = Utc::now() - Duration::minutes(3);
    for (index, event_kind) in known_kinds.iter().enumerate() {
        insert_taxonomy_event(
            &store,
            &format!("evt_taxonomy_{index:02}"),
            event_kind,
            now + Duration::seconds(index as i64),
        )
        .await;
    }
    insert_taxonomy_event(
        &store,
        "evt_historic_other",
        "historic_operator_signal",
        now + Duration::seconds(known_kinds.len() as i64),
    )
    .await;
    let runtime = Ctx::new(store);

    let all = clankcord::views::dashboard::dashboard_timeline(
        &runtime,
        DashboardTimelineRequest {
            record_types: values(["event"]),
            categories: DashboardFilter::All,
            limit: 100,
            ..DashboardTimelineRequest::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(all["matched"], json!(known_kinds.len() + 1));
    for record in all["records"].as_array().unwrap() {
        let kind = record["event"]["event_kind"].as_str().unwrap();
        if kind == "historic_operator_signal" {
            assert_eq!(record["category"], json!("other"));
        } else {
            assert_ne!(
                record["category"],
                json!("other"),
                "unclassified known kind: {kind}"
            );
        }
        assert_eq!(record["category"], record["event"]["category"]);
    }

    let default_page = clankcord::views::dashboard::dashboard_timeline(
        &runtime,
        DashboardTimelineRequest {
            record_types: values(["event"]),
            limit: 100,
            ..DashboardTimelineRequest::default()
        },
    )
    .await
    .unwrap();
    assert!(default_page["records"].as_array().unwrap().iter().any(
        |record| record["event"]["event_kind"] == "historic_operator_signal"
            && record["category"] == "other"
    ));
    assert!(
        default_page["records"]
            .as_array()
            .unwrap()
            .iter()
            .all(
                |record| record["category"] != "background" && record["category"] != "voice_detail"
            )
    );

    let voice_detail = clankcord::views::dashboard::dashboard_timeline(
        &runtime,
        DashboardTimelineRequest {
            record_types: values(["event"]),
            categories: values(["voice_detail"]),
            limit: 100,
            ..DashboardTimelineRequest::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(voice_detail["matched"], json!(8));
    assert!(
        voice_detail["records"]
            .as_array()
            .unwrap()
            .iter()
            .all(|record| record["category"] == "voice_detail")
    );

    let invalid = clankcord::views::dashboard::dashboard_timeline(
        &runtime,
        DashboardTimelineRequest {
            categories: values(["made_up_category"]),
            ..DashboardTimelineRequest::default()
        },
    )
    .await;
    assert!(invalid.is_err());
}

#[tokio::test(flavor = "current_thread")]
async fn dashboard_timeline_applies_search_scope_state_and_kind_before_limit() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(raw.path()).await;
    let base = Utc::now() - Duration::minutes(30);
    insert_voice_room(&store, "guild", "code", "Code Lounge").await;
    insert_voice_room(&store, "guild", "art", "Art Studio").await;
    insert_event(
        &store,
        "evt_canonical",
        "voice_channel",
        "guild",
        "code",
        "feedback",
        base,
        "needle operator feedback",
        "failed",
        "agent_task",
        Some("Code Lounge"),
    )
    .await;
    for index in 0..15 {
        insert_event(
            &store,
            &format!("evt_newer_{index:02}"),
            "voice_channel",
            "guild",
            "art",
            "speech_segment",
            base + Duration::minutes(index + 1),
            "newer unrelated speech",
            "complete",
            "audio_segment",
            Some("Art Studio"),
        )
        .await;
    }
    let runtime = Ctx::new(store);

    let response = clankcord::views::dashboard::dashboard_timeline(
        &runtime,
        DashboardTimelineRequest {
            record_types: values(["event"]),
            kinds: values(["agent_task"]),
            event_kinds: values(["feedback"]),
            states: values(["failed"]),
            scope_kinds: values(["voice_channel"]),
            scope_ids: values(["code"]),
            guild_ids: values(["guild"]),
            search: "needle feedback".to_string(),
            search_field: "all".to_string(),
            limit: 1,
            ..DashboardTimelineRequest::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(response["matched"], json!(1));
    assert_eq!(response["returned"], json!(1));
    assert_eq!(response["hasMore"], json!(false));
    let record = &response["records"][0];
    assert_eq!(record["recordType"], json!("event"));
    assert_eq!(record["id"], json!("evt_canonical"));
    assert_eq!(record["event"]["event_id"], json!("evt_canonical"));
    assert_eq!(record["event"]["scopeLabel"], json!("Code Lounge"));
    assert!(record["event"].get("stt").is_none());
    assert!(record["event"].get("audio_bytes").is_none());
    assert!(serde_json::to_vec(record).unwrap().len() < 20_000);

    assert_eq!(response["facets"]["recordTypes"], json!(["event", "job"]));
    assert!(
        response["facets"]["eventKinds"]
            .as_array()
            .unwrap()
            .contains(&json!("speech_segment")),
        "facets must describe the bounded snapshot instead of the returned page: {response}"
    );
    assert!(
        response["facets"]["kinds"]
            .as_array()
            .unwrap()
            .contains(&json!("audio_segment"))
    );

    let none = clankcord::views::dashboard::dashboard_timeline(
        &runtime,
        DashboardTimelineRequest {
            record_types: values(["event"]),
            event_kinds: DashboardFilter::None,
            limit: 1,
            ..DashboardTimelineRequest::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(none["matched"], json!(0));
    assert_eq!(none["returned"], json!(0));
    assert!(
        none["facets"]["eventKinds"]
            .as_array()
            .unwrap()
            .contains(&json!("feedback")),
        "Select None must not erase facet options: {none}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dashboard_jobs_include_retained_ephemeral_rows_and_human_dm_labels() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(raw.path()).await;
    store
        .upsert_discord_members(
            "guild",
            &[json!({
                "user": {
                    "id": "dm-user",
                    "username": "rowan"
                }
            })],
        )
        .await
        .unwrap();
    let now = Utc::now() - Duration::minutes(5);
    let mut dm_job = Job::agent_task_for_session(
        "ags_dm",
        RuntimeScope::dm("dm-user"),
        "dm-user",
        CommandRequest::agent_task("", "dm-user", "dm-user", "inspect the dashboard"),
    );
    dm_job.created_at = isoformat_z(Some(now));
    dm_job.updated_at = dm_job.created_at.clone();
    let dm_job = store.create_job(dm_job).await.unwrap();
    let mut maintenance = Job::runtime_maintenance(500);
    maintenance.created_at = isoformat_z(Some(now + Duration::seconds(1)));
    maintenance.updated_at = maintenance.created_at.clone();
    let maintenance = store.create_job(maintenance).await.unwrap();
    insert_event(
        &store,
        "evt_dm_feedback",
        "dm",
        "",
        "dm-user",
        "feedback",
        now + Duration::seconds(2),
        "direct feedback",
        "complete",
        "",
        None,
    )
    .await;
    let runtime = Ctx::new(store);

    let response = clankcord::views::dashboard::dashboard_jobs(
        &runtime,
        DashboardJobsRequest {
            from: "-1h".to_string(),
            limit: 10,
            ..DashboardJobsRequest::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(response["matched"], json!(1));
    assert!(
        response["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .all(|job| job["job_id"] != maintenance.id)
    );
    let returned_dm = response["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|job| job["job_id"] == dm_job.id)
        .unwrap();
    assert_eq!(
        returned_dm["scopeLabel"],
        json!("Direct message with rowan")
    );
    assert_eq!(returned_dm["requestedByLabel"], json!("rowan"));
    assert!(
        response["facets"]["jobKinds"]
            .as_array()
            .unwrap()
            .contains(&json!("runtime_maintenance"))
    );
    let dm_scope = response["facets"]["scopes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|scope| scope["kind"] == "dm" && scope["id"] == "dm-user")
        .unwrap();
    assert_eq!(dm_scope["label"], json!("Direct message with rowan"));

    let human_search = clankcord::views::dashboard::dashboard_timeline(
        &runtime,
        DashboardTimelineRequest {
            record_types: values(["event", "job"]),
            search: "rowan".to_string(),
            search_field: "all".to_string(),
            limit: 10,
            ..DashboardTimelineRequest::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(human_search["matched"], json!(2));
    assert!(
        human_search["records"]
            .as_array()
            .unwrap()
            .iter()
            .any(|record| record["recordType"] == "event"
                && record["event"]["scopeLabel"] == "Direct message with rowan")
    );

    let only_maintenance = clankcord::views::dashboard::dashboard_jobs(
        &runtime,
        DashboardJobsRequest {
            categories: values(["background"]),
            kinds: values(["runtime_maintenance"]),
            limit: 10,
            ..DashboardJobsRequest::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(only_maintenance["matched"], json!(1));
    assert_eq!(only_maintenance["jobs"][0]["job_id"], json!(maintenance.id));
    assert_eq!(only_maintenance["jobs"][0]["category"], json!("background"));
}

#[tokio::test(flavor = "current_thread")]
async fn dashboard_timeline_cursor_pins_snapshot_and_walks_combined_records_once() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(raw.path()).await;
    let base = Utc::now() - Duration::minutes(10);
    insert_voice_room(&store, "guild", "code", "Code Lounge").await;
    for index in 0..3 {
        insert_event(
            &store,
            &format!("evt_page_{index}"),
            "voice_channel",
            "guild",
            "code",
            "feedback",
            base + Duration::seconds(index),
            &format!("page event {index}"),
            "complete",
            "",
            Some("Code Lounge"),
        )
        .await;
    }
    let runtime = Ctx::new(store);
    let first = clankcord::views::dashboard::dashboard_timeline(
        &runtime,
        DashboardTimelineRequest {
            record_types: values(["event"]),
            limit: 2,
            ..DashboardTimelineRequest::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(first["matched"], json!(3));
    assert_eq!(first["returned"], json!(2));
    assert_eq!(first["hasMore"], json!(true));
    let cursor = first["nextCursor"].as_str().unwrap().to_string();

    insert_event(
        &runtime.store,
        "evt_after_snapshot",
        "voice_channel",
        "guild",
        "code",
        "feedback",
        Utc::now() + Duration::seconds(1),
        "inserted after the first snapshot",
        "complete",
        "",
        Some("Code Lounge"),
    )
    .await;
    let second = clankcord::views::dashboard::dashboard_timeline(
        &runtime,
        DashboardTimelineRequest {
            record_types: values(["event"]),
            limit: 2,
            cursor,
            metadata: "none".to_string(),
            ..DashboardTimelineRequest::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(second["snapshotAt"], first["snapshotAt"]);
    assert!(second.get("matched").is_none());
    assert!(second.get("facets").is_none());
    assert_eq!(second["metadata"], json!("none"));
    assert_eq!(second["returned"], json!(1));
    assert_eq!(second["hasMore"], json!(false));
    let first_ids = first["records"]
        .as_array()
        .unwrap()
        .iter()
        .map(|record| record["id"].as_str().unwrap())
        .collect::<BTreeSet<_>>();
    let second_id = second["records"][0]["id"].as_str().unwrap();
    assert!(!first_ids.contains(second_id));
    assert_ne!(second_id, "evt_after_snapshot");
}

#[tokio::test(flavor = "current_thread")]
async fn dashboard_overview_aggregates_the_full_hour_and_excludes_stale_failures() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(raw.path()).await;
    let now = Utc::now();
    insert_voice_room(&store, "guild", "code", "Code Lounge").await;

    let mut stale = Job::runtime_maintenance(500);
    stale.set_state(clankcord::model::job::JobState::Failed);
    stale.created_at = isoformat_z(Some(now - Duration::hours(3)));
    stale.updated_at = stale.created_at.clone();
    stale.completed_at = Some(stale.created_at.clone());
    store.create_job(stale).await.unwrap();

    for index in 0..5 {
        let mut fresh = Job::runtime_maintenance(500);
        fresh.set_state(clankcord::model::job::JobState::Failed);
        fresh.created_at =
            isoformat_z(Some(now - Duration::minutes(10) + Duration::seconds(index)));
        fresh.updated_at = fresh.created_at.clone();
        fresh.completed_at = Some(fresh.created_at.clone());
        store.create_job(fresh).await.unwrap();
    }
    insert_event(
        &store,
        "evt_overview_chart",
        "voice_channel",
        "guild",
        "code",
        "feedback",
        now - Duration::minutes(2),
        "overview chart event",
        "complete",
        "",
        Some("Code Lounge"),
    )
    .await;
    let runtime = Ctx::new(store);

    let overview = clankcord::views::dashboard::dashboard_overview(
        &runtime,
        DashboardOverviewRequest { jobs_limit: 2 },
    )
    .await
    .unwrap();
    let failed = overview["jobs"]["summary"]["byState"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["state"] == "failed")
        .unwrap();
    assert_eq!(
        failed["count"],
        json!(5),
        "summary must not be a latest-N sample: {overview}"
    );
    assert_eq!(overview["jobs"]["summary"]["total"], json!(5));
    assert_eq!(overview["jobs"]["recent"].as_array().unwrap().len(), 2);
    assert_eq!(
        overview["timeline"]["recentEvents"][0]["scopeLabel"],
        json!("Code Lounge")
    );
    assert!(overview.get("database").is_none());
    assert!(overview.get("requests").is_none());
    assert!(overview.get("process").is_none());
    assert!(overview["operations"]["latencies"]["byKind"].is_array());
}

#[tokio::test(flavor = "current_thread")]
async fn dashboard_agents_are_exact_beyond_detail_limit_and_resolve_direct_labels() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(raw.path()).await;
    insert_voice_room(&store, "guild", "code", "Code Lounge").await;
    store
        .upsert_discord_members(
            "guild",
            &[
                json!({"user": {"id": "operator", "username": "rowan"}}),
                json!({"user": {"id": "dm-user", "username": "morgan"}}),
            ],
        )
        .await
        .unwrap();
    store
        .upsert_discord_members(
            "other-guild",
            &[json!({"user": {"id": "operator", "username": "wrong-guild-name"}})],
        )
        .await
        .unwrap();
    for index in 0..3 {
        let mut job = Job::agent_task_for_session(
            format!("session-{index}"),
            RuntimeScope::voice_channel("guild", "code"),
            "operator",
            CommandRequest::agent_task("guild", "code", "operator", format!("task {index}")),
        );
        let at = Utc::now() - Duration::minutes(index);
        job.created_at = isoformat_z(Some(at));
        job.updated_at = job.created_at.clone();
        store.create_job(job).await.unwrap();
    }
    let mut dm_job = Job::agent_task_for_session(
        "dm-session",
        RuntimeScope::dm("dm-user"),
        "dm-user",
        CommandRequest::agent_task("", "dm-user", "dm-user", "older DM task"),
    );
    dm_job.created_at = isoformat_z(Some(Utc::now() - Duration::minutes(30)));
    dm_job.updated_at = dm_job.created_at.clone();
    store.create_job(dm_job).await.unwrap();
    let mut stale_failed = Job::agent_task_for_session(
        "stale-session",
        RuntimeScope::voice_channel("guild", "code"),
        "operator",
        CommandRequest::agent_task("guild", "code", "operator", "old failed task"),
    );
    stale_failed.set_state(clankcord::model::job::JobState::Failed);
    stale_failed.created_at = isoformat_z(Some(Utc::now() - Duration::days(8)));
    stale_failed.updated_at = stale_failed.created_at.clone();
    stale_failed.completed_at = Some(stale_failed.created_at.clone());
    store.create_job(stale_failed).await.unwrap();

    let large_request = "r".repeat(100_000);
    let mut fresh_failed = Job::agent_task_for_session(
        "fresh-session",
        RuntimeScope::voice_channel("guild", "code"),
        "operator",
        CommandRequest::agent_task("guild", "code", "operator", &large_request),
    );
    fresh_failed.set_state(clankcord::model::job::JobState::Failed);
    fresh_failed.metadata.error = "e".repeat(100_000);
    fresh_failed.created_at = isoformat_z(Some(Utc::now() + Duration::seconds(1)));
    fresh_failed.updated_at = fresh_failed.created_at.clone();
    fresh_failed.completed_at = Some(fresh_failed.created_at.clone());
    let fresh_failed = store.create_job(fresh_failed).await.unwrap();
    let runtime = Ctx::new(store);
    let view = clankcord::views::dashboard::dashboard_agents(
        &runtime,
        DashboardAgentsRequest { limit: 1 },
    )
    .await
    .unwrap();

    assert_eq!(view["agents"]["jobs"].as_array().unwrap().len(), 1);
    assert_eq!(view["agents"]["summary"]["total"], json!(5));
    assert_eq!(view["agents"]["summary"]["active"], json!(4));
    assert_eq!(view["agents"]["summary"]["failed"], json!(1));
    assert_eq!(view["agents"]["summary"]["window"], json!("24h"));
    let sessions = view["agents"]["sessions"].as_array().unwrap();
    let voice_session = sessions
        .iter()
        .find(|session| session["scope_kind"] == "voice_channel")
        .unwrap();
    let dm_session = sessions
        .iter()
        .find(|session| session["scope_kind"] == "dm")
        .unwrap();
    assert_eq!(voice_session["invocation_count"], json!(5));
    assert_eq!(voice_session["scopeLabel"], json!("Code Lounge"));
    assert_eq!(
        dm_session["scopeLabel"],
        json!("Direct message with morgan")
    );
    assert_eq!(
        view["agents"]["jobs"][0]["job"]["scopeLabel"],
        json!("Code Lounge")
    );
    assert_eq!(
        view["agents"]["jobs"][0]["job"]["requestedByLabel"],
        json!("rowan")
    );
    let list_entry = &view["agents"]["jobs"][0];
    assert_eq!(list_entry["job"]["job_id"], json!(fresh_failed.id));
    assert_eq!(list_entry["job"]["attempts"], json!(0));
    assert!(list_entry["job"]["durationMs"].is_i64());
    assert_eq!(list_entry["job"]["request"].as_str().unwrap().len(), 1000);
    assert_eq!(list_entry["job"]["error"].as_str().unwrap().len(), 1200);
    for omitted in ["paths", "workdir", "prompt", "result", "raw"] {
        assert!(
            list_entry.get(omitted).is_none(),
            "agent list entry must not expose detail artifact {omitted}: {list_entry}"
        );
    }
    assert!(list_entry["job"].get("payload").is_none());
    assert!(list_entry["job"].get("metadata").is_none());
    assert!(
        serde_json::to_vec(&view).unwrap().len() < 100_000,
        "agent list response must remain compact"
    );
    assert!(view["agents"]["codex"].get("auth").is_none());
    let week = view["agents"]["codex"]["usage"]["windows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|window| window["label"] == "1w")
        .unwrap();
    assert_eq!(week["jobs"], json!(5));

    let detail = clankcord::views::dashboard::dashboard_agent_detail(&runtime, &fresh_failed.id)
        .await
        .unwrap();
    assert_eq!(detail["job"]["request"], json!(large_request));
    assert_eq!(detail["job"]["attempts"], json!(0));
    assert!(detail["job"]["durationMs"].is_i64());
    assert!(detail["result"].get("content").is_some());
    assert!(detail["session"].get("transcript").is_none());
    assert!(detail["session"].get("codex").is_none());
    assert!(
        detail["session"]["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .all(|job| job["durationMs"].is_i64())
    );
    for sibling in detail["session"]["jobs"].as_array().unwrap() {
        for omitted in ["prompt", "result", "raw", "codex"] {
            assert!(sibling.get(omitted).is_none());
        }
    }
    assert!(
        serde_json::to_vec(&detail).unwrap().len() < 500_000,
        "selected Agent detail may include its own artifacts but not expanded sibling artifacts"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dashboard_automations_and_transcript_resolve_labels_without_rooms_view_state() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(raw.path()).await;
    insert_voice_room(&store, "guild", "code", "Code Lounge").await;
    store
        .upsert_discord_members(
            "guild",
            &[json!({"user": {"id": "dm-user", "global_name": "Rowan"}})],
        )
        .await
        .unwrap();
    let spec = AutomationSpec::from_json(&json!({
        "schema": "clankcord.automation.v0",
        "name": "announce",
        "idempotency_key": "dashboard-labels",
        "owner": {"kind": "system"},
        "scope": {"scope_kind": "voice_channel", "guild_id": "guild", "scope_id": "code"},
        "trigger": {"kind": "event", "event_kinds": ["room.member_joined"]},
        "condition": {"kind": "true"},
        "actions": [{
            "kind": "response.send",
            "sink": {"kind": "channel", "id": "code"},
            "content": "hello"
        }]
    }))
    .unwrap();
    store.create_automation(spec).await.unwrap();
    let now = Utc::now();
    insert_event(
        &store,
        "evt_dm_older",
        "dm",
        "",
        "dm-user",
        "transcript",
        now - Duration::minutes(2),
        "older",
        "complete",
        "",
        None,
    )
    .await;
    insert_event(
        &store,
        "evt_dm_newer",
        "dm",
        "",
        "dm-user",
        "speech_segment",
        now - Duration::minutes(1),
        "newer",
        "complete",
        "",
        None,
    )
    .await;
    let runtime = Ctx::new(store);

    let automations = clankcord::views::dashboard::dashboard_automations(&runtime)
        .await
        .unwrap();
    let record = &automations["automations"]["records"][0];
    assert_eq!(record["spec"]["scope"]["scopeLabel"], json!("Code Lounge"));
    assert_eq!(
        find_scope_label(&record["spec"]["actions"]),
        Some("Code Lounge")
    );
    assert!(automations.get("publications").is_none());

    let transcript = clankcord::views::dashboard::dashboard_transcript(
        &runtime,
        DashboardTranscriptRequest::default(),
    )
    .await
    .unwrap();
    assert_eq!(
        transcript["transcript"]["events"][0]["event_id"],
        json!("evt_dm_older")
    );
    for event in transcript["transcript"]["events"].as_array().unwrap() {
        assert_eq!(event["scopeLabel"], json!("Direct message with Rowan"));
        assert_eq!(event["requestedByLabel"], json!("Rowan"));
    }
}

fn find_scope_label(value: &serde_json::Value) -> Option<&str> {
    match value {
        serde_json::Value::Object(object) => object
            .get("scopeLabel")
            .and_then(serde_json::Value::as_str)
            .or_else(|| object.values().find_map(find_scope_label)),
        serde_json::Value::Array(values) => values.iter().find_map(find_scope_label),
        _ => None,
    }
}

fn values<const N: usize>(items: [&str; N]) -> DashboardFilter {
    DashboardFilter::Values(items.into_iter().map(ToString::to_string).collect())
}

#[allow(clippy::too_many_arguments)]
async fn insert_event(
    store: &clankcord::store::TimelineStore,
    event_id: &str,
    scope_kind: &str,
    guild_id: &str,
    scope_id: &str,
    event_kind: &str,
    started_at: chrono::DateTime<Utc>,
    text: &str,
    state: &str,
    job_kind: &str,
    scope_label: Option<&str>,
) {
    let started_at_ms = instant_ms_dt(started_at);
    let mut payload = json!({
        "event_id": "payload-must-not-win",
        "eventId": "payload-must-not-win",
        "kind": event_kind,
        "text": text,
        "state": state,
        "job_kind": job_kind,
        "stt": {"provider": "large-provider", "tokens": vec!["heavy"; 5000]},
        "audio_bytes": vec![7; 100_000],
    });
    if let Some(scope_label) = scope_label {
        payload["voice_channel_name"] = json!(scope_label);
    }
    if scope_kind == "dm" {
        payload["requested_by_user_id"] = json!(scope_id);
    }
    sqlx::query(
        r#"
        INSERT INTO timeline_events(
          event_id, scope_kind, guild_id, scope_id, event_kind,
          started_at_ms, ended_at_ms, created_at_ms, text, payload_json
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
        "#,
    )
    .bind(event_id)
    .bind(scope_kind)
    .bind(guild_id)
    .bind(scope_id)
    .bind(event_kind)
    .bind(started_at_ms)
    .bind(started_at_ms + 1000)
    .bind(started_at_ms)
    .bind(text)
    .bind(payload)
    .execute(&store.pool)
    .await
    .unwrap();
}

async fn insert_taxonomy_event(
    store: &clankcord::store::TimelineStore,
    event_id: &str,
    event_kind: &str,
    started_at: chrono::DateTime<Utc>,
) {
    let started_at_ms = instant_ms_dt(started_at);
    sqlx::query(
        r#"
        INSERT INTO timeline_events(
          event_id, scope_kind, guild_id, scope_id, event_kind,
          started_at_ms, ended_at_ms, created_at_ms, text, payload_json
        )
        VALUES ($1, 'runtime', '', 'runtime', $2, $3, $3, $3, '', $4)
        "#,
    )
    .bind(event_id)
    .bind(event_kind)
    .bind(started_at_ms)
    .bind(json!({"event_kind": event_kind, "kind": event_kind}))
    .execute(&store.pool)
    .await
    .unwrap();
}

async fn insert_voice_room(
    store: &clankcord::store::TimelineStore,
    guild_id: &str,
    channel_id: &str,
    channel_name: &str,
) {
    sqlx::query(
        r#"
        INSERT INTO voice_rooms(
          guild_id, voice_channel_id, guild_slug, voice_channel_name,
          voice_channel_slug, updated_at_ms
        )
        VALUES ($1, $2, 'guild', $3, $4, $5)
        ON CONFLICT (guild_id, voice_channel_id) DO UPDATE
        SET voice_channel_name = EXCLUDED.voice_channel_name,
            updated_at_ms = EXCLUDED.updated_at_ms
        "#,
    )
    .bind(guild_id)
    .bind(channel_id)
    .bind(channel_name)
    .bind(channel_name.to_ascii_lowercase().replace(' ', "-"))
    .bind(instant_ms_dt(Utc::now()))
    .execute(&store.pool)
    .await
    .unwrap();
}
