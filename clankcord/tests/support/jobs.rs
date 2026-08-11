//! Job payload and artifact fixture builders shared across category binaries.

use std::collections::BTreeSet;

use chrono::{Duration, TimeZone, Utc};
use serde_json::json;

use clankcord::domain::Ctx;
use clankcord::model::job::{
    AudioSegmentPayload, Job, JobKind, TextDeliveryPayload, WakeActivationPayload, WakeProbePayload,
};
use clankcord::util::sha256_file;

pub fn text_delivery_payload(content: &str) -> TextDeliveryPayload {
    TextDeliveryPayload::from_json(&json!({
        "intent": "message",
        "target": "agent_chat",
        "requested_by_user_id": "user-a",
        "content": content,
    }))
    .unwrap()
}
pub fn raw_path(path: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(path)
}
pub fn wake_probe_payload(stream_id: &str, probe_index: i64) -> WakeProbePayload {
    let start = Utc.with_ymd_and_hms(2026, 5, 13, 12, 0, 0).unwrap()
        + chrono::Duration::milliseconds(probe_index * 500);
    WakeProbePayload {
        guild_id: "guild".to_string(),
        guild_slug: "guild".to_string(),
        voice_channel_id: "code".to_string(),
        voice_channel_name: "Code".to_string(),
        voice_channel_slug: "code".to_string(),
        capture_run_id: "cap".to_string(),
        voice_bot_id: "bot".to_string(),
        voice_bot_discord_user_id: "bot-user".to_string(),
        speaker_user_id: "user-a".to_string(),
        speaker_label: "Will".to_string(),
        speaker_username: "will".to_string(),
        probe_start_time: start,
        probe_end_time: start + chrono::Duration::milliseconds(500),
        probe_index,
        duration_ms: 500,
        source_audio_path: raw_path("/tmp/clankcord/wake-probe.wav"),
        audio_checksum: "sha256:test".to_string(),
        audio_bytes: 44,
        audio_format: "wav".to_string(),
        sample_rate_hz: 48_000,
        channels: 2,
        sample_width_bits: 16,
        post_processing: "pcm_s16le_to_wav".to_string(),
        stream_id: stream_id.to_string(),
        reset_stream: true,
    }
}
pub fn wake_activation_payload(guild_id: &str, voice_channel_id: &str) -> WakeActivationPayload {
    WakeActivationPayload {
        activation_id: "act_route".to_string(),
        guild_id: guild_id.to_string(),
        voice_channel_id: voice_channel_id.to_string(),
        voice_channel_name: "Code".to_string(),
        speaker_user_id: "user-a".to_string(),
        speaker_label: "Will".to_string(),
        wake_event_id: "evt_wake".to_string(),
        wake_started_at: "2026-05-14T12:00:00.000Z".to_string(),
        wake_ended_at: "2026-05-14T12:00:01.000Z".to_string(),
        latest_wake_event_id: "evt_wake".to_string(),
        latest_wake_at: "2026-05-14T12:00:00.000Z".to_string(),
        lookback_seconds: 30,
        min_post_seconds: 5,
        speaker_idle_seconds: 5,
        stt_flush_grace_seconds: 2,
        max_window_seconds: 60,
        additive_preempt_seconds: 10,
        independent_after_seconds: 45,
        amended_wake_event_ids: Vec::new(),
        replacement_of_job_ids: Vec::new(),
    }
}
pub async fn create_audio_segment_slot(
    store: &clankcord::store::TimelineStore,
    runtime: &Ctx,
    root: &std::path::Path,
    speaker_user_id: &str,
    start: chrono::DateTime<Utc>,
    duration: Duration,
    segment_index: i64,
) -> String {
    let wav_path = root.join(format!("planner-segment-{segment_index}.wav"));
    write_test_wav(&wav_path, 48_000, 2, 960);
    let checksum = sha256_file(&wav_path).unwrap();
    let mut payload = audio_segment_payload(
        "guild",
        "code",
        speaker_user_id,
        start,
        start + duration,
        segment_index,
    );
    payload.source_audio_path = wav_path;
    payload.audio_checksum = checksum;
    let job = store.create_job(Job::audio_segment(payload)).await.unwrap();
    let claimed = store
        .claim_due_jobs(JobKind::AudioSegment, 1, &mut BTreeSet::new())
        .await
        .unwrap();
    clankcord::engine::dispatcher::dispatch_claimed_blocking_job(
        runtime,
        claimed.into_iter().next().unwrap(),
    )
    .await
    .unwrap();
    job.id
}
pub fn audio_segment_payload(
    guild_id: &str,
    voice_channel_id: &str,
    speaker_user_id: &str,
    start: chrono::DateTime<Utc>,
    end: chrono::DateTime<Utc>,
    segment_index: i64,
) -> AudioSegmentPayload {
    AudioSegmentPayload {
        guild_id: guild_id.to_string(),
        guild_slug: "guild".to_string(),
        voice_channel_id: voice_channel_id.to_string(),
        voice_channel_name: "Code".to_string(),
        voice_channel_slug: "code".to_string(),
        capture_run_id: "cap_test".to_string(),
        voice_bot_id: "clanky-vc1".to_string(),
        voice_bot_discord_user_id: "bot-user".to_string(),
        speaker_user_id: speaker_user_id.to_string(),
        speaker_label: "Will".to_string(),
        speaker_username: "will".to_string(),
        segment_start_time: start,
        segment_end_time: end,
        segment_index,
        duration_ms: (end - start).num_milliseconds(),
        source_audio_path: std::path::PathBuf::from(format!("/tmp/audio-{segment_index}.wav")),
        audio_checksum: format!("sha256:{segment_index}"),
        audio_bytes: 123,
        audio_format: "wav".to_string(),
        sample_rate_hz: 48_000,
        channels: 2,
        sample_width_bits: 16,
        post_processing: "pcm_s16le_48khz_stereo_to_wav".to_string(),
    }
}
pub fn write_test_wav(path: &std::path::Path, sample_rate: u32, channels: u16, frames: usize) {
    let spec = hound::WavSpec {
        channels,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec).unwrap();
    for index in 0..frames * channels as usize {
        writer.write_sample((index as i16).wrapping_mul(3)).unwrap();
    }
    writer.finalize().unwrap();
}

pub async fn run_transcription_mux_planner(
    store: &clankcord::store::TimelineStore,
) -> serde_json::Value {
    store
        .create_job(Job::transcription_mux_plan("local-granite", 0))
        .await
        .unwrap();
    let claimed = store
        .claim_due_jobs(JobKind::TranscriptionMuxPlan, 1, &mut BTreeSet::new())
        .await
        .unwrap();
    let runtime = Ctx::new(store.clone());
    clankcord::engine::dispatcher::dispatch_claimed_runtime_job(
        &runtime,
        &clankcord::ports::discord::DiscordApiUnavailable,
        claimed.into_iter().next().unwrap(),
    )
    .await
    .unwrap()
}
