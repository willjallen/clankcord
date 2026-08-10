//! Pins for transcription recovery: retryable failures requeue for mux
//! planning, and replanned slots stay isolated for exact attribution
//! (3eb5006, b04131d).

use crate::support::initialize_test_config;
use crate::support::jobs::create_audio_segment_slot;
use crate::support::jobs::run_transcription_mux_planner;
use crate::support::test_store;
use chrono::Duration;
use chrono::Utc;
use clankcord::domain::Ctx;
use serde_json::json;

#[tokio::test(flavor = "current_thread")]
async fn transcription_mux_planner_isolates_slots_replanned_for_exact_attribution() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(&raw.path().join("voice")).await;
    let runtime = Ctx::new(store.clone());
    let base = Utc::now() - Duration::seconds(10);
    let constrained_source_job_id = create_audio_segment_slot(
        &store,
        &runtime,
        raw.path(),
        "user-a",
        base,
        Duration::seconds(2),
        450,
    )
    .await;
    create_audio_segment_slot(
        &store,
        &runtime,
        raw.path(),
        "user-b",
        base + Duration::seconds(3),
        Duration::seconds(2),
        451,
    )
    .await;
    sqlx::query(
        r#"
        UPDATE transcription_slots
        SET requires_single_slot = TRUE,
            payload_json = payload_json || jsonb_build_object('requires_single_slot', TRUE)
        WHERE source_job_id = $1
        "#,
    )
    .bind(&constrained_source_job_id)
    .execute(&store.pool)
    .await
    .unwrap();

    let result = run_transcription_mux_planner(&store).await;

    assert_eq!(result["result"]["status"], json!("planned"));
    let row = sqlx::query(
        r#"
        SELECT constrained.mux_job_id,
               COUNT(all_slots.slot_id) AS slot_count
        FROM transcription_slots constrained
        JOIN transcription_slots all_slots ON all_slots.mux_job_id = constrained.mux_job_id
        WHERE constrained.source_job_id = $1
        GROUP BY constrained.mux_job_id
        "#,
    )
    .bind(&constrained_source_job_id)
    .fetch_one(&store.pool)
    .await
    .unwrap();
    assert!(
        !sqlx::Row::try_get::<String, _>(&row, "mux_job_id")
            .unwrap()
            .is_empty()
    );
    assert_eq!(sqlx::Row::try_get::<i64, _>(&row, "slot_count").unwrap(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn retryable_failed_transcription_slots_requeue_for_mux_planning() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(&raw.path().join("voice")).await;
    let runtime = Ctx::new(store.clone());
    let now = Utc::now();
    let retryable_source_job_id = create_audio_segment_slot(
        &store,
        &runtime,
        raw.path(),
        "user-a",
        now - Duration::seconds(10),
        Duration::seconds(2),
        70,
    )
    .await;
    let non_retryable_source_job_id = create_audio_segment_slot(
        &store,
        &runtime,
        raw.path(),
        "user-b",
        now - Duration::seconds(8),
        Duration::seconds(2),
        71,
    )
    .await;
    let retryable_error = r#"elevenlabs speech-to-text HTTP 500 Internal Server Error: {"detail":{"type":"internal_error","code":"internal_error","message":"An unexpected error occurred."}}"#;
    sqlx::query(
        r#"
        UPDATE transcription_slots
        SET state = 'failed',
            mux_job_id = 'job_retryable_mux',
            mux_stream_id = 'mux:retryable',
            mux_start_ms = 0,
            mux_end_ms = 1000,
            guard_before_ms = 0,
            guard_after_ms = 150,
            payload_json = payload_json || jsonb_build_object(
              'state', 'failed',
              'mux_job_id', 'job_retryable_mux',
              'mux_stream_id', 'mux:retryable',
              'mux_start_ms', 0,
              'mux_end_ms', 1000,
              'guard_after_ms', 150,
              'error', $2
            )
        WHERE source_job_id = $1
        "#,
    )
    .bind(&retryable_source_job_id)
    .bind(retryable_error)
    .execute(&store.pool)
    .await
    .unwrap();
    sqlx::query(
        r#"
        UPDATE transcription_slots
        SET state = 'failed',
            payload_json = payload_json || jsonb_build_object(
              'state', 'failed',
              'error', 'provider returned malformed timestamp payload'
            )
        WHERE source_job_id = $1
        "#,
    )
    .bind(&non_retryable_source_job_id)
    .execute(&store.pool)
    .await
    .unwrap();

    let requeued = store
        .requeue_retryable_failed_transcription_slots(10)
        .await
        .unwrap();

    assert_eq!(requeued.len(), 1);
    assert_eq!(
        requeued[0]["source_job_id"].as_str().unwrap(),
        retryable_source_job_id
    );
    let retryable = sqlx::query(
        r#"
        SELECT state,
               mux_job_id,
               mux_stream_id,
               mux_start_ms,
               payload_json ? 'error' AS has_error
        FROM transcription_slots
        WHERE source_job_id = $1
        "#,
    )
    .bind(&retryable_source_job_id)
    .fetch_one(&store.pool)
    .await
    .unwrap();
    assert_eq!(
        sqlx::Row::try_get::<String, _>(&retryable, "state").unwrap(),
        "queued"
    );
    assert_eq!(
        sqlx::Row::try_get::<String, _>(&retryable, "mux_job_id").unwrap(),
        ""
    );
    assert_eq!(
        sqlx::Row::try_get::<String, _>(&retryable, "mux_stream_id").unwrap(),
        ""
    );
    assert!(
        sqlx::Row::try_get::<Option<i64>, _>(&retryable, "mux_start_ms")
            .unwrap()
            .is_none()
    );
    assert!(!sqlx::Row::try_get::<bool, _>(&retryable, "has_error").unwrap());
    let non_retryable =
        sqlx::query("SELECT state FROM transcription_slots WHERE source_job_id = $1")
            .bind(&non_retryable_source_job_id)
            .fetch_one(&store.pool)
            .await
            .unwrap();
    assert_eq!(
        sqlx::Row::try_get::<String, _>(&non_retryable, "state").unwrap(),
        "failed"
    );
    let result = run_transcription_mux_planner(&store).await;
    assert_eq!(result["result"]["status"], json!("planned"));
    let planned = sqlx::query("SELECT state FROM transcription_slots WHERE source_job_id = $1")
        .bind(&retryable_source_job_id)
        .fetch_one(&store.pool)
        .await
        .unwrap();
    assert_eq!(
        sqlx::Row::try_get::<String, _>(&planned, "state").unwrap(),
        "planned"
    );
}
