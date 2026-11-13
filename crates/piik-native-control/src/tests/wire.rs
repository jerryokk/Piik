use piik_native_capture::CaptureTarget;
use piik_protocol::{DegradationPreference, QualityResolution, QualitySettings};

use crate::media::{QualityProfile, punch_target};
use crate::wire::*;

#[test]
fn stun_urls_use_the_current_bounded_wire() {
    let servers = ice_servers(&[IceServer {
        urls: vec![
            "stun:example.test:3478".into(),
            "stun:example.test:3479".into(),
        ],
    }])
    .unwrap();
    assert_eq!(
        servers,
        vec![vec!["stun:example.test:3478", "stun:example.test:3479"]]
    );
    for value in [
        "turn:example.com:3478",
        "stun:user@example.com:3478",
        "stun:example.com:0",
        "stun:example.com/path",
        "stun:example.com:",
        "stun:",
        "stun:exa mple.com",
    ] {
        assert!(!valid_stun_url(value), "{value}");
    }
    assert!(valid_stun_url("STUN:[::1]:3478"));
    let many = vec![
        IceServer {
            urls: vec!["stun:a.test".into()]
        };
        MAX_ICE_SERVERS + 1
    ];
    assert_eq!(
        ice_servers(&many),
        Err("native ICE server list is too large")
    );
    let wide = IceServer {
        urls: vec!["stun:a.test".into(); MAX_URLS_PER_SERVER + 1],
    };
    assert_eq!(ice_servers(&[wide]), Err("native ICE server is invalid"));
    assert_eq!(
        ice_servers(&[IceServer::default()]),
        Err("native ICE server is invalid")
    );
}

