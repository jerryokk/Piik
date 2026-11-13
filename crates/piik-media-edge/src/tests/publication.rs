// Publications. The SFU answerer is a Piik receiver on its own engine: str0m
// accepts the simulcast offer and receives every RID on the same single-port
// endpoint.

use std::time::SystemTime;

use super::*;

fn options(id: &str) -> EdgeOptions {
    EdgeOptions {
        connection_id: id.into(),
        ..EdgeOptions::default()
    }
}

async fn reservations(engine: &Engine, source: &VideoSource) -> (usize, usize) {
    let key = source.key;
    engine
        .call(move |app, _, _| {
            app.sources.get(&key).map_or((0, 0), |state| {
                (state.reservations, state.publications.len())
            })
        })
        .await
        .unwrap()
}

/// A connected publication with its SFU-side receiver.
struct Upload {
    publication: Publication,
    receiver: Receiver,
    sfu: Engine,
}

async fn upload(engine: &Engine, source: &VideoSource, id: &str) -> Upload {
    let mut publication_events = events();
    let publication = engine
        .new_publication(
            source,
            EdgeOptions {
                connection_id: id.into(),
                events: Some(publication_events.sink.clone()),
                ..EdgeOptions::default()
            },
        )
        .await
        .unwrap();
    let offer = publication.create_offer().await.unwrap();
    let sfu = loopback_engine();
    let mut receiver_events = events();
    let (receiver, answer) = sfu
        .new_receiver(ReceiverOptions {
            offer,
            edge_capacity: 1,
            events: Some(receiver_events.sink.clone()),
            ..ReceiverOptions::default()
        })
        .await
        .unwrap();
    publication.set_answer(&answer).await.unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while publication.state() != PcState::Connected || receiver.state() != PcState::Connected {
        assert!(
            tokio::time::Instant::now() < deadline,
            "publication did not connect"
        );
        for event in drain(&mut publication_events.receiver) {
            if let EdgeEvent::LocalCandidate { candidate, .. } = event {
                receiver.add_remote_candidate(candidate).await.unwrap();
            }
        }
        for event in drain(&mut receiver_events.receiver) {
            if let EdgeEvent::LocalCandidate { candidate, .. } = event {
                publication.add_remote_candidate(candidate).await.unwrap();
            }
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    Upload {
        publication,
        receiver,
        sfu,
    }
}

async fn received(upload: &Upload) -> u64 {
    let key = upload.receiver.key;
    upload
        .sfu
        .call(move |app, _, _| {
            app.receivers
                .get(&key)
                .map_or(0, |receiver| receiver.video_packets)
        })
        .await
        .unwrap_or(0)
}

#[tokio::test]
async fn publication_owns_one_reservation_and_source_lifetime() {
    let engine = loopback_engine();
    let source = engine.new_source("vp8", 3, 2, None).await.unwrap();
    for layer in 0..2 {
        source.set_format(layer, 320, 180).await.unwrap();
    }
    source
        .configure_outputs(vec![90_000, 300_000])
        .await
        .unwrap();
    let publication = engine
        .new_publication(&source, options("publication"))
        .await
        .unwrap();
    let candidate = engine
        .new_publication(&source, options("publication"))
        .await
        .unwrap();
    assert!(
        engine
            .new_publication(&source, options("third"))
            .await
            .is_err(),
        "third publication was admitted"
    );
    assert_eq!(
        reservations(&engine, &source).await,
        (2, 2),
        "failed publication leaked its reservation"
    );
    let edge = engine.new_edge(&source, options("sibling")).await.unwrap();
    let overflow = engine.new_edge(&source, options("overflow")).await;
    assert!(
        matches!(overflow, Err(Error::SourceCapacity)),
        "publication did not hold one copy"
    );
    candidate.close().await;
    assert_eq!(reservations(&engine, &source).await, (1, 1));
    assert_ne!(
        publication.state(),
        PcState::Closed,
        "candidate rollback retired the current publication"
    );
    let _replacement = engine
        .new_publication(&source, options("replacement"))
        .await
        .unwrap();
    source.mark_layer_unavailable(0).await.unwrap();
    assert!(
        engine
            .new_publication(&source, options("failed-output"))
            .await
            .is_err(),
        "failed output was admitted into publication metadata"
    );
    assert_eq!(
        reservations(&engine, &source).await,
        (0, 0),
        "failed demand retained a reservation"
    );
    source
        .configure_outputs(vec![90_000, 300_000])
        .await
        .unwrap();
    let restored = engine
        .new_publication(&source, options("restored-output"))
        .await
        .expect("validated outputs restore publication admission");
    source.close().await;
    assert_eq!(
        restored.state(),
        PcState::Closed,
        "source retirement did not release the publication"
    );
    assert_eq!(edge.state(), PcState::Closed);
    let remaining = engine
        .call(|app, _, _| app.publications.len())
        .await
        .unwrap();
    assert_eq!(remaining, 0);
    engine.close().await.unwrap();
}

#[tokio::test]
async fn publication_admission_ignores_unrelated_failed_outputs() {
    let engine = loopback_engine();
    let source = engine.new_source("vp8", 2, 2, None).await.unwrap();
    let publication = engine
        .new_publication(&source, options("initial"))
        .await
        .expect("unconfigured outputs prevented initial publication");
    source
        .configure_outputs(vec![90_000, 300_000, 90_000, 90_000])
        .await
        .unwrap();
    for slot in [2usize, 3, 0, 1] {
        source.mark_layer_unavailable(slot).await.unwrap();
        let candidate = engine
            .new_publication(&source, options("replacement"))
            .await;
        if slot >= 2 {
            let candidate =
                candidate.unwrap_or_else(|error| panic!("unrelated failed output {slot}: {error}"));
            let media = candidate.media().await.unwrap();
            assert_eq!(
                media.layers.len(),
                2,
                "independent failure changed the base publication"
            );
            assert_eq!(reservations(&engine, &source).await.1, 2);
            candidate.close().await;
        } else {
            assert!(candidate.is_err(), "failed base output {slot} was admitted");
        }
    }
    drop(publication);
    engine.close().await.unwrap();
}

#[tokio::test]
async fn publication_offer_declares_simulcast_rids() {
    let engine = loopback_engine();
    let source = engine.new_source("vp8", 1, 2, None).await.unwrap();
    source.set_format(0, 320, 180).await.unwrap();
    source.set_format(1, 640, 360).await.unwrap();
    source
        .configure_outputs(vec![90_000, 300_000])
        .await
        .unwrap();
    let audio = engine.new_audio_source(1, 96_000).await.unwrap();
    let publication = engine
        .new_publication(
            &source,
            EdgeOptions {
                connection_id: "rids".into(),
                audio: Some(audio),
                ..EdgeOptions::default()
            },
        )
        .await
        .unwrap();
    let offer = publication.create_offer().await.unwrap();
    assert!(
        offer.contains("a=rid:q send") && offer.contains("a=rid:h send"),
        "{offer}"
    );
    assert!(offer.contains("m=audio"));
    let media = publication.media().await.unwrap();
    assert_eq!(media.codec, "vp8");
    assert!(media.audio);
    assert_eq!(media.audio_bitrate, 96_000);
    assert_eq!(
        media.layers,
        vec![
            PublicationLayer {
                rid: "q".into(),
                width: 320,
                height: 180,
                bitrate: 90_000
            },
            PublicationLayer {
                rid: "h".into(),
                width: 640,
                height: 360,
                bitrate: 300_000
            },
        ]
    );
    let json = serde_json::to_value(&media).unwrap();
    assert_eq!(json["audioBitrate"], 96_000);
    assert_eq!(json["layers"][1]["rid"], "h");
    engine.close().await.unwrap();
}

#[tokio::test]
async fn publication_overlap_to_shared_sfu_endpoint() {
    let engine = loopback_engine();
    let source = engine.new_source("vp8", 2, 1, None).await.unwrap();
    source.set_format(0, 8, 8).await.unwrap();
    source.configure_outputs(vec![90_000]).await.unwrap();
    let mut pts = Duration::ZERO;
    let mut delivery = async |uploads: &[&Upload]| {
        let before: Vec<u64> = futures_join(uploads).await;
        for _ in 0..20 {
            pts += Duration::from_secs(1) / 30;
            write_source_frame(&source, VP8_KEY_8X8, pts).await.unwrap();
            tokio::time::sleep(Duration::from_secs(1) / 30).await;
        }
        let after = futures_join(uploads).await;
        for (index, (before, after)) in before.iter().zip(after).enumerate() {
            assert!(after > *before, "publication {index} lost media");
        }
    };
    let current = upload(&engine, &source, "current").await;
    delivery(&[&current]).await;
    let candidate = upload(&engine, &source, "candidate").await;
    delivery(&[&current, &candidate]).await;
    candidate.publication.close().await;
    delivery(&[&current]).await;
    current.sfu.close().await.unwrap();
    candidate.sfu.close().await.unwrap();
    engine.close().await.unwrap();
}

async fn futures_join(uploads: &[&Upload]) -> Vec<u64> {
    let mut counts = Vec::new();
    for upload in uploads {
        counts.push(received(upload).await);
    }
    counts
}

#[tokio::test]
async fn publication_quality_uses_highest_sent_layer_cadence() {
    let engine = loopback_engine();
    let source = engine.new_source("vp8", 1, 2, None).await.unwrap();
    source.set_format(0, 320, 180).await.unwrap();
    source.set_format(1, 640, 360).await.unwrap();
    source
        .configure_outputs(vec![90_000, 300_000])
        .await
        .unwrap();
    let upload = upload(&engine, &source, "publication-quality").await;
    let publication = &upload.publication;
    publication.set_active_count(2).await.unwrap();
    let now = SystemTime::now();
    assert!(
        publication.quality_sample(now).await.is_none(),
        "first sample invented a window"
    );
    for frame in 0..60u32 {
        let pts = Duration::from_secs(1) / 30 * (frame + 1);
        source.begin_frame(pts).await.unwrap();
        source
            .write_video(0, super::frame(VP8_KEY_8X8, pts, true))
            .await
            .unwrap();
        if frame % 2 == 0 {
            source
                .write_video(1, super::frame(VP8_KEY_8X8, pts, true))
                .await
                .unwrap();
        }
        tokio::time::sleep(Duration::from_secs(1) / 30).await;
    }
    let sample = publication
        .quality_sample(now + Duration::from_secs(2))
        .await
        .expect("bounded window");
    assert_eq!(sample.rid, "h", "{sample:?}");
    assert_eq!(sample.active_video_encoding_count, 2, "{sample:?}");
    assert_eq!((sample.width, sample.height), (640, 360));
    assert_eq!(sample.video_encoding_count, 2);
    assert!(sample.interval_frames_sent > 0);
    assert!(
        (sample.frames_per_second - sample.interval_frames_sent as f64 / 2.0).abs() < 1e-9,
        "highest sent output owns cadence: {sample:?}"
    );
    assert!(sample.bitrate_kbps > 0.0);
    assert_eq!(sample.codec, "vp8");
    upload.sfu.close().await.unwrap();
    engine.close().await.unwrap();
}

#[tokio::test]
async fn publication_lowest_layer_budget_follows_active_count() {
    // Exhausting the residual allocation over loopback is not deterministic,
    // so this checks the plan directly: a zero residual budget plans the
    // codec floor, never the ceiling, and one active RID recovers it.
    let ceiling = [300_000, 8_000_000];
    let plan = |budget: i64| {
        let mut groups = crate::groups::OutputGroups::<u8>::new(crate::groups::MediaId(0), false);
        let demand = crate::groups::publication_demand(
            0u8,
            crate::groups::MediaId(0),
            2,
            budget,
            ceiling[0],
        );
        struct Media;
        impl crate::groups::GroupMedia for Media {
            type Error = std::convert::Infallible;
            fn new_group(
                &mut self,
                _: usize,
            ) -> std::result::Result<crate::groups::MediaId, Self::Error> {
                unreachable!()
            }
            fn codec_budget(&self, _: crate::groups::MediaId, budget: i64) -> i64 {
                budget
            }
        }
        groups
            .plan(&[demand], &ceiling, 2, Duration::ZERO, &mut Media)
            .0
    };
    assert_eq!(
        plan(0).bitrates[0],
        1000,
        "zero residual allocation restored the encoder ceiling"
    );
    let restored = plan(250_000);
    assert!(
        restored.bitrates[0] > 1000 && restored.bitrates[0] <= 300_000,
        "{restored:?}"
    );
}
