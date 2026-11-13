use std::time::Instant;

use super::*;
use crate::output::tests::Fixture;

fn single(ceiling: i64) -> Fixture {
    let mut source = Fixture::new(&[ceiling]);
    source.temporal_seen = 0;
    source
}

#[test]
fn publication_rids_follow_encoding_count() {
    assert_eq!(publication_rid(0, 1), "");
    assert_eq!(
        (
            publication_rid(0, 3),
            publication_rid(1, 3),
            publication_rid(2, 3)
        ),
        ("q", "h", "f")
    );
    assert!(PublicationAllocator::new(CodecKind::Opus, 1, 1).is_err());
    assert!(PublicationAllocator::new(CodecKind::Vp8, 0, 1).is_err());
    assert!(PublicationAllocator::new(CodecKind::Vp8, 1, 0).is_err());
}

#[test]
fn encoder_demand_follows_current_source_and_lifetime() {
    let mut source = single(300_000);
    let mut publication =
        PublicationAllocator::new(CodecKind::Vp8, 1, 100_000).expect("publication");
    let check = |publication: &PublicationAllocator, source: &Fixture, want: i64| {
        assert_eq!(publication.additional_probe_demand(source), want);
        assert_eq!(
            publication.video_budget(),
            100_000,
            "desired output replaced measured capacity"
        );
    };
    source.controlled = true;
    check(&publication, &source, 0); // Not attached yet.
    publication.set_connected(&source).expect("connect");
    check(&publication, &source, 300_000); // No measured allocation: not ceiling minus capacity.
    source.controlled = false;
    check(&publication, &source, 0); // Raw forwarding cannot spend capacity inside this layer.
    source.controlled = true;
    publication.set_active_count(&source, 0).expect("inactive");
    check(&publication, &source, 0);
    publication.set_active_count(&source, 1).expect("active");
    check(&publication, &source, 300_000);
    let mut next = single(600_000);
    next.controlled = true;
    publication.replace_source(&next).expect("replace");
    source.controlled = false;
    check(&publication, &next, 600_000); // Retiring the old encoder cannot erase new demand.
    publication.set_channel_capacity(&next, 600_000);
    assert_eq!(
        publication.additional_probe_demand(&next),
        0,
        "satisfied encoder kept probing"
    );
    publication.set_channel_capacity(&next, 100_000);
    check(&publication, &next, 600_000);
    next.controlled = false;
    check(&publication, &next, 0);
    next.controlled = true;
    publication.close();
    check(&publication, &next, 0);
    assert_eq!(publication.required_active_count(), 0);
    assert_eq!(
        publication.set_active_count(&next, 1),
        Err(PublicationError::Closed)
    );
}

// The allocation reads the same committed capacity as the budget.
#[test]
fn congestion_uses_committed_capacity() {
    let source = Fixture::new(&[90_000, 300_000]);
    let mut publication =
        PublicationAllocator::new(CodecKind::Vp8, 2, 1_200_000).expect("publication");
    publication.on_congestion(&source, Congestion::EarlyWarning, 100_000_000);
    publication.on_congestion(&source, Congestion::Congested, 100_000_000);
    assert_eq!(
        publication.video_budget(),
        1_200_000,
        "promoted an unverified capacity"
    );
    publication.on_congestion(&source, Congestion::Congested, 500_000);
    publication.on_congestion(&source, Congestion::None, 800_000);
    assert_eq!(publication.video_budget(), 500_000);
    publication.probe_started();
    publication.on_congestion(&source, Congestion::Congested, 250_000);
    assert_eq!(
        publication.video_budget(),
        500_000,
        "probe congestion reached the budget"
    );
    assert!(
        !publication.limited(&source),
        "probe congestion reported as a limitation"
    );
    publication.probe_finished(&source, false, 2_000_000);
    assert_eq!(
        publication.video_budget(),
        500_000,
        "inconclusive probe raised capacity"
    );
    publication.on_congestion(&source, Congestion::Congested, 250_000);
    assert_eq!(
        publication.video_budget(),
        250_000,
        "retired probe suppressed congestion"
    );
    publication.probe_started();
    publication.probe_finished(&source, true, 800_000);
    assert_eq!(publication.video_budget(), 800_000);
}

fn measured(rates: &[i64]) -> Fixture {
    let mut source = Fixture::new(&[90_000, 300_000]);
    source.available = (0..rates.len() as i32).collect();
    source.published = rates.len() as i32 - 1;
    source.temporal_seen = 0;
    for (layer, rate) in rates.iter().enumerate() {
        source.rates[layer][0] = *rate;
    }
    source
}

