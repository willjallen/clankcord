//! Audio-segment intake through the transcription mux planner: slot queueing,
//! priorities, recovery, and stream packing.

use std::collections::BTreeSet;

use chrono::{Duration, SecondsFormat, TimeZone, Utc};
use serde_json::json;

use clankcord::domain::Ctx;
use clankcord::domain::voice::capture::wake_activations::schedule_from_wake_event;
use clankcord::model::job::{AudioSegmentPayload, Job, JobKind, JobState};
use clankcord::time::isoformat_z;
use clankcord::util::sha256_file;

use crate::support::jobs::run_transcription_mux_planner;
use crate::support::jobs::{
    audio_segment_payload, create_audio_segment_slot, wake_activation_payload, write_test_wav,
};
use crate::support::{append_speech, dt, initialize_test_config, test_store};

#[tokio::test(flavor = "current_thread")]
async fn audio_segment_payload_references_ready_audio_artifact() {
    let start = chrono::Utc.with_ymd_and_hms(2026, 5, 13, 12, 0, 0).unwrap();
    let source_audio_path = std::path::PathBuf::from("/tmp/clankcord/segment.wav");
    let job = Job::audio_segment(AudioSegmentPayload {
        guild_id: "guild".to_string(),
        guild_slug: "guild".to_string(),
        voice_channel_id: "channel".to_string(),
        voice_channel_name: "Channel".to_string(),
        voice_channel_slug: "channel".to_string(),
        capture_run_id: "cap".to_string(),
        voice_bot_id: "bot".to_string(),
        voice_bot_discord_user_id: "bot-user".to_string(),
        speaker_user_id: "speaker".to_string(),
        speaker_label: "Speaker".to_string(),
        speaker_username: "speaker_name".to_string(),
        segment_start_time: start,
        segment_end_time: start + chrono::Duration::milliseconds(20),
        segment_index: 7,
        duration_ms: 20,
        source_audio_path: source_audio_path.clone(),
        audio_checksum: "sha256:test".to_string(),
        audio_bytes: 44,
        audio_format: "wav".to_string(),
        sample_rate_hz: 48_000,
        channels: 2,
        sample_width_bits: 16,
        post_processing: "pcm_s16le_to_wav".to_string(),
    });

    assert_eq!(job.kind, JobKind::AudioSegment);
    assert_eq!(
        job.audio_segment_payload().unwrap().source_audio_path,
        source_audio_path
    );
    let payload = job.payload_value();
    assert_eq!(
        payload["source_audio_path"],
        json!("/tmp/clankcord/segment.wav")
    );
    assert_eq!(payload["audio_bytes"], json!(44));
    assert!(payload.get("pcm").is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn audio_segment_job_queues_transcription_slot_and_mux_plan_job() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(&raw.path().join("voice")).await;
    let wav_path = raw.path().join("segment.wav");
    write_test_wav(&wav_path, 48_000, 2, 960);
    let checksum = sha256_file(&wav_path).unwrap();
    let start = Utc.with_ymd_and_hms(2026, 5, 13, 12, 0, 0).unwrap();
    let mut payload = audio_segment_payload(
        "guild",
        "code",
        "user-a",
        start,
        start + Duration::milliseconds(20),
        42,
    );
    payload.source_audio_path = wav_path;
    payload.audio_checksum = checksum;
    let job = store.create_job(Job::audio_segment(payload)).await.unwrap();
    let mut claimed = store
        .claim_due_jobs(JobKind::AudioSegment, 1, &mut BTreeSet::new())
        .await
        .unwrap();
    assert_eq!(claimed.len(), 1);

    let runtime = Ctx::new(store.clone());
    let result = clankcord::engine::dispatcher::dispatch_claimed_blocking_job(
        &runtime,
        claimed.pop().unwrap(),
    )
    .await
    .unwrap();

    assert_eq!(result["result"]["kind"], json!("audio_segment"));
    assert_eq!(
        result["result"]["status"],
        json!("queued_for_transcription")
    );
    assert_eq!(
        result["result"]["transcription_slot"]["transcription_source_id"],
        json!("local-granite")
    );
    assert_eq!(
        result["result"]["transcription_mux_plan_job"]["kind"],
        json!("transcription_mux_plan")
    );
    assert_eq!(
        store.get_job(&job.id).await.unwrap().state,
        JobState::Complete
    );
}

#[tokio::test(flavor = "current_thread")]
async fn audio_segment_slot_inherits_room_wake_priority() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(&raw.path().join("voice")).await;
    let now = Utc::now();
    let mut wake_payload = wake_activation_payload("guild", "code");
    wake_payload.wake_started_at = isoformat_z(Some(now - Duration::seconds(20)));
    wake_payload.latest_wake_at = isoformat_z(Some(now - Duration::seconds(20)));
    wake_payload.max_window_seconds = 3600;
    store
        .create_job(Job::wake_activation(wake_payload))
        .await
        .unwrap();

    let wav_path = raw.path().join("segment-priority.wav");
    write_test_wav(&wav_path, 48_000, 2, 960);
    let checksum = sha256_file(&wav_path).unwrap();
    let mut payload = audio_segment_payload(
        "guild",
        "code",
        "user-b",
        now - Duration::seconds(10),
        now - Duration::seconds(9),
        7,
    );
    payload.source_audio_path = wav_path;
    payload.audio_checksum = checksum;
    let job = store.create_job(Job::audio_segment(payload)).await.unwrap();
    let claimed = store
        .claim_due_jobs(JobKind::AudioSegment, 1, &mut BTreeSet::new())
        .await
        .unwrap();
    let runtime = Ctx::new(store.clone());
    clankcord::engine::dispatcher::dispatch_claimed_blocking_job(
        &runtime,
        claimed.into_iter().next().unwrap(),
    )
    .await
    .unwrap();

    let row = sqlx::query("SELECT priority FROM transcription_slots WHERE source_job_id = $1")
        .bind(&job.id)
        .fetch_one(&store.pool)
        .await
        .unwrap();
    assert_eq!(
        sqlx::Row::try_get::<i64, _>(&row, "priority").unwrap(),
        1000
    );
    let claimed_planner = store
        .claim_due_jobs(JobKind::TranscriptionMuxPlan, 1, &mut BTreeSet::new())
        .await
        .unwrap();
    assert_eq!(claimed_planner.len(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn wake_activation_promotes_existing_overlapping_transcription_slots() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(&raw.path().join("voice")).await;
    let runtime = Ctx::new(store.clone());
    let now = Utc::now();
    let source_job_id = create_audio_segment_slot(
        &store,
        &runtime,
        raw.path(),
        "user-a",
        now - Duration::seconds(10),
        Duration::seconds(4),
        44,
    )
    .await;
    let before = transcription_slot_priority(&store, &source_job_id).await;
    assert_eq!(before, 0);
    let wake_started_at = now - Duration::seconds(9);
    let wake = store
        .append_event(
            "guild",
            "code",
            json!({
                "event_kind": "wake_detected",
                "kind": "wake_detected",
                "capture_run_id": "cap_test",
                "speaker_user_id": "user-a",
                "speakerId": "user-a",
                "speaker_label": "Will",
                "speakerLabel": "Will",
                "startedAt": wake_started_at.to_rfc3339_opts(SecondsFormat::Millis, true),
                "endedAt": (wake_started_at + Duration::milliseconds(500)).to_rfc3339_opts(SecondsFormat::Millis, true),
                "wake": {"wake": true, "score": 0.91},
                "wake_detected": true,
            }),
        )
        .await
        .unwrap();

    schedule_from_wake_event(&runtime, &wake).await.unwrap();

    assert_eq!(
        transcription_slot_priority(&store, &source_job_id).await,
        1000
    );
}

#[tokio::test(flavor = "current_thread")]
async fn transcription_slot_recovery_handles_terminal_mux_jobs() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(&raw.path().join("voice")).await;
    let runtime = Ctx::new(store.clone());
    let now = Utc::now();
    let mut source_job_ids = Vec::new();
    for index in 0..2 {
        let wav_path = raw.path().join(format!("segment-recovery-{index}.wav"));
        write_test_wav(&wav_path, 48_000, 2, 960);
        let checksum = sha256_file(&wav_path).unwrap();
        let mut payload = audio_segment_payload(
            "guild",
            "code",
            "user-a",
            now + Duration::seconds(index),
            now + Duration::seconds(index + 1),
            20 + index,
        );
        payload.source_audio_path = wav_path;
        payload.audio_checksum = checksum;
        let job = store.create_job(Job::audio_segment(payload)).await.unwrap();
        let claimed = store
            .claim_due_jobs(JobKind::AudioSegment, 1, &mut BTreeSet::new())
            .await
            .unwrap();
        clankcord::engine::dispatcher::dispatch_claimed_blocking_job(
            &runtime,
            claimed.into_iter().next().unwrap(),
        )
        .await
        .unwrap();
        source_job_ids.push(job.id);
    }

    let mut timed_out_mux = Job::transcription_mux("local-granite");
    timed_out_mux.set_state(JobState::FailedTimeout);
    timed_out_mux.metadata.error = "job exceeded stale running-job timeout".to_string();
    let timed_out_mux = store.create_job(timed_out_mux).await.unwrap();
    let mut failed_mux = Job::transcription_mux("local-granite");
    failed_mux.set_state(JobState::Failed);
    failed_mux.metadata.error = "mux audio could not be built".to_string();
    let failed_mux = store.create_job(failed_mux).await.unwrap();

    sqlx::query(
        r#"
        UPDATE transcription_slots
        SET state = 'muxing',
            mux_job_id = $1,
            mux_stream_id = 'mux:test',
            mux_start_ms = 0,
            mux_end_ms = 20
        WHERE source_job_id = $2
        "#,
    )
    .bind(&timed_out_mux.id)
    .bind(&source_job_ids[0])
    .execute(&store.pool)
    .await
    .unwrap();
    sqlx::query(
        r#"
        UPDATE transcription_slots
        SET state = 'muxing',
            mux_job_id = $1,
            mux_stream_id = 'mux:test',
            mux_start_ms = 20,
            mux_end_ms = 40
        WHERE source_job_id = $2
        "#,
    )
    .bind(&failed_mux.id)
    .bind(&source_job_ids[1])
    .execute(&store.pool)
    .await
    .unwrap();

    let recovered = store.recover_abandoned_transcription_slots().await.unwrap();
    assert_eq!(recovered["requeued"].as_array().unwrap().len(), 1);
    assert_eq!(recovered["failed"].as_array().unwrap().len(), 1);

    let requeued =
        sqlx::query("SELECT state, mux_job_id FROM transcription_slots WHERE source_job_id = $1")
            .bind(&source_job_ids[0])
            .fetch_one(&store.pool)
            .await
            .unwrap();
    assert_eq!(
        sqlx::Row::try_get::<String, _>(&requeued, "state").unwrap(),
        "queued"
    );
    assert_eq!(
        sqlx::Row::try_get::<String, _>(&requeued, "mux_job_id").unwrap(),
        ""
    );

    let failed = sqlx::query("SELECT state FROM transcription_slots WHERE source_job_id = $1")
        .bind(&source_job_ids[1])
        .fetch_one(&store.pool)
        .await
        .unwrap();
    assert_eq!(
        sqlx::Row::try_get::<String, _>(&failed, "state").unwrap(),
        "failed"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn transcription_mux_planner_uses_one_stream_without_predicted_backlog() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(&raw.path().join("voice")).await;
    let runtime = Ctx::new(store.clone());
    let now = Utc::now();
    for index in 0..2 {
        create_audio_segment_slot(
            &store,
            &runtime,
            raw.path(),
            "user-a",
            now + Duration::milliseconds(index * 2500),
            Duration::seconds(2),
            100 + index,
        )
        .await;
    }

    let result = run_transcription_mux_planner(&store).await;

    assert_eq!(result["result"]["status"], json!("planned"));
    assert_eq!(
        result["result"]["created_mux_jobs"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let row = sqlx::query(
        "SELECT COUNT(DISTINCT mux_job_id) AS mux_jobs FROM transcription_slots WHERE state = 'planned'",
    )
    .fetch_one(&store.pool)
    .await
    .unwrap();
    assert_eq!(sqlx::Row::try_get::<i64, _>(&row, "mux_jobs").unwrap(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn transcription_mux_planner_overflows_when_one_stream_misses_deadlines() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(&raw.path().join("voice")).await;
    let runtime = Ctx::new(store.clone());
    let base = Utc::now() - Duration::seconds(90);
    for index in 0..8 {
        create_audio_segment_slot(
            &store,
            &runtime,
            raw.path(),
            &format!("user-{}", index % 4),
            base + Duration::seconds(index * 8),
            Duration::seconds(8),
            200 + index,
        )
        .await;
    }

    let result = run_transcription_mux_planner(&store).await;

    assert_eq!(result["result"]["status"], json!("planned"));
    assert_eq!(
        result["result"]["created_mux_jobs"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let row = sqlx::query(
        "SELECT COUNT(DISTINCT mux_job_id) AS mux_jobs FROM transcription_slots WHERE state = 'planned'",
    )
    .fetch_one(&store.pool)
    .await
    .unwrap();
    assert_eq!(sqlx::Row::try_get::<i64, _>(&row, "mux_jobs").unwrap(), 2);
}

#[tokio::test(flavor = "current_thread")]
async fn transcription_mux_planner_fairly_packs_short_room_speaker_work() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(&raw.path().join("voice")).await;
    let runtime = Ctx::new(store.clone());
    let base = Utc::now() - Duration::seconds(90);
    for index in 0..8 {
        create_audio_segment_slot(
            &store,
            &runtime,
            raw.path(),
            "user-a",
            base + Duration::seconds(index * 8),
            Duration::seconds(8),
            300 + index,
        )
        .await;
    }
    create_audio_segment_slot(
        &store,
        &runtime,
        raw.path(),
        "user-b",
        base + Duration::seconds(80),
        Duration::seconds(2),
        400,
    )
    .await;

    let result = run_transcription_mux_planner(&store).await;
    let first_mux_job_id = result["result"]["created_mux_jobs"][0]["job_id"]
        .as_str()
        .unwrap()
        .to_string();
    let rows = sqlx::query(
        r#"
        SELECT speaker_user_id
        FROM transcription_slots
        WHERE mux_job_id = $1
        ORDER BY created_at_ms, slot_id
        "#,
    )
    .bind(first_mux_job_id)
    .fetch_all(&store.pool)
    .await
    .unwrap();
    let speakers = rows
        .iter()
        .map(|row| sqlx::Row::try_get::<String, _>(row, "speaker_user_id").unwrap())
        .collect::<Vec<_>>();

    assert!(speakers.contains(&"user-b".to_string()));
}

async fn transcription_slot_priority(
    store: &clankcord::store::TimelineStore,
    source_job_id: &str,
) -> i64 {
    let row = sqlx::query("SELECT priority FROM transcription_slots WHERE source_job_id = $1")
        .bind(source_job_id)
        .fetch_one(&store.pool)
        .await
        .unwrap();
    sqlx::Row::try_get::<i64, _>(&row, "priority").unwrap()
}

#[tokio::test(flavor = "current_thread")]
async fn timeline_finds_existing_speech_segment_for_audio_retry() {
    let raw = tempfile::tempdir().unwrap();
    let store = test_store(raw.path()).await;
    let start = dt(2026, 5, 12, 16, 0, 0);
    let event = append_speech(
        &store,
        raw.path(),
        start,
        start + chrono::Duration::seconds(2),
        "retry-safe words",
        4,
        None,
    )
    .await;
    let found = store
        .speech_event_for_segment("guild", "code", "cap_test", "user-a", 4)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found["event_id"], event["event_id"]);
    let (count, last) = store
        .speech_stats_for_capture_run("guild", "code", "cap_test")
        .await
        .unwrap();
    assert_eq!(count, 1);
    assert_eq!(last.unwrap(), start + chrono::Duration::seconds(2));
}
