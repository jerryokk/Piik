use std::time::Duration;

use piik_loopback::ControlSession;
use piik_native_capture::{Capabilities, CaptureTarget, Failure};
use serde_json::{Value, json};

use super::fakes::{Outcome, StartMode, deliver, harness, harness_with_queue};
use crate::media::MediaError;
use crate::session::Fail;

const START: &str = r#"{"version":10,"id":"request_start","type":"start-share","shareId":"share_123456","source":{"kind":"display","sourceId":"1","title":"Fixture"},"audio":false,"edgeCapacity":1,"codec":"vp8","profile":{"resolution":{"width":1920,"height":1080},"maxFramerate":30,"maxBitrate":5000000,"degradationPreference":"balanced"}}"#;
const SECRET: &str = "4242424242424242424242424242424242424242424242424242424242424242";

fn vp8() -> Capabilities {
    Capabilities {
        video_capture: true,
        software_vp8: true,
        ..Capabilities::default()
    }
}

fn ack(id: &str, kind: &str) -> Value {
    json!({"version":10,"id":id,"type":kind})
}

#[tokio::test]
async fn control_messages_reject_unknown_fields_and_stale_versions() {
    let control = harness(Capabilities::default());
    for payload in [
        r#"{"version":10,"id":"request_sources","type":"list-sources","extra":true}"#,
        r#"{"version":4,"id":"request_sources","type":"list-sources"}"#,
        r#"{"version":10,"id":"short","type":"stop-share","shareId":"share_123456"}"#,
        r#"{"version":10,"id":"request_unknown","type":"unknown"}"#,
    ] {
        control.violation(payload).await;
    }
}

