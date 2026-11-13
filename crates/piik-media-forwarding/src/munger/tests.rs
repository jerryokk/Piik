use super::*;

fn vp8(picture_id: u16, tl0: u8, key: u8) -> Vp8Descriptor {
    // X, I/L/K plus TID: 15-bit PictureID, TL0PICIDX, TID=0 KEYIDX.
    let payload = [
        0x90,
        0xf0,
        0x80 | (picture_id >> 8) as u8,
        picture_id as u8,
        tl0,
        key & 0x1f,
        0x00,
    ];
    Vp8Descriptor::parse(&payload).expect("descriptor")
}

#[test]
fn first_sequence_is_below_one_rollover_for_long_running_sources() {
    let mut munger = RtpMunger::default();
    // A long-running source: ROC 7 and a 32-bit timestamp past its first wrap.
    let ext_seq = 7 * SEQ_CYCLE + 65_530;
    let ext_ts = 3 * TS_CYCLE + 1234;
    assert!(
        munger.translate(ext_seq, ext_ts, false).is_none(),
        "unstarted munger forwarded"
    );
    munger.start(ext_seq, ext_ts);
    let first = munger.translate(ext_seq, ext_ts, true).expect("first");
    assert!(
        first.ext_seq < SEQ_CYCLE && first.ext_timestamp < TS_CYCLE,
        "{first:?}"
    );
    // Wrap of the 16-bit sequence continues in extended space.
    let mut last = first;
    for step in 1..20u64 {
        let next = munger
            .translate(ext_seq + step, ext_ts + step * 3000, true)
            .expect("next");
        assert_eq!(next.ext_seq, last.ext_seq + 1);
        assert_eq!(next.ext_timestamp, last.ext_timestamp + 3000);
        last = next;
    }
    assert!(last.ext_seq >= SEQ_CYCLE, "sequence did not cross its wrap");
}

#[test]
fn reordering_duplicates_and_stale_packets() {
    let mut munger = RtpMunger::default();
    munger.start(1000, 90_000);
    let base = munger
        .translate(1000, 90_000, false)
        .expect("first")
        .ext_seq;
    assert!(munger.translate(1000, 90_000, false).is_none(), "duplicate");
    assert_eq!(
        munger.translate(1002, 90_000, true).map(|t| t.ext_seq),
        Some(base + 2)
    );
    assert_eq!(
        munger.translate(1001, 90_000, false).map(|t| t.ext_seq),
        Some(base + 1),
        "late packet"
    );
    assert!(
        munger.translate(999, 90_000, false).is_none(),
        "before start"
    );
    for seq in 1003..(1003 + MAX_REORDER) {
        munger.translate(seq, 90_000, true);
    }
    assert!(
        munger.translate(1001, 90_000, false).is_none(),
        "beyond reorder bound"
    );
}

#[test]
fn dropped_packets_and_padding_keep_output_contiguous() {
    let mut munger = RtpMunger::default();
    munger.start(10, 0);
    let first = munger.translate(10, 0, false).expect("first").ext_seq;
    munger.drop_packet(11);
    assert_eq!(
        munger.translate(12, 3000, true).map(|t| t.ext_seq),
        Some(first + 1)
    );
    assert!(
        munger.translate(11, 0, false).is_none(),
        "dropped position revived"
    );
    let padding = munger.padding(3, false);
    assert_eq!(
        padding
            .iter()
            .map(|t| (t.ext_seq, t.ext_timestamp))
            .collect::<Vec<_>>(),
        vec![(first + 2, 3000), (first + 3, 3000), (first + 4, 3000)]
    );
    assert_eq!(
        munger.translate(13, 6000, false).map(|t| t.ext_seq),
        Some(first + 5)
    );
    assert!(
        munger.padding(1, false).is_empty(),
        "padding inside a frame"
    );
    assert_eq!(munger.padding(1, true).len(), 1, "forced padding");
    assert_eq!(
        munger.translate(14, 6000, true).map(|t| t.ext_seq),
        Some(first + 7)
    );
    // A late packet before the padding keeps its original offset.
    assert_eq!(
        munger.translate(12, 3000, true).map(|t| t.ext_seq),
        Some(first + 1)
    );
}