#[test]
fn relayed_text_keeps_upstream_limits() {
    for character in ["a", "界", "😀"] {
        let units = piik_protocol::utf16_length(character);
        let sdp = character.repeat(MAX_SDP_UNITS / units);
        assert!(valid_sdp(&sdp));
        assert!(!valid_sdp(&(sdp + character)));
        assert!(!valid_sdp(""));
        let payload = format!(
            r#"{{"candidate":{:?},"sdpMid":{:?},"usernameFragment":{:?},"sdpMLineIndex":255}}"#,
            character.repeat(MAX_CANDIDATE_UNITS / units),
            character.repeat(128 / units),
            character.repeat(256 / units),
        );
        let mut candidate: IceCandidateInit = serde_json::from_str(&payload).unwrap();
        assert!(valid_candidate(Some(&candidate)));
        candidate.candidate.push_str(character);
        assert!(!valid_candidate(Some(&candidate)));
    }
    let index: IceCandidateInit =
        serde_json::from_str(r#"{"candidate":"","sdpMLineIndex":256}"#).unwrap();
    assert!(!valid_candidate(Some(&index)));
    assert!(valid_candidate(None));
}

#[test]
fn requests_reject_unknown_fields_at_every_depth() {
    let valid = r#"{"version":10,"id":"request_source","type":"replace-share-source","shareId":"share_123456","source":{"kind":"picker","sourceId":"1","title":"Portal"},"audio":false,"adapterIndex":0,"encoderIndex":0}"#;
    let request: ReplaceShareSourceRequest = decode_strict(valid.as_bytes()).unwrap();
    assert!(request.source.valid());
    assert!(
        decode_strict::<ReplaceShareSourceRequest>(
            valid
                .replace(r#""encoderIndex":0"#, r#""encoderIndex":0,"extra":1"#)
                .as_bytes()
        )
        .is_none()
    );
    assert!(
        decode_strict::<ReplaceShareSourceRequest>(
            valid
                .replace(r#""title":"Portal""#, r#""title":"Portal","x":1"#)
                .as_bytes()
        )
        .is_none()
    );
    // Null reads as the zero value, and trailing input is rejected.
    let null: ShareRequest =
        decode_strict(br#"{"version":10,"id":"request_stop","type":"stop-share","shareId":null}"#)
            .unwrap();
    assert!(null.share_id.is_empty());
    assert!(decode_strict::<ShareRequest>(br#"{"version":10} {}"#).is_none());
}

#[test]
fn quality_profile_stays_strict() {
    let profile = r#"{"resolution":{"width":1920,"height":1080},"maxFramerate":30,"maxBitrate":5000000,"degradationPreference":"balanced","maxAudioBitrate":128000}"#;
    let payload = format!(
        r#"{{"version":10,"id":"request_update","type":"update-share","shareId":"share_123456","profile":{profile}}}"#
    );
    let request: UpdateShareRequest = decode_strict(payload.as_bytes()).unwrap();
    assert!(QualityProfile::from_settings(request.profile.as_ref().unwrap()).valid());
    let extended = payload.replace("128000}", r#"128000,"extra":true}"#);
    assert!(decode_strict::<UpdateShareRequest>(extended.as_bytes()).is_none());
    let null = payload.replace(profile, "null");
    assert!(decode_strict::<UpdateShareRequest>(null.as_bytes()).is_none());
    let absent: UpdateShareRequest = decode_strict(
        br#"{"version":10,"id":"request_update","type":"update-share","shareId":"share_123456"}"#,
    )
    .unwrap();
    assert!(absent.profile.is_none());
}

#[test]
fn quality_settings_map_once_into_native_media() {
    let mut settings = QualitySettings {
        resolution: QualityResolution {
            width: 2560,
            height: 1440,
        },
        max_framerate: 60.0,
        max_bitrate: 12_000_000,
        degradation_preference: DegradationPreference::MaintainFramerate,
        max_audio_bitrate: Some(192_000),
    };
    let profile = QualityProfile::from_settings(&settings);
    assert!(profile.valid());
    assert_eq!(
        (
            profile.video.width,
            profile.video.height,
            profile.video.framerate,
            profile.video.bitrate
        ),
        (2560, 1440, 60, 12_000_000)
    );
    assert_eq!(profile.video.preference, "maintain-framerate");
    assert_eq!(profile.audio_bitrate, 192_000);
    settings.resolution = QualityResolution {
        width: 1920,
        height: 1200,
    };
    assert_eq!(QualityProfile::from_settings(&settings).video.height, 1200);
    settings.resolution = QualityResolution {
        width: 3840,
        height: 2160,
    };
    assert!(QualityProfile::from_settings(&settings).valid(), "4K60");
    settings.max_framerate = 30.0;
    assert!(QualityProfile::from_settings(&settings).valid(), "4K30");
    settings.max_audio_bitrate = None;
    assert_eq!(
        QualityProfile::from_settings(&settings).audio_bitrate,
        128_000
    );
    for fps in [30.5, 65.0, 120.0, 4_294_967_326.0] {
        settings.max_framerate = fps;
        assert!(!QualityProfile::from_settings(&settings).valid(), "{fps}");
    }
}

#[test]
fn capture_target_keys_and_punch_targets() {
    let window = CaptureTarget {
        kind: "window".into(),
        source_id: "2".into(),
        pid: 123,
        creation_time: "456".into(),
        title: "Fixture".into(),
    };
    assert_eq!(capture_target_key(&window), "window:2:123:456");
    let display = CaptureTarget {
        kind: "display".into(),
        source_id: "65537".into(),
        ..CaptureTarget::default()
    };
    assert_eq!(capture_target_key(&display), "display:65537");
    let prediction = |address: &str, port, step| piik_protocol::PunchPrediction {
        address: address.into(),
        port,
        step,
    };
    assert_eq!(
        punch_target(&prediction("198.51.100.1", 5000, 2))
            .unwrap()
            .port(),
        5008
    );
    for invalid in [
        prediction("10.0.0.1", 5000, 0),
        prediction("127.0.0.1", 5000, 0),
        prediction("169.254.0.1", 5000, 0),
        prediction("198.51.100.1", 1023, 0),
        prediction("198.51.100.1", 65535, 1),
        prediction("198.51.100.1", 5000, 17),
    ] {
        assert!(punch_target(&invalid).is_err(), "{invalid:?}");
    }
}

#[test]
fn responses_keep_names_and_omission() {
    let answer = |reused| {
        serde_json::to_value(ReceiveAnswerResponse {
            envelope: ResponseEnvelope::new("request_offer", "receive-answer"),
            share_id: "viewer_session".into(),
            connection_id: "connection_1234".into(),
            sdp: "v=0".into(),
            audio: false,
            codec: "vp8".into(),
            reused,
        })
        .unwrap()
    };
    assert!(answer(None).get("reused").is_none());
    assert_eq!(answer(Some(true))["reused"], true);
    let quality = serde_json::to_value(EdgeQualityEvent::new(
        EventEnvelope::new("edge-quality", "share_123456"),
        &crate::media::QualitySample {
            interval_frames_sent: 7,
            ..Default::default()
        },
    ))
    .unwrap();
    assert_eq!(quality["intervalFramesEncoded"], 7);
    assert!(quality["reason"].is_null());
    assert!(quality.get("connectionId").is_none());
    let ended = serde_json::to_value(ShareEndedEvent {
        version: 10,
        kind: "share-ended",
        share_id: "share_123456".into(),
        failed: true,
        failure: None,
    })
    .unwrap();
    assert_eq!(
        ended,
        serde_json::json!({"version":10,"type":"share-ended","shareId":"share_123456","failed":true})
    );
}
