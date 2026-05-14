use serde_json::json;

use clankcord::adapters::wakeword::parse_wake_payload;

#[test]
fn wakeword_payload_parser_preserves_detector_metadata() {
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
