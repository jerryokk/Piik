//! One forwarded packet onto a str0m send stream, and its bounded admission.

use std::time::{Duration, Instant};

use piik_media_forwarding::{Forwarded, Source, Vp8Rewrite};
use str0m::Rtc;
use str0m::media::{Mid, Pt, Rid};
use str0m::rtp::{RtpWrite, StreamTx, Vp8Descriptor};

use crate::{EgressStats, SourcePacket};

/// Leaky-bucket drain target: a stale low estimate must not trap already
/// encoded frames for seconds. Not a network latency promise.
pub const DRAIN_BOUND: Duration = Duration::from_millis(200);
/// Pion's 1.5 pacing headroom over the committed budget.
const PACING_HEADROOM_PCT: u64 = 150;
const RTP_HEADER_BYTES: u64 = 12;

/// The negotiated identity of one outgoing encoding.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Stream {
    pub mid: Mid,
    pub rid: Option<Rid>,
    pub pt: Pt,
    pub nackable: bool,
}

impl Stream {
    pub fn with_tx<R>(&self, rtc: &mut Rtc, f: impl FnOnce(&mut StreamTx) -> R) -> Option<R> {
        rtc.direct_api().stream_tx_by_mid(self.mid, self.rid).map(f)
    }

    /// Whether a new frame may enter this stream's send queue. str0m samples
    /// the queue at each pacer update; a frame is refused when the queue would
    /// take longer than [`DRAIN_BOUND`] to drain at the paced budget, when its
    /// oldest packet already waited that long, or at the packet bound.
    pub fn admits(&self, rtc: &mut Rtc, budget: i64, max_packets: usize) -> bool {
        let rate = u64::try_from(budget.max(1)).unwrap_or(1) * PACING_HEADROOM_PCT / 100;
        let drain_bytes = rate * DRAIN_BOUND.as_millis() as u64 / 8_000;
        self.with_tx(rtc, |tx| {
            let Some(info) = tx.queue_info() else {
                return true;
            };
            if info.packet_count() == 0 {
                return true;
            }
            // This is a cached pacer snapshot, not the live queue. Aging it
            // with the caller's clock can reject frames after it has drained.
            let waited = info.first_unsent().is_some_and(|first| {
                info.created_at().saturating_duration_since(first) >= DRAIN_BOUND
            });
            info.packet_count() < max_packets && (info.byte_size() as u64) < drain_bytes && !waited
        })
        .unwrap_or(true)
    }

    /// Writes one forwarded packet. Returns false when the stream is absent or
    /// a VP8 rewrite does not fit the payload's descriptor.
    pub fn write(
        &self,
        rtc: &mut Rtc,
        source: &Source,
        packet: &SourcePacket,
        forwarded: &Forwarded,
        now: Instant,
        stats: &mut EgressStats,
    ) -> bool {
        let wallclock = wallclock(source, packet, now);
        let mut write = RtpWrite::new(
            self.pt,
            forwarded.ext_seq.into(),
            forwarded.ext_timestamp as u32,
            wallclock,
            packet.payload.clone(),
        )
        .marker(forwarded.marker)
        .nackable(self.nackable);
        if let Some(rewrite) = forwarded.vp8 {
            let Some(patch) = vp8_patch(&packet.payload, &rewrite) else {
                return false;
            };
            write = write.vp8_patch(patch);
        }
        if self.with_tx(rtc, |tx| tx.write_rtp(write)).is_none() {
            return false;
        }
        stats.packets += 1;
        stats.rtp_bytes += RTP_HEADER_BYTES + packet.payload.len() as u64;
        true
    }
}

/// `RtpWrite.wallclock` is the packet's capture instant on the media core's
/// clock: the source's RTP/NTP correlation projected locally. str0m derives
/// sender reports from it, so it must not be in the future.
pub(crate) fn wallclock(source: &Source, packet: &SourcePacket, now: Instant) -> Instant {
    let layer = usize::try_from(packet.info.layer).unwrap_or(0);
    packet
        .info
        .reference
        .or_else(|| source.reference_time(layer, packet.info.ext_timestamp as u32))
        .and_then(|captured| source.local_time(layer, captured))
        .unwrap_or(packet.info.arrival)
        .min(now)
}

/// Converts the forwarder's descriptor values to str0m's validated patch.
fn vp8_patch(payload: &[u8], rewrite: &Vp8Rewrite) -> Option<str0m::rtp::Vp8Patch> {
    let descriptor = Vp8Descriptor::parse(payload).ok()?;
    let mut builder = descriptor.patch();
    if let Some(id) = rewrite.picture_id {
        builder = builder.picture_id(id);
    }
    if let Some(tl0) = rewrite.tl0_pic_idx {
        builder = builder.tl0_pic_idx(tl0);
    }
    if let Some(key) = rewrite.key_idx {
        builder = builder.key_idx(key);
    }
    builder.build().ok()
}
