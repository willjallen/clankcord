use super::*;
use crate::adapters::discord::voice::diagnostics::default_packet_debug;
use crate::runtime::RoomConfig;
use std::path::Path;

fn session(root: &Path) -> LiveCaptureSession {
    LiveCaptureSession::new(
        LiveVoiceSession {
            session_id: "handoff-test".into(),
            room: RoomConfig {
                room_id: "room".into(),
                guild_id: "guild".into(),
                guild_slug: "guild".into(),
                channel_id: "channel".into(),
                channel_slug: "channel".into(),
                channel_name: "Channel".into(),
                auto_join: false,
            },
            bot_id: "bot".into(),
            bot_user_id: "bot-user".into(),
            thread_id: String::new(),
            thread_name: String::new(),
            started_at: Utc::now(),
            session_dir: root.join("session"),
            minute_message_ids: Default::default(),
            participants: Default::default(),
            buffers: Default::default(),
            packet_debug: default_packet_debug(),
            debug_notes: Default::default(),
            segment_counter: 0,
            audio_segments: Vec::new(),
            transcription_task_ids: Default::default(),
            finalizing: false,
            ended_at: None,
            voice_channel_id: "channel".into(),
            transcript_event_count: 0,
            last_pcm_at: None,
            last_transcript_at: None,
            last_pcm_monotonic: 0.0,
            last_transcript_monotonic: 0.0,
            last_stall_log_monotonic: 0.0,
            voice_client_debug: Default::default(),
            capture_run_id: "handoff-test".into(),
            assignment_id: String::new(),
            mode: "local_buffering".into(),
        },
        350,
        WakeProbeConfig {
            minimum_ms: 0,
            window_ms: 0,
            interval_ms: 0,
        },
        SpeechGateConfig::conservative(),
    )
}

fn blake() -> CaptureUser {
    CaptureUser {
        id: "blake".into(),
        display_name: "Blake".into(),
        global_name: String::new(),
        name: "blake".into(),
    }
}

fn packet() -> VoiceData {
    VoiceData {
        user: None,
        pcm: [1000_i16.to_le_bytes(); 1920].concat(),
        has_packet: true,
        is_silence: false,
    }
}

fn record(session: &mut LiveCaptureSession, ssrc: u32, silent: Vec<u32>) {
    for _ in 0..30 {
        session.write_voice_tick(vec![(ssrc, packet())], silent.clone());
    }
}

fn assert_recorded(session: &mut LiveCaptureSession, expected_ms: i64) {
    let AudioPipelineOutcome::SegmentReady { payload, .. } = session
        .pipeline
        .close_speaker_segment(&mut session.session, "blake")
        .unwrap()
    else {
        panic!("replacement-device speech must produce a WAV and transcription payload");
    };
    assert_eq!(payload.speaker_user_id, "blake");
    assert_eq!(payload.duration_ms, expected_ms);
    assert!(payload.source_audio_path.is_file());
    let mut wav = hound::WavReader::open(&payload.source_audio_path).unwrap();
    assert!(wav.samples::<i16>().any(|sample| sample.unwrap() != 0));
    assert_eq!(session.session.packet_debug["missingUserPackets"], 0);
}

#[test]
fn voice_handoff_late_phone_disconnect_keeps_pc_audio() {
    let root = tempfile::tempdir().unwrap();
    let mut session = session(root.path());
    session.note_speaking_state(100, blake(), true);
    session.note_speaking_state(200, blake(), true);
    session.note_client_disconnect("blake");
    record(&mut session, 200, vec![]);
    assert_recorded(&mut session, 600);
}

#[test]
fn voice_handoff_disconnect_then_pc_speaking_keeps_audio() {
    let root = tempfile::tempdir().unwrap();
    let mut session = session(root.path());
    session.note_speaking_state(100, blake(), true);
    session.note_client_disconnect("blake");
    session.note_speaking_state(200, blake(), true);
    record(&mut session, 200, vec![]);
    assert_recorded(&mut session, 600);
}

