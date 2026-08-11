//! Pins for speech gating: low-energy audio never reaches STT, preroll
//! attaches only to live speech, and deafen stops capture (3383d63, e59db3b).

use crate::support::initialize_test_config;
use crate::support::rooms::{room_runtime, test_room};
use crate::support::test_store;
use crate::support::test_voice_session;
use crate::support::voice::pcm_ms;
use clankcord::adapters::discord::voice::artifacts::PCM_20MS_SILENCE;
use clankcord::adapters::discord::voice::session::AudioPipelineOutcome;
use clankcord::adapters::discord::voice::session::SegmentCloseReason;
use clankcord::adapters::discord::voice::session::SessionAudioPipeline;
use clankcord::adapters::discord::voice::session::WakeProbeConfig;
use clankcord::model::job::CommandKind;
use clankcord::model::job::CommandRequest;
use clankcord::model::job::Job;
use clankcord::model::job::JobKind;
use clankcord::model::rooms::RoomConfig;
use clankcord::model::scope::RuntimeScope;
use clankcord::model::voice::VoiceCaptureSessionStatus;
use serde_json::json;

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
async fn deafen_and_undeafen_commands_create_discord_deafen_jobs() {
    let raw = tempfile::tempdir().unwrap();
    initialize_test_config(raw.path());
    let store = test_store(raw.path()).await;
    let room = test_room();
    let runtime = room_runtime(store.clone(), room.clone());
    store
        .upsert_capture_session_status(&VoiceCaptureSessionStatus {
            session_id: "cap_1".to_string(),
            guild_id: room.guild_id.clone(),
            voice_channel_id: room.channel_id.clone(),
            bot_id: "clanky-vc1".to_string(),
            active: true,
            started_at: "2026-05-15T00:00:00.000Z".to_string(),
            mode: "local_buffering".to_string(),
            ..VoiceCaptureSessionStatus::default()
        })
        .await
        .unwrap();

    let deafen = command_job(&room, CommandKind::DeafenListening);
    let deafen_id = deafen.id.clone();
    let deafen = store.create_job(deafen).await.unwrap();
    clankcord::engine::dispatcher::dispatch_claimed_runtime_job(
        &runtime,
        &clankcord::ports::discord::DiscordApiUnavailable,
        deafen,
    )
    .await
    .unwrap();

    let deafen_jobs = store
        .list_jobs_by_scope_kind(
            &room.guild_id,
            &room.channel_id,
            JobKind::DiscordVoiceDeafen,
        )
        .await
        .unwrap();
    assert_eq!(deafen_jobs.len(), 1);
    let deafen_payload = deafen_jobs[0].discord_voice_deafen_payload().unwrap();
    assert_eq!(deafen_payload.session_id, "cap_1");
    assert!(deafen_payload.deafened);
    assert_eq!(deafen_payload.source_job_id, deafen_id);

    let placement_jobs = store
        .list_jobs_by_scope_kind(
            &room.guild_id,
            &room.channel_id,
            JobKind::RoomAgentPlacement,
        )
        .await
        .unwrap();
    assert!(placement_jobs.is_empty());

    let undeafen = command_job(&room, CommandKind::ResumeListening);
    let undeafen = store.create_job(undeafen).await.unwrap();
    clankcord::engine::dispatcher::dispatch_claimed_runtime_job(
        &runtime,
        &clankcord::ports::discord::DiscordApiUnavailable,
        undeafen,
    )
    .await
    .unwrap();

    let deafen_jobs = store
        .list_jobs_by_scope_kind(
            &room.guild_id,
            &room.channel_id,
            JobKind::DiscordVoiceDeafen,
        )
        .await
        .unwrap();
    assert_eq!(deafen_jobs.len(), 2);
    let undeafen_payload = deafen_jobs
        .iter()
        .find_map(|job| {
            let payload = job.discord_voice_deafen_payload()?;
            (!payload.deafened).then_some(payload)
        })
        .expect("undeafen job");
    assert_eq!(undeafen_payload.session_id, "cap_1");
}

fn command_job(room: &RoomConfig, command_kind: CommandKind) -> Job {
    Job::command_request(
        RuntimeScope::voice_channel(&room.guild_id, &room.channel_id),
        "user-a",
        CommandRequest::from_json(&json!({
            "action": "dispatch_now",
            "command_kind": command_kind.as_str(),
            "guild_id": room.guild_id,
            "scope_id": room.channel_id,
            "requested_by_user_id": "user-a",
            "arguments": {
                "channel": "",
                "target_channel": "",
            },
        }))
        .unwrap(),
    )
}
