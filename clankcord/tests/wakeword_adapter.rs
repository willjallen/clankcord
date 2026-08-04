use serde_json::json;

use chrono::{Duration, Utc};

use clankcord::adapters::wakeword::{WakeCircuitAdmission, WakeCircuitBreaker, parse_wake_payload};

#[tokio::test(flavor = "current_thread")]
async fn wakeword_payload_parser_preserves_detector_metadata() {
    let result = parse_wake_payload(&json!({
        "wake": true,
        "score": 0.73,
        "threshold": "0.5",
        "model_label": "hey_clanky",
        "stream_id": "guild123:channel456:user789",
        "processed_frames": 3,
        "scores": {"hey_clanky": 0.73},
        "extra": {"adapter": "local-stt"}
    }));

    assert!(result.wake);
    assert_eq!(result.score, Some(0.73));
    assert_eq!(result.threshold, Some(0.5));
    assert_eq!(result.model_label, "hey_clanky");
    assert_eq!(result.stream_id, "guild123:channel456:user789");
    assert_eq!(result.processed_frames, Some(3));
    assert_eq!(result.to_json()["scores"]["hey_clanky"], json!(0.73));
    assert_eq!(result.to_json()["extra"]["adapter"], json!("local-stt"));
}

#[test]
fn wake_provider_circuit_opens_backs_off_and_recovers_with_one_half_open_probe() {
    let circuit = WakeCircuitBreaker::new(3, 30, 120);
    let now = Utc::now();

    for _ in 0..3 {
        assert_eq!(circuit.admit(now), Some(WakeCircuitAdmission::Closed));
        circuit.record_failure(now, "connection refused with secret-looking detail");
    }

    let open = circuit.snapshot(now + Duration::seconds(1));
    assert_eq!(open["status"], json!("open"));
    assert_eq!(open["consecutiveFailures"], json!(3));
    assert!(circuit.submission_suppressed(now + Duration::seconds(1)));
    assert_eq!(circuit.admit(now + Duration::seconds(1)), None);

    assert_eq!(
        circuit.admit(now + Duration::seconds(30)),
        Some(WakeCircuitAdmission::HalfOpen)
    );
    assert_eq!(circuit.admit(now + Duration::seconds(30)), None);
    circuit.record_failure(now + Duration::seconds(30), "connection refused again");
    assert_eq!(
        circuit.snapshot(now + Duration::seconds(31))["status"],
        json!("open")
    );

    assert_eq!(
        circuit.admit(now + Duration::seconds(90)),
        Some(WakeCircuitAdmission::HalfOpen)
    );
    circuit.record_success(now + Duration::seconds(90));
    let recovered = circuit.snapshot(now + Duration::seconds(90));
    assert_eq!(recovered["status"], json!("closed"));
    assert_eq!(recovered["consecutiveFailures"], json!(0));
}