#[test]
fn voice_handoff_old_stream_silence_does_not_extend_pc_audio() {
    let root = tempfile::tempdir().unwrap();
    let mut session = session(root.path());
    session.note_speaking_state(100, blake(), true);
    session.note_speaking_state(200, blake(), true);
    record(&mut session, 200, vec![100]);
    assert_recorded(&mut session, 600);
}

#[test]
fn voice_disconnect_preserves_inflight_audio_until_packet_timeout() {
    let root = tempfile::tempdir().unwrap();
    let mut session = session(root.path());
    session.note_speaking_state(100, blake(), true);
    record(&mut session, 100, vec![]);
    session.note_client_disconnect("blake");
    assert!(!session.session.buffers["blake"].pcm.is_empty());
    // A real departure stops sending packets and follows the ordinary idle flush.
    session
        .session
        .buffers
        .get_mut("blake")
        .unwrap()
        .last_packet_monotonic = -10.0;
    let jobs = session.flush_ready_buffers(15_000, 2_500);
    assert_eq!(jobs.len(), 1);
    assert!(session.session.buffers["blake"].pcm.is_empty());
    assert!(!session.session.buffers["blake"].active);
    assert_eq!(session.session.audio_segments.len(), 1);
}

#[test]
fn voice_handoff_disconnect_mid_utterance_keeps_both_halves() {
    let root = tempfile::tempdir().unwrap();
    let mut session = session(root.path());
    session.note_speaking_state(200, blake(), true);
    for _ in 0..10 {
        session.write_voice_tick(vec![(200, packet())], vec![]);
    }
    session.note_client_disconnect("blake");
    for _ in 0..20 {
        session.write_voice_tick(vec![(200, packet())], vec![]);
    }
    assert_recorded(&mut session, 600);
}

#[test]
fn voice_handoff_duplicate_silence_advances_speaker_once() {
    let root = tempfile::tempdir().unwrap();
    let mut session = session(root.path());
    session.note_speaking_state(100, blake(), true);
    session.note_speaking_state(200, blake(), true);
    record(&mut session, 200, vec![]);
    let before = session.session.buffers["blake"].pcm.len();
    session.write_voice_tick(vec![], vec![100, 200]);
    assert_eq!(
        session.session.buffers["blake"].pcm.len(),
        before + PCM_20MS_SILENCE.len()
    );
}

#[test]
fn voice_handoff_concealment_does_not_suppress_real_silence() {
    let root = tempfile::tempdir().unwrap();
    let mut session = session(root.path());
    session.note_speaking_state(100, blake(), true);
    session.note_speaking_state(200, blake(), true);
    record(&mut session, 200, vec![]);
    let before = session.session.buffers["blake"].pcm.len();
    let mut synthetic = packet();
    synthetic.has_packet = false;
    session.write_voice_tick(vec![(100, synthetic)], vec![200]);
    assert_eq!(
        session.session.buffers["blake"].pcm.len(),
        before + PCM_20MS_SILENCE.len()
    );
}

#[test]
fn voice_handoff_ssrc_reassignment_uses_new_user() {
    let root = tempfile::tempdir().unwrap();
    let mut session = session(root.path());
    session.note_speaking_state(100, blake(), true);
    let mut other = blake();
    other.id = "vince".into();
    other.display_name = "Vince".into();
    other.name = "vince".into();
    session.note_speaking_state(100, other, true);
    session.note_client_disconnect("blake");
    record(&mut session, 100, vec![]);
    assert!(!session.session.buffers.contains_key("blake"));
    assert_eq!(
        session.session.buffers["vince"].pcm.len(),
        30 * PCM_20MS_SILENCE.len()
    );
}

#[test]
fn voice_handoff_other_users_audio_does_not_suppress_blakes_silence() {
    let root = tempfile::tempdir().unwrap();
    let mut session = session(root.path());
    session.note_speaking_state(100, blake(), true);
    let mut other = blake();
    other.id = "vince".into();
    session.note_speaking_state(200, other, true);
    record(&mut session, 100, vec![]);
    let before = session.session.buffers["blake"].pcm.len();
    session.write_voice_tick(vec![(200, packet())], vec![100]);
    assert_eq!(
        session.session.buffers["blake"].pcm.len(),
        before + PCM_20MS_SILENCE.len()
    );
}
