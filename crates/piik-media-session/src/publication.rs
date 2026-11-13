//! Native Host to SFU simulcast over one connection. One RID per source
//! layer, one shared estimate.

use std::collections::VecDeque;
use std::time::Instant;

use piik_media_forwarding::{
    BitrateGoal, Congestion, PublicationAllocator, Source, publication_rid,
};
use str0m::bwe::{Bitrate, BweKind};
use str0m::media::{Mid, Pt, Rid};
use str0m::{Event, Rtc};

use crate::probe::{EstimateChange, Meter, ProbeDriver, bitrate, classify};
use crate::transport::PAUSED_MIN_WAIT;
use crate::write::Stream;
use crate::{EgressStats, Error, SourcePacket, UpstreamKeyframe};

/// Bounded history of whole-frame admission drops per RID.
const SHIFT_HISTORY: usize = 64;

#[derive(Clone, Copy, Debug)]
pub struct PublicationOptions {
    /// Negotiated send-only video media whose simulcast send layers are the
    /// source layers' RIDs (`publication_rid`). str0m declares one send
    /// stream per RID and carries the RID header extension until the
    /// receiver acknowledges each SSRC.
    pub mid: Mid,
    pub pt: Pt,
    pub initial_bitrate: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PublicationCounters {
    /// Completed frames written per RID.
    pub frames: Vec<u64>,
    pub egress: EgressStats,
    pub target_bitrate: i64,
    pub observed: bool,
    pub limited: bool,
}

/// Output sequence correction for frames refused by bounded admission. The
/// RID's forwarder never saw them, so later packets close their gap instead
/// of leaving an unrepairable hole on the wire.
#[derive(Debug, Default)]
struct SeqShift {
    /// (first source sequence after a drop, cumulative packets dropped)
    steps: VecDeque<(u64, u64)>,
    frame: Option<u64>,
    first: u64,
    count: u64,
}

impl SeqShift {
    fn shift(&self, source_seq: u64) -> u64 {
        self.steps
            .iter()
            .rev()
            .find(|(start, _)| *start <= source_seq)
            .map_or(0, |(_, total)| *total)
    }

    fn drop_packet(&mut self, timestamp: u64, source_seq: u64) {
        if self.frame != Some(timestamp) {
            self.frame = Some(timestamp);
            self.first = source_seq;
            self.count = 0;
        }
        self.count += 1;
        let total = self.steps.back().map_or(0, |(_, total)| *total) + 1;
        if self.steps.len() == SHIFT_HISTORY {
            self.steps.pop_front();
        }
        self.steps.push_back((source_seq + 1, total));
    }
}

#[derive(Debug)]
struct Track {
    stream: Stream,
    frames: u64,
    last_frame: Option<u64>,
    refused: Option<u64>,
    shift: SeqShift,
}

#[derive(Debug)]
pub struct SimulcastSend {
    tracks: Vec<Track>,
    allocator: PublicationAllocator,
    estimates: u32,
    observed: bool,
    probe: ProbeDriver,
    stats: EgressStats,
    closed: bool,
    initial_bitrate: i64,
    last_write: Option<Instant>,
    paused_since: Option<Instant>,
    meter: Meter,
    now: Option<Instant>,
}

impl SimulcastSend {
    pub fn new(options: PublicationOptions, source: &Source) -> Result<Self, Error> {
        if source.is_closed() || options.initial_bitrate <= 0 {
            return Err(Error::Invalid);
        }
        let count = source.layer_count();
        let allocator =
            PublicationAllocator::new(source.codec().kind, count, options.initial_bitrate)?;
        let tracks = (0..count)
            .map(|layer| {
                let rid = publication_rid(layer, count);
                Track {
                    stream: Stream {
                        mid: options.mid,
                        rid: (!rid.is_empty()).then(|| Rid::from(rid)),
                        pt: options.pt,
                        nackable: true,
                    },
                    frames: 0,
                    last_frame: None,
                    refused: None,
                    shift: SeqShift::default(),
                }
            })
            .collect();
        Ok(Self {
            tracks,
            allocator,
            estimates: 0,
            observed: false,
            probe: ProbeDriver::default(),
            stats: EgressStats::default(),
            closed: false,
            initial_bitrate: options.initial_bitrate,
            last_write: None,
            paused_since: None,
            meter: Meter::default(),
            now: None,
        })
    }

