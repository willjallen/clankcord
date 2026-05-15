use serde_json::json;

use clankcord::adapters::stt::{
    parse_stt_payload, should_drop_low_confidence_transcription, stt_avg_token_logprob,
    stt_drop_decision, stt_no_speech_probability,
};

#[tokio::test(flavor = "current_thread")]
async fn stt_payload_parser_preserves_quality_metadata() {
    let result = parse_stt_payload(&json!({
        "text": "Clanky",
        "logprobs": [
            {"token": "Cl", "logprob": -2.0, "bytes": [67, 108]},
            {"token": "anky", "logprob": -3.0, "bytes": [97, 110, 107, 121]}
        ],
        "segments": [{"avg_logprob": -2.82, "compression_ratio": 0.42, "no_speech_prob": null}],
        "local": {"avg_logprob": -2.81, "audio_rms": 0.0, "audio_peak": 0.0, "estimated_no_speech_prob": 1.0}
    }));

    assert_eq!(result.text, "Clanky");
    assert_eq!(result.metadata["avg_logprob"], json!(-2.81));
    assert_eq!(result.metadata["compression_ratio"], json!(0.42));
    assert_eq!(
        result.metadata["local"]["estimated_no_speech_prob"],
        json!(1.0)
    );
    assert_eq!(result.metadata["tokens"]["token_count"], json!(2));
    assert_eq!(result.metadata["tokens"]["avg_token_logprob"], json!(-2.5));
    assert_eq!(result.metadata["token_logprobs"][0]["token"], json!("Cl"));
}

#[tokio::test(flavor = "current_thread")]
async fn low_confidence_filter_uses_no_speech_or_token_average() {
    let result = parse_stt_payload(&json!({
        "text": "Clanky",
        "segments": [{"no_speech_prob": 0.4}],
        "local": {"estimated_no_speech_prob": 0.71}
    }));
    assert_eq!(
        stt_no_speech_probability(Some(&result.metadata)),
        Some(0.71)
    );
    assert!(should_drop_low_confidence_transcription(
        Some(&result.metadata),
        Some(0.7),
        None
    ));
    assert!(!should_drop_low_confidence_transcription(
        Some(&result.metadata),
        Some(0.71),
        None
    ));

    let low_token = parse_stt_payload(&json!({
        "text": "Clanky",
        "logprobs": [{"token": "Cl", "logprob": -0.9}, {"token": "anky", "logprob": -1.1}],
        "segments": [{"no_speech_prob": 0.1}],
        "local": {"estimated_no_speech_prob": 0.1}
    }));
    let decision = stt_drop_decision(Some(&low_token.metadata), Some(0.7), Some(-0.8));
    assert_eq!(stt_avg_token_logprob(Some(&low_token.metadata)), Some(-1.0));
    assert_eq!(decision["drop"], json!(true));
    assert_eq!(decision["reasons"], json!(["avg_token_logprob"]));
}