// One shared capacity bounds the sum of both RIDs; deactivating the high RID
// clears its limitation; a live encoder restricted within its sole RID stays
// limited until its ceiling fits.
#[test]
fn simulcast_shares_capacity_and_tracks_demand() {
    let mut source = measured(&[90_000, 300_000]);
    let mut publication =
        PublicationAllocator::new(CodecKind::Vp8, 2, 1_000_000).expect("publication");
    publication.set_connected(&source).expect("connect");
    publication
        .set_connected(&source)
        .expect("idempotent connect");
    assert_eq!(publication.required_active_count(), 2);
    let targets = |publication: &PublicationAllocator| {
        (0..2)
            .map(|layer| publication.track(layer).map(|t| t.target_layer().spatial))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        targets(&publication),
        vec![Some(0), Some(1)],
        "each RID owns exactly its own layer"
    );
    assert!(!publication.limited(&source));
    // A shared capacity update constrains the sum of both RID allocations.
    publication.on_congestion(&source, Congestion::Congested, 1_000);
    let used: i64 = (0..2)
        .map(|layer| publication.bandwidth_requested(&source, layer))
        .sum();
    assert!(
        used <= 1_000,
        "simulcast allocation exceeded shared capacity: {used}"
    );
    assert!(publication.limited(&source));
    assert_eq!(
        publication.lowest_layer_budget(&source),
        1_000,
        "paused higher output kept the lowest budget"
    );
    publication.set_congestion(Congestion::None);
    assert!(
        publication.limited(&source),
        "clear feedback hid the committed allocation deficit"
    );
    publication
        .set_active_count(&source, 1)
        .expect("deactivate high");
    assert_eq!(publication.required_active_count(), 1);
    publication.set_channel_capacity(&source, 1_000_000);
    publication.set_congestion(Congestion::None);
    publication.reconcile(&source);
    assert_eq!(
        targets(&publication),
        vec![Some(0), Some(-1)],
        "inactive high RID still allocated"
    );
    assert!(
        !publication.limited(&source),
        "stayed limited after the requested low output recovered"
    );
    // A live encoder constrained within its sole requested RID.
    source.controlled = true;
    publication.set_channel_capacity(&source, 80_000);
    assert!(
        publication.limited(&source),
        "hid the encoder's unmet 90 kbps ceiling"
    );
    publication.set_channel_capacity(&source, 1_000_000);
    assert!(
        !publication.limited(&source),
        "kept a limitation after the ceiling was satisfied"
    );
    publication
        .set_active_count(&source, 2)
        .expect("reactivate");
    assert_eq!(targets(&publication), vec![Some(0), Some(1)]);
    // Packets reach only their own RID.
    let now = Instant::now();
    let high = InboundPacket::new(CodecKind::Vp8, 1, 5, 0, &[0x10, 0x00], now).marker(true);
    assert_eq!(publication.forward(&high).map(|(rid, _)| rid), Some(1));
    assert_eq!(publication.keyframe_requests(), vec![0]);
    publication.close();
    assert!(publication.forward(&high).is_none() && publication.keyframe_requests().is_empty());
}

#[test]
fn bitrate_goal_combines_transitions_and_encoder_demand() {
    let mut source = measured(&[90_000, 300_000]);
    let mut publication =
        PublicationAllocator::new(CodecKind::Vp8, 2, 1_000_000).expect("publication");
    assert_eq!(
        publication.bitrate_goal(&source),
        BitrateGoal::default(),
        "unattached"
    );
    publication.set_connected(&source).expect("connect");
    assert_eq!(
        publication.bitrate_goal(&source),
        bitrate_goal(390_000, 0),
        "healthy publication pads nothing"
    );
    publication.on_congestion(&source, Congestion::Congested, 100_000);
    let low =
        InboundPacket::new(CodecKind::Vp8, 0, 1, 0, &[0x10, 0x00], Instant::now()).marker(true);
    publication.forward(&low).expect("low keyframe");
    // The high RID is paused; its layer is the next transition.
    let goal = publication.bitrate_goal(&source);
    assert_eq!(goal, bitrate_goal(90_000, 300_000), "{goal:?}");
    // A rate-controlled lowest encoder with a larger unmet ceiling dominates.
    source.ceilings = vec![900_000, 300_000];
    source.controlled = true;
    assert_eq!(publication.additional_probe_demand(&source), 810_000);
    assert_eq!(
        publication.bitrate_goal(&source),
        bitrate_goal(90_000, 810_000)
    );
}