    /// Timer path. When every requested RID is bandwidth-paused nothing is
    /// sent, so no feedback can raise the estimate and an unchanged probe goal
    /// starts no probe. After the paused minimum wait without media the
    /// estimator restarts from the initial bitrate (the allocator rule).
    pub fn poll(&mut self, rtc: &mut Rtc, source: &Source, now: Instant) {
        self.now = Some(now);
        if !self.closed {
            self.probe.poll(rtc, self.allocator.video_budget(), now);
        }
        let active = self.allocator.required_active_count();
        let paused = active > 0
            && (0..active).all(|layer| {
                self.allocator
                    .track(layer)
                    .is_some_and(|track| !track.target_layer().is_valid())
            });
        if self.closed || !paused || self.allocator.video_budget() >= self.initial_bitrate {
            self.paused_since = None;
            return;
        }
        let since = *self.paused_since.get_or_insert(now);
        let waited = |at: Instant| now.saturating_duration_since(at) >= PAUSED_MIN_WAIT;
        if waited(since) && self.last_write.is_none_or(waited) {
            self.paused_since = None;
            self.estimates = 0;
            rtc.bwe().reset(bitrate(self.initial_bitrate));
            self.allocator
                .set_channel_capacity(source, self.initial_bitrate);
            self.refresh_goal(rtc, source);
        }
    }

    /// Earliest time [`Self::poll`] can act.
    pub fn poll_deadline(&self) -> Option<Instant> {
        let paused = self.paused_since.map(|since| since + PAUSED_MIN_WAIT);
        [paused, self.probe.deadline()].into_iter().flatten().min()
    }

    pub fn mid(&self) -> Mid {
        self.tracks[0].stream.mid
    }

    pub fn allocator(&self) -> &PublicationAllocator {
        &self.allocator
    }

    pub fn is_closed(&self) -> bool {
        self.closed
    }

    pub fn rid(&self, layer: usize) -> Option<Rid> {
        self.tracks.get(layer).and_then(|track| track.stream.rid)
    }

    pub fn set_connected(&mut self, rtc: &mut Rtc, source: &Source) -> Result<(), Error> {
        if self.closed {
            return Err(Error::Closed);
        }
        self.allocator.set_connected(source)?;
        self.refresh_goal(rtc, source);
        Ok(())
    }

    pub fn set_active_count(
        &mut self,
        rtc: &mut Rtc,
        source: &Source,
        count: usize,
    ) -> Result<(), Error> {
        if self.closed {
            return Err(Error::Closed);
        }
        self.allocator.set_active_count(source, count)?;
        self.refresh_goal(rtc, source);
        Ok(())
    }

    pub fn required_active_count(&self) -> usize {
        self.allocator.required_active_count()
    }

    pub fn video_budget(&self) -> i64 {
        self.allocator.video_budget()
    }

    pub fn lowest_layer_budget(&self, source: &Source) -> i64 {
        self.allocator.lowest_layer_budget(source)
    }

    pub fn bitrate_goal(&self) -> Option<BitrateGoal> {
        self.probe.goal()
    }

    pub fn counters(&self, source: &Source) -> PublicationCounters {
        PublicationCounters {
            frames: self.tracks.iter().map(|track| track.frames).collect(),
            egress: self.stats,
            target_bitrate: self.allocator.video_budget(),
            observed: self.observed,
            limited: self.allocator.limited(source),
        }
    }

    /// Source events (layers, bitrates, discovery) and timers. Returns the
    /// layers whose RID still waits for a keyframe.
    pub fn reconcile(&mut self, rtc: &mut Rtc, source: &Source) -> Vec<UpstreamKeyframe> {
        if self.closed {
            return Vec::new();
        }
        self.allocator.reconcile(source);
        self.refresh_goal(rtc, source);
        self.pending_keyframes()
    }

    fn pending_keyframes(&self) -> Vec<UpstreamKeyframe> {
        self.allocator
            .keyframe_requests()
            .into_iter()
            .filter_map(|layer| usize::try_from(layer).ok())
            .map(|layer| UpstreamKeyframe {
                layer,
                force: false,
            })
            .collect()
    }

    pub fn handle_event(
        &mut self,
        rtc: &mut Rtc,
        source: &Source,
        event: &Event,
    ) -> Option<UpstreamKeyframe> {
        if self.closed {
            return None;
        }
        match event {
            Event::KeyframeRequest(request) if request.mid == self.mid() => self
                .tracks
                .iter()
                .position(|track| track.stream.rid == request.rid)
                .map(|layer| UpstreamKeyframe {
                    layer,
                    force: false,
                }),
            Event::EgressBitrateEstimate(BweKind::Twcc { estimate, .. }) => {
                self.on_estimate(rtc, source, *estimate);
                None
            }
            Event::MediaEgressStats(stats) if stats.mid == self.mid() => {
                if stats.rtt.is_some() {
                    self.observed = true;
                }
                None
            }
            _ => None,
        }
    }

