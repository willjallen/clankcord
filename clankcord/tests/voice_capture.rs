mod common;

use clankcord::adapters::discord::voice::artifacts::PCM_20MS_SILENCE;
use clankcord::adapters::discord::voice::capture::{
    CaptureAction, CaptureUser, VoiceCaptureSink, VoiceData,
};
use clankcord::adapters::discord::voice::session::{AudioPipelineOutcome, SessionAudioPipeline};

use common::test_voice_session;

#[test]
fn capture_sink_translates_voice_packets_to_pipeline_actions() {
    let user = CaptureUser {
        id: "user-a".to_string(),
        display_name: "Will".to_string(),
        global_name: String::new(),
        name: "will".to_string(),
    };
    let mut sink = VoiceCaptureSink::new("session-1");
    assert!(!sink.wants_opus());
    assert_eq!(
        sink.on_voice_member_speaking_start(&user),
        CaptureAction::SpeakingState {
            session_id: "session-1".to_string(),
            user_id: "user-a".to_string(),
            label: "Will".to_string(),
            username: "will".to_string(),
            active: true
        }
    );

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

#[test]
fn voice_segmenter_buffers_pcm_and_drops_short_segments() {
    let raw = tempfile::tempdir().unwrap();
    let pipeline = SessionAudioPipeline::new().with_minimum_utterance_ms(1_000);
    let mut session = test_voice_session(raw.path());
    let outcome =
        pipeline.handle_pcm_packet(Some(&mut session), "user-a", "Will", "will", &[0; 480]);

    assert_eq!(outcome, AudioPipelineOutcome::Buffered);
    assert_eq!(session.participants["user-a"]["label"], "Will");
    assert!(session.last_pcm_at.is_some());
    assert_eq!(session.buffers["user-a"].pcm.len(), 480);

    let outcome = pipeline.handle_empty_pcm_packet(Some(&mut session), "user-a", "Will", "will");
    assert_eq!(outcome, AudioPipelineOutcome::Buffered);
    assert!(session.buffers["user-a"].pcm.len() > 480);
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

#[test]
fn voice_segmenter_emits_ready_wav_artifact_job_payload() {
    let raw = tempfile::tempdir().unwrap();
    let pipeline = SessionAudioPipeline::new().with_minimum_utterance_ms(1);
    let mut session = test_voice_session(raw.path());
    let pcm = vec![0_u8; PCM_20MS_SILENCE.len()];
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
    assert!(payload.source_audio_path.is_file());
    assert_eq!(
        payload.audio_bytes,
        payload.source_audio_path.metadata().unwrap().len()
    );
    assert_eq!(segment.segment_index, 0);
    assert_eq!(segment.wav_path, payload.source_audio_path);
    assert_eq!(segment.audio_checksum, payload.audio_checksum);
}
