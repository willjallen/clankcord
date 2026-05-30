mod common;

use clankcord::adapters::discord::voice::artifacts::PCM_20MS_SILENCE;
use clankcord::adapters::discord::voice::capture::{
    CaptureAction, CaptureUser, VoiceCaptureSink, VoiceData,
};
use clankcord::adapters::discord::voice::session::{
    AudioPipelineOutcome, SegmentCloseReason, SessionAudioPipeline, WakeProbeConfig,
};

use common::test_voice_session;

fn pcm_frame(amplitude: i16) -> Vec<u8> {
    let mut pcm = Vec::with_capacity(PCM_20MS_SILENCE.len());
    let bytes = amplitude.to_le_bytes();
    for _ in 0..(PCM_20MS_SILENCE.len() / 4) {
        pcm.extend_from_slice(&bytes);
        pcm.extend_from_slice(&bytes);
    }
    pcm
}

fn pcm_ms(amplitude: i16, ms: usize) -> Vec<u8> {
    let frame = pcm_frame(amplitude);
    let frames = (ms / 20).max(1);
    let mut pcm = Vec::with_capacity(frame.len() * frames);
    for _ in 0..frames {
        pcm.extend_from_slice(&frame);
    }
    pcm
}

#[tokio::test(flavor = "current_thread")]
async fn capture_sink_translates_voice_packets_to_pipeline_actions() {
    let user = CaptureUser {
        id: "user-a".to_string(),
        display_name: "Will".to_string(),
        global_name: String::new(),
        name: "will".to_string(),
    };
    let mut sink = VoiceCaptureSink::new("session-1");

    let actions = sink.write_actions(VoiceData {
        user: Some(user.clone()),
        pcm: vec![1, 2, 3, 4],
        has_packet: true,
        is_silence: false,
    });
    assert!(
        matches!(actions[0], CaptureAction::PacketDebug { ref key, .. } if key == "writeCalls")
    );
    assert!(
        matches!(actions[1], CaptureAction::PacketDebug { ref key, .. } if key == "pcmPackets")
    );
    assert!(
        matches!(actions[2], CaptureAction::PcmPacket { ref user_id, ref label, .. } if user_id == "user-a" && label == "Will")
    );

    let silence_actions = sink.write_actions(VoiceData {
        user: Some(user),
        pcm: Vec::new(),
        has_packet: true,
        is_silence: true,
    });
    assert!(
        matches!(silence_actions.last(), Some(CaptureAction::SilencePacket { pcm, .. }) if pcm.len() == PCM_20MS_SILENCE.len())
    );

    let missing_user_actions = sink.write_actions(VoiceData {
        user: None,
        pcm: vec![1, 2],
        has_packet: true,
        is_silence: false,
    });
    assert!(missing_user_actions.iter().any(
        |action| matches!(action, CaptureAction::PacketDebug { key, .. } if key == "missingUserPackets")
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn voice_segmenter_buffers_pcm_and_drops_short_segments() {
    let raw = tempfile::tempdir().unwrap();
    let pipeline = SessionAudioPipeline::new().with_minimum_utterance_ms(1_000);
    let mut session = test_voice_session(raw.path());
    let pcm = pcm_ms(1_000, 200);
    let outcome = pipeline.handle_pcm_packet(Some(&mut session), "user-a", "Will", "will", &pcm);

    assert_eq!(outcome, AudioPipelineOutcome::Buffered);
    assert_eq!(session.participants["user-a"]["label"], "Will");
    assert!(session.last_pcm_at.is_some());
    assert_eq!(session.buffers["user-a"].pcm.len(), pcm.len());

    let outcome = pipeline.handle_empty_pcm_packet(Some(&mut session), "user-a", "Will", "will");
    assert_eq!(outcome, AudioPipelineOutcome::Buffered);
    assert!(session.buffers["user-a"].pcm.len() > pcm.len());
    assert_eq!(session.packet_debug["emptyPcmSilenceFrames"], 1);

    let outcome = pipeline
        .close_speaker_segment(&mut session, "user-a")
        .unwrap();
    assert!(matches!(
        outcome,
        AudioPipelineOutcome::SegmentTooShort { .. }
    ));
    assert!(session.buffers["user-a"].pcm.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn voice_segmenter_drops_low_energy_audio_before_stt_but_keeps_wake_probe_stream() {
    let raw = tempfile::tempdir().unwrap();
    let pipeline = SessionAudioPipeline::new().with_minimum_utterance_ms(1);
    let mut session = test_voice_session(raw.path());
    let ambient = pcm_ms(40, 600);
    let outcome =
        pipeline.handle_pcm_packet(Some(&mut session), "user-a", "Will", "will", &ambient);

    assert_eq!(outcome, AudioPipelineOutcome::Buffered);
    assert!(session.buffers["user-a"].pcm.is_empty());
    assert!(!session.buffers["user-a"].wake_pcm.is_empty());
    assert!(session.last_pcm_at.is_none());

    let wake = pipeline
        .capture_wake_probe(
            &mut session,
            "user-a",
            WakeProbeConfig {
                minimum_ms: 200,
                window_ms: 500,
                interval_ms: 1,
            },
            1.0,
            false,
        )
        .unwrap()
        .expect("wake probe payload");
    assert_eq!(wake.duration_ms, 500);
    assert!(wake.reset_stream);

    let outcome = pipeline
        .close_speaker_segment(&mut session, "user-a")
        .unwrap();
    assert_eq!(outcome, AudioPipelineOutcome::Ignored);
}

#[tokio::test(flavor = "current_thread")]
async fn low_energy_wake_only_audio_is_not_reported_as_live_stt_capture() {
    let raw = tempfile::tempdir().unwrap();
    let pipeline = SessionAudioPipeline::new().with_minimum_utterance_ms(1);
    let mut session = test_voice_session(raw.path());
    let ambient = pcm_ms(40, 600);
    let outcome =
        pipeline.handle_pcm_packet(Some(&mut session), "user-a", "Will", "will", &ambient);
    assert_eq!(outcome, AudioPipelineOutcome::Buffered);

    let metadata = session.metadata(chrono_tz::UTC);
    let speaker = metadata
        .capture_stats
        .speakers
        .get("user-a")
        .expect("speaker capture stats");
    assert!(!speaker.active);
    assert_eq!(speaker.buffered_audio_bytes, 0);
    assert!(speaker.segment_started_at.is_empty());
    assert!(speaker.last_pcm_at.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn deafened_voice_session_drops_packets_before_buffering() {
    let raw = tempfile::tempdir().unwrap();
    let pipeline = SessionAudioPipeline::new().with_minimum_utterance_ms(1);
    let mut session = test_voice_session(raw.path());
    session.mode = "deafened_paused".to_string();
    let pcm = vec![0_u8; PCM_20MS_SILENCE.len()];

    let outcome = pipeline.handle_pcm_packet(Some(&mut session), "user-a", "Will", "will", &pcm);
    assert_eq!(outcome, AudioPipelineOutcome::Paused);
    assert!(session.buffers.is_empty());
    assert!(session.last_pcm_at.is_none());
    assert_eq!(session.packet_debug["droppedPausedPcmPackets"], 1);

    let outcome =
        pipeline.handle_speaking_state(Some(&mut session), "user-a", "Will", "will", true);
    assert_eq!(outcome, AudioPipelineOutcome::Paused);
    assert!(session.participants.is_empty());
    assert_eq!(session.packet_debug["droppedPausedSpeakingStates"], 1);
}

#[tokio::test(flavor = "current_thread")]
async fn voice_segmenter_emits_ready_wav_artifact_job_payload() {
    let raw = tempfile::tempdir().unwrap();
    let pipeline = SessionAudioPipeline::new().with_minimum_utterance_ms(1);
    let mut session = test_voice_session(raw.path());
    let pcm = pcm_ms(1_000, 200);
    let outcome = pipeline.handle_pcm_packet(Some(&mut session), "user-a", "Will", "will", &pcm);
    assert_eq!(outcome, AudioPipelineOutcome::Buffered);

    let outcome = pipeline
        .close_speaker_segment(&mut session, "user-a")
        .unwrap();
    let AudioPipelineOutcome::SegmentReady { payload, segment } = outcome else {
        panic!("expected ready audio segment");
    };

    assert_eq!(payload.guild_id, "guild");
    assert_eq!(payload.voice_channel_id, "code");
    assert_eq!(payload.capture_run_id, "cap_test");
    assert_eq!(payload.speaker_user_id, "user-a");
    assert_eq!(payload.audio_format, "wav");
    assert_eq!(payload.sample_rate_hz, 48_000);
    assert_eq!(payload.channels, 2);
    assert!(payload.post_processing.contains("stt_gate=rms"));
    assert!(
        payload
            .post_processing
            .contains("stt_close_reason=manual_flush")
    );
    assert!(!payload.post_processing.contains("audio_rms=0.000000"));
    assert!(payload.source_audio_path.is_file());
    assert_eq!(
        payload.audio_bytes,
        payload.source_audio_path.metadata().unwrap().len()
    );
    assert_eq!(segment.segment_index, 0);
    assert_eq!(segment.wav_path, payload.source_audio_path);
    assert_eq!(segment.audio_checksum, payload.audio_checksum);
    assert!(payload.audio_checksum.starts_with("sha256:"));
}

#[tokio::test(flavor = "current_thread")]
async fn voice_segmenter_preserves_preroll_after_initial_ambient_noise() {
    let raw = tempfile::tempdir().unwrap();
    let pipeline = SessionAudioPipeline::new().with_minimum_utterance_ms(1);
    let mut session = test_voice_session(raw.path());
    let ambient = pcm_ms(40, 200);
    let speech = pcm_ms(1_000, 100);

    let outcome =
        pipeline.handle_pcm_packet(Some(&mut session), "user-a", "Will", "will", &ambient);
    assert_eq!(outcome, AudioPipelineOutcome::Buffered);
    assert!(session.buffers["user-a"].pcm.is_empty());

    let outcome = pipeline.handle_pcm_packet(Some(&mut session), "user-a", "Will", "will", &speech);
    assert_eq!(outcome, AudioPipelineOutcome::Buffered);
    assert!(session.buffers["user-a"].pcm.len() > speech.len());

    let outcome = pipeline
        .close_speaker_segment(&mut session, "user-a")
        .unwrap();
    let AudioPipelineOutcome::SegmentReady { payload, segment } = outcome else {
        panic!("expected ready audio segment");
    };

    assert_eq!(payload.duration_ms, 220);
    assert_eq!(
        (payload.segment_end_time - payload.segment_start_time).num_milliseconds(),
        220
    );
    assert_eq!(segment.started_at, payload.segment_start_time);
    assert_eq!(segment.ended_at, payload.segment_end_time);
    assert!(payload.post_processing.contains("stt_dropped_ms=80"));
    assert!(payload.post_processing.contains("stt_preroll_ms=200"));
}

#[tokio::test(flavor = "current_thread")]
async fn voice_segmenter_does_not_attach_stale_ambient_preroll_after_input_gap() {
    let raw = tempfile::tempdir().unwrap();
    let pipeline = SessionAudioPipeline::new().with_minimum_utterance_ms(1);
    let mut session = test_voice_session(raw.path());
    let ambient = pcm_ms(40, 200);
    let speech = pcm_ms(1_000, 100);

    let outcome =
        pipeline.handle_pcm_packet(Some(&mut session), "user-a", "Will", "will", &ambient);
    assert_eq!(outcome, AudioPipelineOutcome::Buffered);
    assert!(session.buffers["user-a"].pcm.is_empty());
    session.buffers.get_mut("user-a").unwrap().last_input_at =
        Some(chrono::Utc::now() - chrono::Duration::seconds(10));

    let outcome = pipeline.handle_pcm_packet(Some(&mut session), "user-a", "Will", "will", &speech);
    assert_eq!(outcome, AudioPipelineOutcome::Buffered);

    let outcome = pipeline
        .close_speaker_segment(&mut session, "user-a")
        .unwrap();
    let AudioPipelineOutcome::SegmentReady { payload, segment } = outcome else {
        panic!("expected ready audio segment");
    };

    assert_eq!(payload.duration_ms, 100);
    assert_eq!(
        (payload.segment_end_time - payload.segment_start_time).num_milliseconds(),
        100
    );
    assert_eq!(segment.started_at, payload.segment_start_time);
    assert_eq!(segment.ended_at, payload.segment_end_time);
    assert!(payload.post_processing.contains("stt_dropped_ms=0"));
    assert!(payload.post_processing.contains("stt_preroll_ms=80"));
}

#[tokio::test(flavor = "current_thread")]
async fn voice_segmenter_keeps_short_internal_pause_inside_segment() {
    let raw = tempfile::tempdir().unwrap();
    let pipeline = SessionAudioPipeline::new().with_minimum_utterance_ms(1);
    let mut session = test_voice_session(raw.path());
    let first = pcm_ms(1_000, 120);
    let second = pcm_ms(1_000, 100);

    let outcome = pipeline.handle_pcm_packet(Some(&mut session), "user-a", "Will", "will", &first);
    assert_eq!(outcome, AudioPipelineOutcome::Buffered);

    let silence = pcm_ms(0, 400);
    let outcome =
        pipeline.handle_silence_packet(Some(&mut session), "user-a", "Will", "will", &silence);
    assert_eq!(outcome, AudioPipelineOutcome::Buffered);
    assert_eq!(
        pipeline.should_flush_speaker(&session.buffers["user-a"], 15_000, 2_500, 0.0),
        None
    );

    let outcome = pipeline.handle_pcm_packet(Some(&mut session), "user-a", "Will", "will", &second);
    assert_eq!(outcome, AudioPipelineOutcome::Buffered);
    let last_pcm_at = session.buffers["user-a"].last_pcm_at.unwrap();

    let outcome = pipeline
        .close_speaker_segment(&mut session, "user-a")
        .unwrap();
    let AudioPipelineOutcome::SegmentReady { payload, segment } = outcome else {
        panic!("expected ready audio segment");
    };

    assert_eq!(payload.segment_end_time, last_pcm_at);
    assert_eq!(segment.ended_at, last_pcm_at);
    assert_eq!(payload.duration_ms, 620);
    assert!(payload.post_processing.contains("stt_soft_break_ms=400"));
    assert!(
        payload
            .post_processing
            .contains("stt_trimmed_trailing_ms=0")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn voice_segmenter_flushes_after_long_silence_and_trims_hangover() {
    let raw = tempfile::tempdir().unwrap();
    let pipeline = SessionAudioPipeline::new().with_minimum_utterance_ms(1);
    let mut session = test_voice_session(raw.path());
    let pcm = pcm_ms(1_000, 120);

    let outcome = pipeline.handle_pcm_packet(Some(&mut session), "user-a", "Will", "will", &pcm);
    assert_eq!(outcome, AudioPipelineOutcome::Buffered);
    let last_pcm_at = session.buffers["user-a"].last_pcm_at.unwrap();

    let silence = pcm_ms(0, 1400);
    let outcome =
        pipeline.handle_silence_packet(Some(&mut session), "user-a", "Will", "will", &silence);
    assert_eq!(outcome, AudioPipelineOutcome::Buffered);
    assert_eq!(session.buffers["user-a"].last_pcm_at, Some(last_pcm_at));
    assert_eq!(
        pipeline.should_flush_speaker(&session.buffers["user-a"], 15_000, 2_500, 0.0),
        Some(SegmentCloseReason::EndSilence)
    );

    let outcome = pipeline
        .close_speaker_segment_with_reason(&mut session, "user-a", SegmentCloseReason::EndSilence)
        .unwrap();
    let AudioPipelineOutcome::SegmentReady { payload, segment } = outcome else {
        panic!("expected ready audio segment");
    };

    assert_eq!(payload.segment_end_time, last_pcm_at);
    assert_eq!(segment.ended_at, last_pcm_at);
    assert_eq!(payload.duration_ms, 120);
    assert!(
        payload
            .post_processing
            .contains("stt_close_reason=end_silence")
    );
    assert!(
        payload
            .post_processing
            .contains("stt_trimmed_trailing_ms=1400")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn voice_segmenter_emits_ordered_streaming_wake_probe_chunks_without_closing_segment() {
    let raw = tempfile::tempdir().unwrap();
    let pipeline = SessionAudioPipeline::new().with_minimum_utterance_ms(1);
    let mut session = test_voice_session(raw.path());
    let pcm = pcm_ms(0, 1000);
    let outcome = pipeline.handle_pcm_packet(Some(&mut session), "user-a", "Will", "will", &pcm);
    assert_eq!(outcome, AudioPipelineOutcome::Buffered);

    let payload = pipeline
        .capture_wake_probe(
            &mut session,
            "user-a",
            WakeProbeConfig {
                minimum_ms: 200,
                window_ms: 500,
                interval_ms: 1,
            },
            1.0,
            false,
        )
        .unwrap()
        .expect("wake probe payload");

    assert_eq!(payload.guild_id, "guild");
    assert_eq!(payload.voice_channel_id, "code");
    assert_eq!(payload.capture_run_id, "cap_test");
    assert_eq!(payload.speaker_user_id, "user-a");
    assert_eq!(payload.duration_ms, 500);
    assert_eq!(payload.stream_id, "guild:code:cap_test:user-a");
    assert!(payload.reset_stream);
    assert_eq!(payload.sample_rate_hz, 16_000);
    assert_eq!(payload.channels, 1);
    assert!(payload.audio_bytes < 20_000);
    assert!(payload.source_audio_path.is_file());
    assert!(payload.audio_checksum.starts_with("sha256:"));
    assert!(session.buffers["user-a"].pcm.is_empty());
    assert!(session.buffers["user-a"].wake_pcm.is_empty());
    assert_eq!(session.buffers["user-a"].wake_probe_counter, 1);

    let next_pcm = pcm_ms(0, 500);
    let outcome =
        pipeline.handle_pcm_packet(Some(&mut session), "user-a", "Will", "will", &next_pcm);
    assert_eq!(outcome, AudioPipelineOutcome::Buffered);

    let second = pipeline
        .capture_wake_probe(
            &mut session,
            "user-a",
            WakeProbeConfig {
                minimum_ms: 200,
                window_ms: 500,
                interval_ms: 1,
            },
            2.0,
            false,
        )
        .unwrap()
        .expect("second wake probe payload");
    assert_eq!(second.probe_index, 1);
    assert!(!second.reset_stream);
    assert_eq!(second.duration_ms, 500);
    assert!(second.probe_start_time > payload.probe_start_time);
}
