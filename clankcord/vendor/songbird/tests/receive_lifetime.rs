use super::*;
use crate::model::id::UserId;
use discortp::rtp::MutableRtpPacket;

// Exercise real transport decryption and decoder expiry with no Discord connection.
#[tokio::test(start_paused = true)]
async fn voice_handoff_idle_decoder_expires_and_resumes_with_existing_identity() {
    let mode = CryptoMode::Aes256Gcm;
    let cipher = mode.cipher_from_key(&[7; 32]).unwrap();
    let (_tx, rx) = flume::unbounded();
    let tracker = Arc::new(SsrcTracker::default());
    tracker.ssrc_user_map.insert(123, UserId(42));
    let mut receiver = UdpRx {
        cipher,
        crypto_mode: mode,
        decoder_map: HashMap::new(),
        config: Config::default()
            .decode_mode(DecodeMode::Decode(Default::default()))
            .decode_state_timeout(Duration::from_secs(60)),
        rx,
        ssrc_signalling: tracker.clone(),
        udp_socket: UdpSocket::bind("127.0.0.1:0").await.unwrap(),
        dave_session: Arc::new(RwLock::new(None)),
        dave_protocol_version: Arc::new(AtomicU16::new(0)),
    };
    let (core, _core_rx) = flume::unbounded();
    let (events, _events_rx) = flume::unbounded();
    let (mixer, _mixer_rx) = flume::unbounded();
    let interconnect = Interconnect {
        core,
        events,
        mixer,
    };
    let packet = |sequence: u16| {
        let mut bytes = BytesMut::zeroed(12 + SILENT_FRAME.len() + mode.payload_overhead());
        let mut rtp = MutableRtpPacket::new(&mut bytes).unwrap();
        rtp.set_version(RTP_VERSION);
        rtp.set_payload_type(RTP_PROFILE_TYPE);
        rtp.set_ssrc(123);
        rtp.set_sequence(sequence.into());
        rtp.set_timestamp((u32::from(sequence) * 960).into());
        let len = rtp.payload().len();
        rtp.payload_mut()[..SILENT_FRAME.len()].copy_from_slice(&SILENT_FRAME);
        rtp.payload_mut()[len - 4..].copy_from_slice(&u32::from(sequence).to_be_bytes());
        mode.cipher_from_key(&[7; 32])
            .unwrap()
            .encrypt_pkt_in_place(&mut rtp, len)
            .unwrap();
        bytes
    };
    receiver.process_udp_message(&interconnect, packet(1)).await;
    assert!(receiver.decoder_map.contains_key(&123));
    tokio::time::advance(Duration::from_secs(59)).await;
    receiver.process_udp_message(&interconnect, packet(2)).await;
    tokio::time::advance(Duration::from_secs(2)).await;
    receiver.prune_decoders(Instant::now());
    assert!(
        receiver.decoder_map.contains_key(&123),
        "live packets extend decoder lifetime"
    );
    tokio::time::advance(Duration::from_secs(60)).await;
    receiver.prune_decoders(Instant::now());
    assert!(
        receiver.decoder_map.is_empty(),
        "idle decoders are still reclaimed"
    );
    assert_eq!(
        tracker.ssrc_user_map.get(&123).map(|v| *v),
        Some(UserId(42))
    );
    receiver.process_udp_message(&interconnect, packet(3)).await;
    assert!(
        receiver.decoder_map.contains_key(&123),
        "audio resumes without another Speaking event"
    );
}