    /// Applies one estimate to the shared committed capacity (see
    /// [`classify`]). The first event is only the configured start bitrate.
    fn on_estimate(&mut self, rtc: &mut Rtc, source: &Source, estimate: Bitrate) {
        self.estimates = self.estimates.saturating_add(1);
        let estimate = i64::try_from(estimate.as_u64()).unwrap_or(i64::MAX);
        if self.estimates == 1 {
            self.allocator.reconcile(source);
        } else {
            self.observed = true;
            let usage = self.now.map_or(0, |now| self.meter.rate(now));
            let committed = self.allocator.video_budget();
            match classify(committed, estimate, usage, self.probe.is_probing()) {
                EstimateChange::Raise(capacity) => {
                    self.allocator.set_congestion(Congestion::None);
                    self.allocator.probe_started();
                    self.allocator.probe_finished(source, true, capacity);
                }
                EstimateChange::Congested(capacity) => {
                    self.allocator
                        .on_congestion(source, Congestion::Congested, capacity);
                }
                EstimateChange::Hold => self.allocator.set_congestion(Congestion::None),
            }
        }
        self.refresh_goal(rtc, source);
    }

    fn refresh_goal(&mut self, rtc: &mut Rtc, source: &Source) {
        if self.closed {
            return;
        }
        let mut goal = self.allocator.bitrate_goal(source);
        // Native encodings have known ceilings before the first rate window.
        // Keep that demand while a RID is dormant or bandwidth-paused; measured
        // traffic is not the upload capacity we need str0m to discover.
        let desired: i64 = source
            .formats()
            .iter()
            .take(self.required_active_count())
            .map(|format| i64::from(format.bitrate))
            .sum();
        goal.desired = goal.desired.max(desired);
        if desired > self.video_budget() {
            // Bwe::set_bitrate sustains this with padding, capped by its own
            // estimate, until media can replace it. It does not reserve capacity.
            goal.minimum = desired;
        }
        self.probe.apply(rtc, goal);
    }

    /// Routes one source packet to its RID. A frame exceeding the RID send
    /// queue's drain bound is dropped whole and its layer re-requested.
    pub fn forward(
        &mut self,
        rtc: &mut Rtc,
        source: &Source,
        packet: &SourcePacket,
        now: Instant,
    ) -> Option<UpstreamKeyframe> {
        if self.closed {
            return None;
        }
        let layer = usize::try_from(packet.info.layer).ok()?;
        let budget = self.allocator.video_budget();
        let selected = self
            .allocator
            .track(layer)
            .is_some_and(|forwarder| forwarder.target_layer().is_valid());
        let track = self.tracks.get_mut(layer)?;
        let timestamp = packet.info.ext_timestamp;
        if track.refused == Some(timestamp) {
            track.shift.drop_packet(timestamp, packet.info.ext_seq);
            return None;
        }
        // Closing a refused frame's RTP gap does not restore the decoder's
        // reference pictures. Keep this RID fenced until a recovery frame.
        if track.refused.is_some() && !packet.info.keyframe {
            track.refused = Some(timestamp);
            track.shift.drop_packet(timestamp, packet.info.ext_seq);
            return Some(UpstreamKeyframe {
                layer,
                force: false,
            });
        }
        if selected && track.last_frame != Some(timestamp) && !packet.info.padding_only {
            track.last_frame = Some(timestamp);
            if !track.stream.admits(rtc, budget, source.max_packets()) {
                track.refused = Some(timestamp);
                track.shift.drop_packet(timestamp, packet.info.ext_seq);
                self.stats.dropped_frames += 1;
                return Some(UpstreamKeyframe {
                    layer,
                    force: false,
                });
            }
            track.refused = None;
        }
        let (layer, mut forwarded) = self.allocator.forward(&packet.info)?;
        let track = &mut self.tracks[layer];
        forwarded.ext_seq = forwarded
            .ext_seq
            .wrapping_sub(track.shift.shift(packet.info.ext_seq));
        let written = track
            .stream
            .write(rtc, source, packet, &forwarded, now, &mut self.stats);
        if written {
            self.last_write = Some(now);
            self.meter.record(now, 12 + packet.payload.len() as u64);
        }
        self.now = Some(now);
        if written && forwarded.marker && !packet.payload.is_empty() {
            track.frames += 1;
        }
        None
    }

    /// Keeps every RID, counter and the estimate; forwarding re-acquires the
    /// next source. Returns forced recovery for every layer.
    pub fn replace_source(
        &mut self,
        rtc: &mut Rtc,
        current: &Source,
        next: &Source,
    ) -> Result<Vec<UpstreamKeyframe>, Error> {
        if self.closed {
            return Err(Error::Closed);
        }
        if !current.compatible_with(next) {
            return Err(Error::Incompatible);
        }
        self.allocator.replace_source(next)?;
        for track in &mut self.tracks {
            track.last_frame = None;
            track.refused = None;
        }
        self.refresh_goal(rtc, next);
        if self.allocator.required_active_count() == 0 {
            return Ok(Vec::new());
        }
        Ok((0..next.layer_count())
            .map(|layer| UpstreamKeyframe { layer, force: true })
            .collect())
    }

    pub fn close(&mut self, rtc: &mut Rtc) {
        if self.closed {
            return;
        }
        self.closed = true;
        self.allocator.close();
        self.probe.clear(rtc);
    }
}
