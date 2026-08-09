//! Durable wake circuit: threshold opening, backoff, half-open leases, restart survival.



use clankcord::store::WakeCircuitAdmission;


const LEASE_MS: i64 = 60_000;

#[tokio::test(flavor = "current_thread")]
async fn wake_circuit_opens_backs_off_and_recovers_with_one_half_open_probe() {
    let root = tempfile::tempdir().unwrap();
    let store = crate::support::test_store(root.path()).await;
    let now = 1_000_000_000_000_i64;

    for attempt in 0..3 {
        assert_eq!(
            store.wake_circuit_admit(now, LEASE_MS).await.unwrap(),
            Some(WakeCircuitAdmission::Closed),
            "attempt {attempt} admits while closed"
        );
        store
            .wake_circuit_record_failure(now, "connection refused", 3, 30, 120)
            .await
            .unwrap();
    }

    let open = store.wake_circuit_row().await.unwrap();
    assert_eq!(open.consecutive_failures, 3);
    assert_eq!(open.open_until_ms, Some(now + 30_000));
    assert_eq!(
        store
            .wake_circuit_admit(now + 1_000, LEASE_MS)
            .await
            .unwrap(),
        None,
        "open circuit suppresses probes"
    );

    // Open window elapsed: exactly one probe claims the half-open slot.
    assert_eq!(
        store
            .wake_circuit_admit(now + 30_000, LEASE_MS)
            .await
            .unwrap(),
        Some(WakeCircuitAdmission::HalfOpen)
    );
    assert_eq!(
        store
            .wake_circuit_admit(now + 30_000, LEASE_MS)
            .await
            .unwrap(),
        None,
        "half-open slot admits a single probe"
    );

    // Half-open failure doubles the backoff.
    store
        .wake_circuit_record_failure(now + 30_000, "connection refused again", 3, 30, 120)
        .await
        .unwrap();
    let reopened = store.wake_circuit_row().await.unwrap();
    assert_eq!(reopened.open_until_ms, Some(now + 30_000 + 60_000));
    assert_eq!(reopened.half_open_started_at_ms, None);

    // Second half-open probe succeeds and closes the circuit fully.
    assert_eq!(
        store
            .wake_circuit_admit(now + 90_000, LEASE_MS)
            .await
            .unwrap(),
        Some(WakeCircuitAdmission::HalfOpen)
    );
    store
        .wake_circuit_record_success(now + 90_000)
        .await
        .unwrap();
    let recovered = store.wake_circuit_row().await.unwrap();
    assert_eq!(recovered.consecutive_failures, 0);
    assert_eq!(recovered.open_until_ms, None);
    assert_eq!(recovered.half_open_started_at_ms, None);
    assert_eq!(
        store
            .wake_circuit_admit(now + 91_000, LEASE_MS)
            .await
            .unwrap(),
        Some(WakeCircuitAdmission::Closed)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn wake_circuit_state_survives_a_new_store_and_releases_stale_half_open_leases() {
    let root = tempfile::tempdir().unwrap();
    let store = crate::support::test_store(root.path()).await;
    let now = 1_000_000_000_000_i64;

    for _ in 0..2 {
        store
            .wake_circuit_record_failure(now, "provider down", 2, 30, 120)
            .await
            .unwrap();
    }
    assert_eq!(
        store
            .wake_circuit_admit(now + 1_000, LEASE_MS)
            .await
            .unwrap(),
        None,
        "circuit opened at the failure threshold"
    );

    // A different handle to the same database sees the same open circuit —
    // the row, not process memory, is the source of truth.
    let schema: String = sqlx::query_scalar("SELECT current_schema()")
        .fetch_one(&store.pool)
        .await
        .unwrap();
    let restarted = clankcord::store::TimelineStore::new_with_database(
        Some(root.path().to_path_buf()),
        store.database_url.clone(),
        schema,
    )
    .unwrap();
    assert_eq!(
        restarted
            .wake_circuit_admit(now + 1_000, LEASE_MS)
            .await
            .unwrap(),
        None,
        "restart keeps an open circuit open"
    );

    // Claim the half-open slot, then abandon it (process death). After the
    // lease expires another probe may claim the slot.
    assert_eq!(
        restarted
            .wake_circuit_admit(now + 30_000, LEASE_MS)
            .await
            .unwrap(),
        Some(WakeCircuitAdmission::HalfOpen)
    );
    assert_eq!(
        restarted
            .wake_circuit_admit(now + 31_000, LEASE_MS)
            .await
            .unwrap(),
        None,
        "live lease blocks other probes"
    );
    assert_eq!(
        restarted
            .wake_circuit_admit(now + 30_000 + LEASE_MS + 1_000, LEASE_MS)
            .await
            .unwrap(),
        Some(WakeCircuitAdmission::HalfOpen),
        "expired lease releases the half-open slot"
    );
}