#[tokio::test]
async fn operational_failure_and_idempotent_stop_keep_control() {
    let control = harness(Capabilities::default());
    // VP8 is unavailable, so start fails its request only.
    let start = START.replace("share_123456", "missing_share");
    let mut harness = control;
    let value = harness.await_response(&start).await;
    assert_eq!(value["type"], "request-failed");
    let control = harness;
    for (kind, fields, expected) in [
        (
            "prepare-edge",
            r#","shareId":"missing_share","connectionId":"edge_123456","iceServers":[]"#,
            "request-failed",
        ),
        (
            "edge-answer",
            r#","shareId":"missing_share","connectionId":"edge_123456","sdp":"v=0""#,
            "request-failed",
        ),
        (
            "receive-candidate",
            r#","shareId":"missing_share","connectionId":"edge_123456","candidate":null"#,
            "request-failed",
        ),
        (
            "pause-share",
            r#","shareId":"missing_share","paused":true"#,
            "request-failed",
        ),
        (
            "set-remote-control",
            r#","shareId":"missing_share","requestId":"input_request","allow":true"#,
            "request-failed",
        ),
        (
            "publication-media",
            r#","shareId":"missing_share","publicationGeneration":"publication_123","connectionId":"edge_123456""#,
            "request-failed",
        ),
        (
            "stop-share",
            r#","shareId":"missing_share""#,
            "share-stopped",
        ),
        (
            "stop-share",
            r#","shareId":"missing_share""#,
            "share-stopped",
        ),
        (
            "close-publication",
            r#","shareId":"missing_share","publicationGeneration":"publication_123","connectionId":"edge_123456""#,
            "publication-closed",
        ),
        (
            "close-edge",
            r#","shareId":"missing_share","connectionId":"edge_123456""#,
            "edge-closed",
        ),
        (
            "close-punch",
            r#","shareId":"missing_share","connectionId":"edge_123456""#,
            "punch-closed",
        ),
        (
            "stop-receive",
            r#","shareId":"missing_share""#,
            "receive-stopped",
        ),
    ] {
        let payload = format!(r#"{{"version":10,"id":"request_{kind}","type":"{kind}"{fields}}}"#);
        let value = control.respond(&payload).await;
        assert_eq!(value["type"], expected, "{kind}: {value}");
        assert_eq!(value["id"], format!("request_{kind}"));
        if expected == "request-failed" {
            assert_eq!(value["code"], "operation-failed");
        }
    }
    // A gone target must not bypass strict publication validation.
    let reason = control
        .violation(r#"{"version":10,"id":"request_invalid","type":"close-publication","shareId":"missing_share","publicationGeneration":"publication_123","connectionId":"edge_123456","extra":true}"#)
        .await;
    assert_eq!(reason, "native control message is invalid");
}

#[tokio::test]
async fn prepare_local_edge_owns_one_strict_request_shape() {
    let control = harness(Capabilities::default());
    let valid = r#"{"version":10,"id":"request_local_edge","type":"prepare-local-edge","shareId":"share_123456","connectionId":"edge_1234567"}"#;
    control.assert_failed(valid).await;
    let invalid = valid.replace(r#""edge_1234567""#, r#""edge_1234567","iceServers":[]"#);
    assert_eq!(
        control.violation(&invalid).await,
        "native prepare-local-edge request is invalid"
    );
}

#[tokio::test]
async fn preview_failure_returns_an_advisory_response() {
    let control = harness(Capabilities::default());
    let value = control
        .respond(r#"{"version":10,"id":"request_preview","type":"source-preview","source":{"kind":"display","sourceId":"65537","title":"Display 1"}}"#)
        .await;
    assert_eq!(
        value,
        json!({"version":10,"id":"request_preview","type":"source-preview","sourceKey":"display:65537","mime":"image/bmp","data":""})
    );
}

#[tokio::test]
async fn source_enumeration_failure_does_not_close_control() {
    let control = harness(Capabilities::default());
    let value = control
        .respond(r#"{"version":10,"id":"request_sources","type":"list-sources"}"#)
        .await;
    assert_eq!(
        value,
        json!({"version":10,"id":"request_sources","type":"request-failed","code":"operation-failed"})
    );
    let options = control
        .respond(r#"{"version":10,"id":"request_options","type":"capture-options"}"#)
        .await;
    assert_eq!(options["adapters"], json!([]));
}

#[tokio::test]
async fn microphone_list_requires_capability() {
    let request = r#"{"version":10,"id":"request_mics","type":"list-microphones"}"#;
    harness(Capabilities::default())
        .assert_failed(request)
        .await;
    let control = harness(Capabilities {
        microphone: true,
        ..Capabilities::default()
    });
    let value = control.respond(request).await;
    assert_eq!(value["type"], "microphone-list");
    assert_eq!(
        value["devices"],
        json!([{"id":"device","label":"Microphone"}])
    );
}

#[tokio::test]
async fn update_share_requires_the_current_strict_profile() {
    let control = harness(Capabilities::default());
    let valid = r#"{"version":10,"id":"request_update","type":"update-share","shareId":"share_123456","profile":{"resolution":{"width":1920,"height":1080},"maxFramerate":30,"maxBitrate":5000000,"degradationPreference":"balanced","maxAudioBitrate":128000}}"#;
    control.assert_failed(valid).await;
    // Opus permits this numeric request, but the current sender does not.
    control
        .assert_failed(&valid.replace(r#""maxAudioBitrate":128000"#, r#""maxAudioBitrate":510000"#))
        .await;
    control
        .assert_failed(&valid.replace(
            r#""width":1920,"height":1080"#,
            r#""width":7680,"height":4320"#,
        ))
        .await;
    for (from, to) in [
        (r#""width":1920"#, r#""width":32768"#),
        (r#""width":1920"#, r#""width":1921"#),
        (r#""maxFramerate":30"#, r#""maxFramerate":480"#),
        (r#""maxFramerate":30"#, r#""maxFramerate":30.5"#),
        (r#""maxFramerate":30"#, r#""maxFramerate":4294967326"#),
        (r#""maxFramerate":30"#, r#""maxFramerate":0.5"#),
        (r#""maxBitrate":5000000"#, r#""maxBitrate":240000001"#),
        (r#""maxBitrate":5000000"#, r#""maxBitrate":1000"#),
    ] {
        control.assert_failed(&valid.replace(from, to)).await;
    }
    let invalid = valid.replace("128000}", r#"128000,"extra":true}"#);
    assert_eq!(
        control.violation(&invalid).await,
        "native update-share request is invalid"
    );
}

#[tokio::test]
async fn replace_share_source_keeps_one_strict_target_shape() {
    let control = harness(Capabilities::default());
    let valid = r#"{"version":10,"id":"request_source","type":"replace-share-source","shareId":"share_123456","source":{"kind":"picker","sourceId":"1","title":"Portal"},"audio":false,"adapterIndex":0,"encoderIndex":0}"#;
    control.assert_failed(valid).await;
    let extended = valid.replace(r#""encoderIndex":0"#, r#""encoderIndex":0,"extra":true"#);
    assert_eq!(
        control.violation(&extended).await,
        "native replace-share-source request is invalid"
    );
    let unknown = valid.replace(r#""kind":"picker""#, r#""kind":"unknown""#);
    assert_eq!(
        control.violation(&unknown).await,
        "native replace-share-source request is invalid"
    );
}

#[tokio::test]
async fn audio_exclusion_validates_scope_without_dropping_requested_policy() {
    let mut control = harness(Capabilities {
        microphone: true,
        system_audio: true,
        process_audio_exclusion: true,
        ..Capabilities::default()
    });
    let display = CaptureTarget {
        kind: "display".into(),
        source_id: "1".into(),
        title: "Screen".into(),
        ..CaptureTarget::default()
    };
    let window = CaptureTarget {
        kind: "window".into(),
        source_id: "2".into(),
        pid: 123,
        creation_time: "456".into(),
        title: "Voice fixture".into(),
    };
    let inner = control.session.inner.clone();
    assert!(
        inner
            .validate_audio_exclusion_for_test(&display, true, Some(&window))
            .is_ok()
    );
    let missing = CaptureTarget {
        creation_time: String::new(),
        ..window.clone()
    };
    for (source, audio, excluded) in [
        (&display, false, &window),
        (&window, true, &window),
        (&display, true, &display),
        (&display, true, &missing),
    ] {
        assert!(matches!(
            inner.validate_audio_exclusion_for_test(source, audio, Some(excluded)),
            Err(Fail::Violation(_))
        ));
    }
    assert!(
        inner
            .validate_audio_exclusion_for_test(&display, true, None)
            .is_ok()
    );
    drop(inner);
    control = harness(Capabilities {
        microphone: true,
        system_audio: true,
        ..Capabilities::default()
    });
    assert!(matches!(
        control
            .session
            .inner
            .validate_audio_exclusion_for_test(&display, true, Some(&window)),
        Err(Fail::Operation(_))
    ));
}

#[tokio::test]
async fn remote_stop_shortcut_requires_an_owned_share() {
    let control = harness(Capabilities::default());
    for (kind, key) in [
        ("get-remote-stop-shortcut", 0),
        ("set-remote-stop-shortcut", 9),
    ] {
        let payload = format!(
            r#"{{"version":10,"id":"request_shortcut","type":"{kind}","shareId":"retired_share","key":{key}}}"#
        );
        control.assert_failed(&payload).await;
    }
    control
        .violation(r#"{"version":10,"id":"request_shortcut","type":"set-remote-stop-shortcut","shareId":"retired_share","key":12}"#)
        .await;
    control
        .violation(r#"{"version":10,"id":"request_shortcut","type":"get-remote-stop-shortcut","shareId":"retired_share","key":3}"#)
        .await;
}

#[tokio::test]
async fn receive_offer_reports_reuse_only_when_requested() {
    let control = harness(Capabilities::default());
    for reuse in [false, true] {
        let payload = format!(
            r#"{{"version":10,"id":"request_offer","type":"receive-offer","shareId":"viewer_session","connectionId":"connection_1234","edgeCapacity":2,"iceServers":null,"sdp":"v=0","reuseReceiver":{reuse}}}"#
        );
        let value = control.respond(&payload).await;
        assert_eq!(value["type"], "receive-answer");
        assert_eq!(value.get("reused").is_some(), reuse, "{value}");
        if reuse {
            assert_eq!(value["reused"], true);
        }
    }
    // The Viewer role excludes a Host start.
    let mut control = control;
    let value = control.await_response(START).await;
    assert_eq!(value["type"], "request-failed");
}

#[tokio::test]
async fn failed_edge_and_late_stop_do_not_retire_another_share() {
    let mut control = harness(vp8());
    let started = control.await_response(START).await;
    assert_eq!(
        started,
        json!({"version":10,"id":"request_start","type":"share-started","shareId":"share_123456","audio":false,"codec":"vp8"})
    );
    let host = control.host(0);
    let edge = r#"{"version":10,"id":"request_edge","type":"prepare-local-edge","shareId":"share_123456","connectionId":"edge_123456"}"#;
    assert_eq!(control.respond(edge).await["type"], "edge-offer");
    control.assert_failed(edge).await;
    control
        .assert_failed(r#"{"version":10,"id":"request_edge","type":"prepare-edge","shareId":"stale_share","connectionId":"edge_123456","iceServers":[]}"#)
        .await;
    for text in ["界".repeat(4096), "界".repeat(4097)] {
        let candidate = json!({"candidate": text});
        for kind in [
            "edge-candidate",
            "receive-candidate",
            "publication-candidate",
        ] {
            let fields = if kind == "publication-candidate" {
                r#","publicationGeneration":"publication_123""#
            } else {
                ""
            };
            let payload = format!(
                r#"{{"version":10,"id":"request_remote","type":"{kind}","shareId":"share_123456","connectionId":"edge_123456","candidate":{candidate}{fields}}}"#
            );
            if text.chars().count() > 4096 {
                control.assert_failed(&payload).await;
            } else {
                control
                    .handle(&payload)
                    .await
                    .expect("upstream-admitted content");
            }
        }
    }
    assert!(control.session.inner.current("share_123456").is_some());
    // The underlying owner can finish before the Browser's stop arrives.
    host.end(Ok(()));
    let ended = control.next_event().await;
    assert_eq!(
        ended,
        json!({"version":10,"type":"share-ended","shareId":"share_123456","failed":false})
    );
    assert!(control.session.inner.current("share_123456").is_none());
    let replacement = START.replace("share_123456", "share_654321");
    assert_eq!(
        control.await_response(&replacement).await["type"],
        "share-started"
    );
    for _ in 0..2 {
        let value = control
            .respond(r#"{"version":10,"id":"request_stop","type":"stop-share","shareId":"share_123456"}"#)
            .await;
        assert_eq!(value, ack("request_stop", "share-stopped"));
    }
    assert!(control.session.inner.current("share_654321").is_some());
    control.close().await;
    assert!(control.host(1).closed.is_cancelled());
}

#[tokio::test]
async fn live_capture_failure_details_require_opt_in() {
    for opt_in in [false, true] {
        let mut control = harness(vp8());
        let start = if opt_in {
            START.replace(
                r#""audio":false"#,
                r#""audio":false,"reportCaptureFailure":true"#,
            )
        } else {
            START.to_owned()
        };
        assert_eq!(
            control.await_response(&start).await["type"],
            "share-started"
        );
        let details = Failure {
            stage: "capture".into(),
            hresult: "0x80004005".into(),
            exit_code: 3,
        };
        control.host(0).end(Err(MediaError::Capture(
            piik_native_capture::Error::Process {
                details: details.clone(),
                cause: piik_native_capture::ExitCause::Wait("fixture".into()),
            },
        )));
        let ended = control.next_event().await;
        assert_eq!(ended["failed"], true);
        assert_eq!(ended.get("failure").is_some(), opt_in, "{ended}");
        if opt_in {
            assert_eq!(
                ended["failure"],
                json!({"stage":"capture","hresult":"0x80004005","exitCode":3})
            );
        }
    }
}

#[tokio::test]
async fn source_cancellation_is_reported_only_when_requested() {
    for opt_in in [false, true] {
        let mut control = harness(vp8());
        *control.hosts.mode.lock().unwrap() = StartMode::SourceCanceled;
        let start = if opt_in {
            START.replace(
                r#""audio":false"#,
                r#""audio":false,"reportSourceCancellation":true"#,
            )
        } else {
            START.to_owned()
        };
        let value = control.await_response(&start).await;
        let expected = if opt_in {
            "source-canceled"
        } else {
            "operation-failed"
        };
        assert_eq!(value["code"], expected);
        // The slot was released on delivery; the next start is admitted.
        *control.hosts.mode.lock().unwrap() = StartMode::Succeed;
        assert_eq!(control.await_response(START).await["type"], "share-started");
        let host = control.host(0);
        *host.outcome.lock().unwrap() = Outcome::SourceCanceled;
        let replace = format!(
            r#"{{"version":10,"id":"request_replace","type":"replace-share-source","shareId":"share_123456","source":{{"kind":"display","sourceId":"2","title":"Fixture"}},"audio":false,"reportSourceCancellation":{opt_in}}}"#
        );
        assert_eq!(control.await_response(&replace).await["code"], expected);
    }
}

#[tokio::test]
async fn capture_border_preference_follows_source_and_capabilities() {
    for supported in [false, true] {
        let mut control = harness(Capabilities {
            capture_border_control: supported,
            ..vp8()
        });
        let start = START.replace(
            r#""audio":false"#,
            r#""audio":false,"showCaptureBorder":true"#,
        );
        assert_eq!(
            control.await_response(&start).await["type"],
            "share-started"
        );
        let host = control.host(0);
        assert_eq!(host.options.video.show_capture_border, supported);
        let update = r#"{"version":10,"id":"request_update","type":"update-share","shareId":"share_123456","profile":{"resolution":{"width":854,"height":480},"maxFramerate":30,"maxBitrate":5000000,"degradationPreference":"balanced"}}"#;
        assert_eq!(
            control.await_response(update).await,
            json!({"version":10,"id":"request_update","type":"share-updated","shareId":"share_123456"})
        );
        for show in [false, true] {
            let replace = format!(
                r#"{{"version":10,"id":"request_replace","type":"replace-share-source","shareId":"share_123456","source":{{"kind":"display","sourceId":"2","title":"Other"}},"showCaptureBorder":{show}}}"#
            );
            assert_eq!(
                control.await_response(&replace).await["type"],
                "share-source-replaced"
            );
            let calls = host.calls.lock().unwrap().clone();
            assert!(
                calls.contains(&format!("border:{}", show && supported)),
                "{calls:?}"
            );
        }
    }
}

#[tokio::test]
async fn microphone_gain_is_synchronous_and_device_setup_uses_the_slot() {
    let mut control = harness(vp8());
    assert_eq!(control.await_response(START).await["type"], "share-started");
    let host = control.host(0);
    let gain = r#"{"version":10,"id":"request_gain","type":"set-microphone","shareId":"share_123456","volume":1.5}"#;
    assert_eq!(
        control.respond(gain).await,
        ack("request_gain", "microphone-set")
    );
    for invalid in [
        r#""volume":2.5"#,
        r#""volume":-1"#,
        r#""deviceId":"a\u0000b""#,
    ] {
        let payload = format!(
            r#"{{"version":10,"id":"request_gain","type":"set-microphone","shareId":"share_123456",{invalid}}}"#
        );
        control.violation(&payload).await;
    }
    control
        .violation(r#"{"version":10,"id":"request_gain","type":"set-microphone","shareId":"share_123456"}"#)
        .await;
    *host.outcome.lock().unwrap() = Outcome::Block;
    let enable = r#"{"version":10,"id":"request_mic","type":"set-microphone","shareId":"share_123456","enabled":true,"volume":1}"#;
    assert!(control.handle(enable).await.unwrap().is_none());
    // Gain committed first, in command order; the device setup holds the slot.
    assert_eq!(
        host.calls.lock().unwrap()[..2],
        ["gain".to_owned(), "gain".to_owned()]
    );
    control
        .assert_failed(enable.replace("request_mic", "request_again").as_str())
        .await;
    host.release();
    assert_eq!(
        control.next_event().await,
        ack("request_mic", "microphone-set")
    );
}

#[tokio::test]
async fn capture_startup_and_replacement_remain_cancellable() {
    for kind in ["start-share", "replace-share-source"] {
        let mut control = harness(vp8());
        let payload = if kind == "replace-share-source" {
            assert_eq!(control.await_response(START).await["type"], "share-started");
            *control.host(0).outcome.lock().unwrap() = Outcome::Block;
            r#"{"version":10,"id":"request_replace","type":"replace-share-source","shareId":"share_123456","source":{"kind":"display","sourceId":"2","title":"Fixture"},"audio":false}"#.to_owned()
        } else {
            *control.hosts.mode.lock().unwrap() = StartMode::Block;
            START.replace(
                r#""width":1920,"height":1080"#,
                r#""width":1280,"height":720"#,
            )
        };
        assert!(
            control.handle(&payload).await.unwrap().is_none(),
            "{kind} did not defer"
        );
        // No unbounded parallel initialization while the first is pending.
        control.assert_failed(&payload).await;
        let candidate = r#"{"version":10,"id":"request_ice","type":"edge-candidate","shareId":"share_123456","connectionId":"missing_edge","candidate":null}"#;
        control
            .handle(candidate)
            .await
            .expect("independent control");
        let stop =
            r#"{"version":10,"id":"request_stop","type":"stop-share","shareId":"share_123456"}"#;
        let stopped = tokio::time::timeout(Duration::from_secs(2), control.handle(stop))
            .await
            .expect("stop could not cancel pending capture")
            .unwrap();
        assert_eq!(stopped, Some(ack("request_stop", "share-stopped")));
        assert!(control.session.inner.current("share_123456").is_none());
        let id = if kind == "start-share" {
            "request_start"
        } else {
            "request_replace"
        };
        let failed = control.next_event().await;
        assert_eq!(
            (failed["id"].as_str(), failed["type"].as_str()),
            (Some(id), Some("request-failed"))
        );
    }
}

#[tokio::test]
async fn close_cancels_a_pending_start() {
    let control = harness(vp8());
    *control.hosts.mode.lock().unwrap() = StartMode::Block;
    assert!(control.handle(START).await.unwrap().is_none());
    tokio::time::timeout(Duration::from_secs(2), control.close())
        .await
        .expect("close joins the pending start");
    assert!(control.hosts.hosts.lock().unwrap().is_empty());
    control.assert_failed(START).await;
}

#[tokio::test]
async fn punch_response_backpressure_bounds_workers() {
    let mut control = harness_with_queue(Capabilities::default(), 4);
    let prepare = format!(
        r#"{{"version":10,"id":"prepare_request","type":"prepare-punch","shareId":"review_share","connectionId":"review_connection","iceServers":[],"secret":"{SECRET}","edgeCapacity":1}}"#
    );
    assert!(control.handle(&prepare).await.unwrap().is_none());
    let prepared = control.next_event().await;
    assert_eq!(
        prepared,
        json!({"version":10,"id":"prepare_request","type":"punch-prepared","prediction":null})
    );
    for _ in 0..4 {
        control
            .filler
            .try_send(piik_loopback::ControlEvent::new(Value::Null))
            .unwrap();
    }
    let run = r#"{"version":10,"id":"run_request","type":"run-punch","shareId":"review_share","connectionId":"review_connection","prediction":{"address":"198.51.100.1","port":5000,"step":0}}"#;
    assert!(
        control.handle(run).await.unwrap().is_none(),
        "first probe was not admitted"
    );
    for _ in 0..64 {
        control.assert_failed(run).await;
    }
    let close = r#"{"version":10,"id":"close_request","type":"close-punch","shareId":"review_share","connectionId":"review_connection"}"#;
    assert_eq!(
        control.respond(close).await,
        ack("close_request", "punch-closed")
    );
    // Draining the queue delivers the completion; it is not a session failure.
    for _ in 0..4 {
        let _ = control.events.recv().await.unwrap();
    }
    let completion = tokio::time::timeout(Duration::from_secs(1), control.events.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(completion.value["connected"], false);
    // Until the writer releases it, the slot still bounds the next phase.
    let next = prepare.replace("review_connection", "next_connection");
    control.assert_failed(&next).await;
    let before_write = completion.before_write.expect("admission release");
    before_write();
    assert!(
        control.handle(&next).await.unwrap().is_none(),
        "next preparation was not admitted"
    );
    // A stale release must not free the successor's admission.
    assert!(control.session.inner.lock().punch.is_some());
}

#[tokio::test]
async fn stale_slot_release_keeps_the_successor() {
    let mut control = harness(Capabilities::default());
    let prepare = |id: &str| {
        format!(
            r#"{{"version":10,"id":"{id}","type":"prepare-punch","shareId":"review_share","connectionId":"{id}","iceServers":[],"secret":"{SECRET}","edgeCapacity":1}}"#
        )
    };
    assert!(
        control
            .handle(&prepare("first_request"))
            .await
            .unwrap()
            .is_none()
    );
    let first = control.events.recv().await.unwrap();
    let release = first.before_write.unwrap();
    release();
    assert!(
        control
            .handle(&prepare("second_request"))
            .await
            .unwrap()
            .is_none()
    );
    // The first event's release already ran; a duplicate cannot run, and the
    // successor's slot remains until its own event is written.
    let second = control.events.recv().await.unwrap();
    assert!(control.session.inner.lock().punch.is_some());
    assert!(deliver(second).is_some());
    assert!(control.session.inner.lock().punch.is_none());
}

#[tokio::test]
async fn punch_preparation_preserves_media_role() {
    for source in ["", "retired_receiver"] {
        let control = harness(Capabilities::default());
        let payload = format!(
            r#"{{"version":10,"id":"prepare_request","type":"prepare-punch","shareId":"retired_share","connectionId":"new_connection","sourceConnectionId":"{source}","iceServers":null,"secret":"{SECRET}"}}"#
        );
        control.assert_failed(&payload).await;
        assert!(
            control
                .session
                .inner
                .current_viewer("retired_share")
                .is_none()
        );
        // A failed preparation releases its slot immediately.
        assert!(control.session.inner.lock().punch.is_none());
    }
    let control = harness(Capabilities::default());
    let payload = format!(
        r#"{{"version":10,"id":"prepare_request","type":"prepare-punch","shareId":"receiving_share","connectionId":"new_connection","edgeCapacity":2,"iceServers":[],"secret":"{SECRET}"}}"#
    );
    assert!(control.handle(&payload).await.unwrap().is_none());
    assert!(
        control
            .session
            .inner
            .current_viewer("receiving_share")
            .is_some()
    );
    for invalid in [
        payload.replace(SECRET, "42"),
        payload.replace(r#""edgeCapacity":2"#, r#""edgeCapacity":21"#),
        payload.replace(
            r#""edgeCapacity":2"#,
            r#""edgeCapacity":2,"sourceConnectionId":"source_connection""#,
        ),
    ] {
        control.violation(&invalid).await;
    }
}

#[tokio::test]
async fn punch_requires_explicit_mapping_step() {
    for step in ["", r#","step":null"#, r#","step":0"#] {
        let control = harness(Capabilities::default());
        let payload = format!(
            r#"{{"version":10,"id":"run_request","type":"run-punch","shareId":"share_id_1","connectionId":"connection","prediction":{{"address":"198.51.100.1","port":5000{step}}}}}"#
        );
        if step == r#","step":0"# {
            control.assert_failed(&payload).await;
        } else {
            control.violation(&payload).await;
        }
    }
}

#[tokio::test]
async fn role_exclusivity_between_host_and_viewer() {
    let mut control = harness(vp8());
    assert_eq!(control.await_response(START).await["type"], "share-started");
    let offer = r#"{"version":10,"id":"request_offer","type":"receive-offer","shareId":"viewer_session","connectionId":"connection_1234","edgeCapacity":2,"iceServers":[],"sdp":"v=0"}"#;
    control.assert_failed(offer).await;
    let pause = r#"{"version":10,"id":"request_pause","type":"pause-share","shareId":"share_123456","paused":true}"#;
    assert_eq!(
        control.respond(pause).await,
        ack("request_pause", "share-paused")
    );
    assert!(
        control
            .host(0)
            .calls
            .lock()
            .unwrap()
            .contains(&"paused:true".to_owned())
    );
    let stop = r#"{"version":10,"id":"request_stop","type":"stop-share","shareId":"share_123456"}"#;
    assert_eq!(
        control.respond(stop).await,
        ack("request_stop", "share-stopped")
    );
    assert_eq!(control.respond(offer).await["type"], "receive-answer");
}

#[tokio::test]
async fn session_implements_the_loopback_contract() {
    let control = harness(Capabilities::default());
    let session: &dyn ControlSession = &control.session;
    let cancel = piik_loopback::CancellationToken::new();
    let value = session
        .handle(
            &cancel,
            br#"{"version":10,"id":"request_options","type":"capture-options"}"#,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(value.value["type"], "capture-options");
    session.close().await;
    session.close().await;
    control
        .assert_failed(r#"{"version":10,"id":"request_stop","type":"pause-share","shareId":"share_123456","paused":true}"#)
        .await;
}
