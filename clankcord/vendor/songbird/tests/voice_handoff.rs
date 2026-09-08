use super::*;
use crate::{driver::CryptoMode, model::payload::ClientDisconnect, Config};
use tokio::net::{TcpListener, UdpSocket};

// Drive the actual websocket event handler and UDP cleanup loop together. No Discord
// account, external service, or real-time five-second sleep is involved.
async fn handoff_survives_cleanup(disconnect_first: bool, replacement_ssrc: u32) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap())
        .parse()
        .unwrap();
    let (client, server) = tokio::join!(WsStream::connect(url), async {
        let (stream, _) = listener.accept().await.unwrap();
        tokio_tungstenite::accept_async(stream).await.unwrap()
    });
    let _server = server;
    let (_ws_tx, ws_rx) = flume::unbounded();
    let (_udp_tx, udp_rx) = flume::unbounded();
    let (core, _core_rx) = flume::unbounded();
    let (events, event_rx) = flume::unbounded();
    let (mixer, _mixer_rx) = flume::unbounded();
    let interconnect = Interconnect {
        core,
        events,
        mixer,
    };
    let tracker = Arc::new(SsrcTracker::default());
    let dave = Arc::new(RwLock::new(None));
    let protocol = Arc::new(AtomicU16::new(0));
    let mut network = AuxNetwork::new(
        ws_rx,
        client.unwrap(),
        999,
        30_000.0,
        0,
        ConnectionInfo {
            channel_id: std::num::NonZeroU64::new(1).unwrap().into(),
            endpoint: String::new(),
            guild_id: std::num::NonZeroU64::new(2).unwrap().into(),
            session_id: "receiver-session".into(),
            token: String::new(),
            user_id: std::num::NonZeroU64::new(3).unwrap().into(),
        },
        dave.clone(),
        protocol.clone(),
        tracker.clone(),
    );
    let user = UserId(42);
    let speaking = |ssrc| {
        GatewayEvent::Speaking(Speaking {
            delay: Some(0),
            speaking: SpeakingState::MICROPHONE,
            ssrc,
            user_id: Some(user),
        })
    };
    let disconnect = || GatewayEvent::ClientDisconnect(ClientDisconnect { user_id: user });
    network
        .process_ws(&interconnect, speaking(100))
        .await
        .unwrap();
    let transitions = if disconnect_first {
        [disconnect(), speaking(replacement_ssrc)]
    } else {
        [speaking(replacement_ssrc), disconnect()]
    };
    for event in transitions {
        network.process_ws(&interconnect, event).await.unwrap();
    }
    assert!(event_rx.try_iter().any(|event| matches!(
        event,
        EventMessage::FireCoreEvent(CoreContext::ClientDisconnect(_))
    )));

    tokio::time::pause();
    let mode = CryptoMode::Aes256Gcm;
    let receiver = tokio::spawn(crate::driver::tasks::udp_rx::runner(
        interconnect,
        udp_rx,
        mode.cipher_from_key(&[0; 32]).unwrap(),
        mode,
        Config::default(),
        UdpSocket::bind("127.0.0.1:0").await.unwrap(),
        tracker.clone(),
        dave,
        protocol,
    ));
    // Cross several cleanup sweeps, including when the new device is initially quiet.
    for _ in 0..3 {
        tokio::time::sleep(Duration::from_secs(6)).await;
        assert_eq!(
            tracker.ssrc_user_map.get(&replacement_ssrc).map(|v| *v),
            Some(user),
            "a departing device must not erase the replacement device's DAVE identity"
        );
    }
    receiver.abort();
    let _ = receiver.await;
}

#[tokio::test]
async fn voice_handoff_disconnect_before_new_speaking_survives_cleanup() {
    handoff_survives_cleanup(true, 200).await;
}

#[tokio::test]
async fn voice_handoff_late_disconnect_survives_cleanup() {
    handoff_survives_cleanup(false, 200).await;
}

#[tokio::test]
async fn voice_handoff_same_ssrc_reconnect_survives_cleanup() {
    handoff_survives_cleanup(true, 100).await;
}