#[test]
fn switch_continues_sequence_and_timestamp_across_input_wraps() {
    let mut munger = RtpMunger::default();
    munger.start(65_534, u64::from(u32::MAX) - 2999);
    munger
        .translate(65_534, u64::from(u32::MAX) - 2999, true)
        .expect("first");
    let last = munger
        .translate(65_535, u64::from(u32::MAX) + 1, true)
        .expect("wrap");
    assert_eq!(
        last.ext_timestamp,
        u64::from(u32::MAX) + 1,
        "timestamp crossed its wrap"
    );
    // The new layer's input is far behind the old one in both counters.
    let out_ts = last.ext_timestamp + 3000;
    munger.switch(5, 100, out_ts);
    let switched = munger.translate(5, 100, false).expect("switched");
    assert_eq!(
        (switched.ext_seq, switched.ext_timestamp),
        (last.ext_seq + 1, out_ts)
    );
    let next = munger.translate(6, 3100, true).expect("next");
    assert_eq!(
        (next.ext_seq, next.ext_timestamp),
        (last.ext_seq + 2, out_ts + 3000)
    );
    assert!(
        munger.translate(4, 0, true).is_none(),
        "previous input survived the switch"
    );
    // Switching to an input far ahead also stays contiguous.
    munger.switch(9 * SEQ_CYCLE, 5 * TS_CYCLE, next.ext_timestamp + 1);
    let ahead = munger
        .translate(9 * SEQ_CYCLE, 5 * TS_CYCLE, true)
        .expect("ahead");
    assert_eq!(
        (ahead.ext_seq, ahead.ext_timestamp),
        (next.ext_seq + 1, next.ext_timestamp + 1)
    );
}

#[test]
fn vp8_identities_continue_across_switches() {
    let mut munger = Vp8Munger::default();
    let descriptor = vp8(0x7ffd, 250, 30);
    assert!(munger.rewrite(&descriptor, false).is_none(), "unstarted");
    munger.start(&descriptor);
    let mut outputs = Vec::new();
    for index in 0..4u16 {
        let input = vp8(
            (0x7ffd + index) & 0x7fff,
            250u8.wrapping_add(index as u8),
            30,
        );
        outputs.push(munger.rewrite(&input, false).expect("rewrite"));
    }
    let ids: Vec<_> = outputs.iter().map(|r| r.picture_id.expect("id")).collect();
    assert_eq!(
        ids,
        vec![0x7ffd, 0x7ffe, 0x7fff, 0],
        "PictureID did not wrap at 15 bits"
    );
    let tl0: Vec<_> = outputs
        .iter()
        .map(|r| r.tl0_pic_idx.expect("tl0"))
        .collect();
    assert_eq!(tl0, vec![250, 251, 252, 253]);
    // A different layer has unrelated identities; output continues at +1.
    munger.switch(&vp8(17, 3, 9));
    let switched = munger.rewrite(&vp8(17, 3, 9), false).expect("switched");
    assert_eq!(
        (switched.picture_id, switched.tl0_pic_idx, switched.key_idx),
        (Some(1), Some(254), Some(31))
    );
    let next = munger.rewrite(&vp8(18, 4, 9), false).expect("next");
    assert_eq!(
        (next.picture_id, next.tl0_pic_idx, next.key_idx),
        (Some(2), Some(255), Some(31))
    );
    // A dropped temporal picture leaves no PictureID gap.
    assert!(munger.rewrite(&vp8(19, 4, 9), true).is_none());
    assert!(
        munger.rewrite(&vp8(19, 4, 9), false).is_none(),
        "dropped picture's later packet"
    );
    let after = munger.rewrite(&vp8(20, 5, 9), false).expect("after drop");
    assert_eq!((after.picture_id, after.tl0_pic_idx), (Some(3), Some(0)));
    // A late packet of an earlier picture keeps its own output identity.
    assert_eq!(
        munger
            .rewrite(&vp8(18, 4, 9), false)
            .and_then(|r| r.picture_id),
        Some(2)
    );
    // Input wrap after a switch.
    munger.switch(&vp8(0x7fff, 0, 0));
    assert_eq!(
        munger
            .rewrite(&vp8(0x7fff, 0, 0), false)
            .and_then(|r| r.picture_id),
        Some(4)
    );
    assert_eq!(
        munger
            .rewrite(&vp8(0, 1, 0), false)
            .and_then(|r| r.picture_id),
        Some(5)
    );
}

#[test]
fn vp8_seven_bit_identities_keep_their_width() {
    let short = |id: u8| Vp8Descriptor::parse(&[0x90, 0x80, id & 0x7f, 0x00]).expect("short");
    let mut munger = Vp8Munger::default();
    munger.start(&vp8(0x7fff, 0, 0));
    munger.rewrite(&vp8(0x7fff, 0, 0), false);
    munger.switch(&short(126));
    let ids: Vec<_> = [126u8, 127, 0]
        .into_iter()
        .map(|id| munger.rewrite(&short(id), false).and_then(|r| r.picture_id))
        .collect();
    assert_eq!(ids, vec![Some(0), Some(1), Some(2)]);
}
