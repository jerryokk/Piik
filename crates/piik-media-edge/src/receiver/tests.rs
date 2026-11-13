// Relay derivation. These are native-codec acceptance gates that need
// piik-capture.exe and encoded fixtures absent from testdata.

use std::path::PathBuf;
use std::time::Duration;

use piik_native_capture::{Adapter, Capabilities, Encoder, VideoProfile};

use crate::relay::relay_backend;
use crate::tests::{Peer, VP8_KEY_8X8, loopback_engine};
use crate::{RelayOptions, VideoSource as Source};

fn options(capabilities: Capabilities) -> RelayOptions {
    RelayOptions {
        capture_process: PathBuf::from("piik-capture.exe"),
        capabilities,
    }
}

fn adapter(index: u32, hardware_h264: Vec<u32>) -> Adapter {
    Adapter {
        index,
        name: format!("adapter-{index}"),
        identity: String::new(),
        hardware_h264: hardware_h264
            .into_iter()
            .map(|index| Encoder {
                index,
                name: String::new(),
                identity: String::new(),
            })
            .collect(),
    }
}

#[test]
fn relay_backend_selects_the_first_capable_adapter() {
    use piik_media_transport::sdp::VideoCodec;
    assert!(
        relay_backend(VideoCodec::Vp8, None).is_none(),
        "no relay options"
    );
    let empty = RelayOptions::default();
    assert!(
        relay_backend(VideoCodec::Vp8, Some(&empty)).is_none(),
        "no capture process"
    );
    let vp8 = options(Capabilities {
        software_vp8: true,
        adapters: vec![adapter(3, Vec::new())],
        ..Capabilities::default()
    });
    let backend = relay_backend(VideoCodec::Vp8, Some(&vp8)).expect("software VP8");
    assert_eq!((backend.codec.as_str(), backend.adapter_index), ("vp8", 3));
    assert!(
        relay_backend(VideoCodec::H264, Some(&vp8)).is_none(),
        "no hardware H.264"
    );
    let h264 = options(Capabilities {
        adapters: vec![adapter(0, Vec::new()), adapter(1, vec![7, 8])],
        ..Capabilities::default()
    });
    let backend = relay_backend(VideoCodec::H264, Some(&h264)).expect("hardware H.264");
    assert_eq!((backend.adapter_index, backend.encoder_index), (1, 7));
    let none = options(Capabilities {
        software_vp8: true,
        ..Capabilities::default()
    });
    assert!(
        relay_backend(VideoCodec::Vp8, Some(&none)).is_none(),
        "no adapter"
    );
}

#[tokio::test]
async fn receiver_with_relay_backend_declares_two_layers() {
    let host = loopback_engine();
    let viewer = loopback_engine();
    let source = host.new_source("vp8", 1, 1, None).await.unwrap();
    let edge = host
        .new_edge(
            &source,
            crate::EdgeOptions {
                connection_id: "relay".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let offer = edge.create_offer().await.unwrap();
    let relay = options(Capabilities {
        software_vp8: true,
        adapters: vec![adapter(0, Vec::new())],
        ..Capabilities::default()
    });
    let (receiver, _) = viewer
        .new_receiver(crate::ReceiverOptions {
            offer,
            edge_capacity: 2,
            relay: Some(relay),
            ..Default::default()
        })
        .await
        .unwrap();
    let key = receiver.source().key;
    let (layers, relay) = viewer
        .call(move |app, _, _| {
            let state = &app.sources[&key];
            (state.format_count, state.relay.is_some())
        })
        .await
        .unwrap();
    assert_eq!(
        (layers, relay),
        (2, true),
        "a capable relay derives one lower layer"
    );
    // Recording a profile never starts the process by itself.
    receiver
        .source()
        .set_relay_profile(VideoProfile {
            width: 1280,
            height: 720,
            framerate: 30,
            bitrate: 2_000_000,
            preference: "balanced".into(),
        })
        .await
        .unwrap();
    let running = viewer
        .call(move |app, _, _| app.sources[&key].relay_running())
        .await
        .unwrap();
    assert!(!running, "profile alone started a decoder");
    assert!(
        receiver
            .source()
            .set_relay_profile(VideoProfile {
                width: 1,
                ..VideoProfile::default()
            })
            .await
            .is_err()
    );
    viewer.close().await.unwrap();
    host.close().await.unwrap();
}

/// Raw forwarding stays healthy while one shared capture-process decoder
/// derives the lower layer for a weak child.
#[tokio::test]
#[ignore = "requires PIIK_NATIVE_CAPTURE (the real piik-capture.exe); its encoded fixture also needs PIIK_ENCODED_FIXTURE, absent from testdata"]
async fn relay_derivation_with_the_capture_process() {
    let executable = std::env::var("PIIK_NATIVE_CAPTURE").expect("set PIIK_NATIVE_CAPTURE");
    let host = loopback_engine();
    let viewer = loopback_engine();
    let original = host.new_source("vp8", 1, 1, None).await.unwrap();
    original.set_format(0, 640, 360).await.unwrap();
    original.configure_outputs(vec![2_000_000]).await.unwrap();
    let upstream = {
        let relay = RelayOptions {
            capture_process: executable.into(),
            capabilities: Capabilities {
                software_vp8: true,
                adapters: vec![adapter(0, Vec::new())],
                ..Capabilities::default()
            },
        };
        let mut edge_events = crate::tests::events();
        let edge = host
            .new_edge(
                &original,
                crate::EdgeOptions {
                    connection_id: "up".into(),
                    events: Some(edge_events.sink.clone()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let offer = edge.create_offer().await.unwrap();
        let mut receiver_events = crate::tests::events();
        let (receiver, answer) = viewer
            .new_receiver(crate::ReceiverOptions {
                offer,
                edge_capacity: 2,
                relay: Some(relay),
                events: Some(receiver_events.sink.clone()),
                ..Default::default()
            })
            .await
            .unwrap();
        edge.set_answer(&answer, "").await.unwrap();
        crate::tests::exchange(&edge, &mut edge_events, &receiver, &mut receiver_events).await;
        receiver
    };
    let relay: Source = upstream.source().clone();
    relay
        .set_relay_profile(VideoProfile {
            width: 1280,
            height: 720,
            framerate: 30,
            bitrate: 2_000_000,
            preference: "balanced".into(),
        })
        .await
        .unwrap();
    let healthy = Peer::connect(&viewer, &relay, "relay-healthy").await;
    let weak = Peer::connect(&viewer, &relay, "relay-weak").await;
    weak.edge.set_target_layer(0).await.unwrap();
    let key = relay.key;
    let mut pts = Duration::ZERO;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(12);
    loop {
        pts += Duration::from_secs(1) / 30;
        crate::tests::write_source_frame(&original, VP8_KEY_8X8, pts)
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_secs(1) / 30).await;
        let running = viewer
            .call(move |app, _, _| app.sources[&key].relay_running())
            .await
            .unwrap();
        if running && !weak.received().await.is_empty() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no owned shared decoder process"
        );
    }
    assert!(
        !healthy.received().await.is_empty(),
        "raw forwarding stopped"
    );
    weak.edge.close().await;
    healthy.edge.close().await;
    viewer.close().await.unwrap();
    host.close().await.unwrap();
}
