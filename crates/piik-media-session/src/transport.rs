//! One outbound video stream on a subscriber connection, with str0m owning
//! send-side bandwidth estimation and pacing.

use std::time::{Duration, Instant};

use piik_media_forwarding::{
    BitrateGoal, CommittedBudget, Congestion, OutputAllocator, OutputState, Source,
};
use str0m::bwe::{Bitrate, BweKind};
use str0m::media::{Mid, Pt};
use str0m::{Event, Rtc};

use crate::probe::{CongestionEvidence, EstimateChange, Meter, ProbeDriver, bitrate, classify};
use crate::write::Stream;
use crate::{EgressStats, Error, SourcePacket, UpstreamKeyframe};

/// The allocator's paused minimum wait before the estimator restarts.
pub const PAUSED_MIN_WAIT: Duration = Duration::from_secs(5);
/// An estimate this far below the committed capacity is a collapse, not the
/// estimator following an application-limited source.
const COLLAPSED_PCT: i64 = 50;

#[derive(Clone, Copy, Debug)]
pub struct TransportOptions {
    /// Negotiated send-only video media with one declared send stream.
    pub mid: Mid,
    /// Negotiated payload type of the source codec on this connection.
    pub pt: Pt,
    /// Initial estimate; the `Rtc` should be built with the same BWE start.
    pub initial_bitrate: i64,
    /// The source forwards another owner's encoding (a relay's downstream
    /// edge) instead of a locally encoded one it can resize on demand.
    pub forwarded: bool,
}

/// The frame bounded admission is currently refusing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Frame {
    layer: i32,
    timestamp: u64,
}

#[derive(Debug)]
pub struct VideoSend {
    stream: Stream,
    output: OutputAllocator,
    attached: bool,
    observed: bool,
    estimates: u32,
    probe: ProbeDriver,
    last_frame: Option<Frame>,
    refused: Option<Frame>,
    stats: EgressStats,
    initial_bitrate: i64,
    last_write: Option<Instant>,
    paused_since: Option<Instant>,
    committed: CommittedBudget,
    meter: Meter,
    now: Option<Instant>,
    last_estimate: i64,
    /// Overuse evidence from refusals and remote loss (see
    /// [`CongestionEvidence`]).
    congestion: CongestionEvidence,
    /// See [`TransportOptions::forwarded`].
    forwarded: bool,
}

impl VideoSend {
    pub fn new(options: TransportOptions, source: &Source) -> Result<Self, Error> {
        let kind = source.codec().kind;
        if !kind.is_video() || options.initial_bitrate <= 0 || source.is_closed() {
            return Err(Error::Invalid);
        }
        let mut output = OutputAllocator::new(kind, options.initial_bitrate);
        // Bootstrap until this connection's own feedback exists.
        output.set_feedback_observed(Some(false));
        output.set_max_spatial_layer(source, source.layer_count() as i32 - 1);
        Ok(Self {
            stream: Stream {
                mid: options.mid,
                rid: None,
                pt: options.pt,
                nackable: true,
            },
            output,
            attached: false,
            observed: false,
            estimates: 0,
            probe: ProbeDriver::default(),
            last_frame: None,
            refused: None,
            stats: EgressStats::default(),
            initial_bitrate: options.initial_bitrate,
            last_write: None,
            paused_since: None,
            committed: CommittedBudget::new(options.initial_bitrate),
            meter: Meter::default(),
            now: None,
            last_estimate: options.initial_bitrate,
            congestion: CongestionEvidence::default(),
            forwarded: options.forwarded,
        })
    }

    pub fn mid(&self) -> Mid {
        self.stream.mid
    }

    pub fn output(&self) -> &OutputAllocator {
        &self.output
    }

    pub fn is_closed(&self) -> bool {
        self.output.is_closed()
    }

    /// The connection is up; forwarding begins acquiring `source`. Idempotent.
    pub fn set_connected(&mut self, rtc: &mut Rtc, source: &Source) -> Result<(), Error> {
        if self.is_closed() {
            return Err(Error::Closed);
        }
        if !self.attached {
            self.attached = true;
            self.output.reconcile(source);
            self.refresh_goal(rtc, source);
        }
        Ok(())
    }

    pub fn is_attached(&self) -> bool {
        self.attached && !self.is_closed()
    }

    /// Committed TWCC estimate and whether this connection's own feedback
    /// produced it.
    pub fn target_bitrate(&self) -> (i64, bool) {
        (self.output.budget(), self.observed)
    }

    /// Latest raw str0m TWCC estimate (diagnostics; allocation reads the
    /// committed value).
    pub fn estimate(&self) -> i64 {
        self.last_estimate
    }

    pub fn state(&self, source: &Source) -> OutputState {
        self.output.state(source)
    }

    pub fn bandwidth_limited(&self, source: &Source) -> bool {
        self.output.bandwidth_limited(source)
    }

    pub fn required_active_count(&self, source: &Source) -> usize {
        self.output.required_active_count(source, self.observed)
    }

    pub fn egress(&self) -> EgressStats {
        self.stats
    }

    /// Last rates handed to `Bwe::set_bitrate`.
    pub fn bitrate_goal(&self) -> Option<BitrateGoal> {
        self.probe.goal()
    }

    pub fn set_max_spatial_layer(&mut self, rtc: &mut Rtc, source: &Source, layer: i32) {
        self.output.set_max_spatial_layer(source, layer);
        self.refresh_goal(rtc, source);
    }

    pub fn set_max_temporal_layer(&mut self, rtc: &mut Rtc, source: &Source, layer: i32) {
        self.output.set_max_temporal_layer(source, layer);
        self.refresh_goal(rtc, source);
    }

    /// Fences a controlled Native input frame before any of its layers arrive.
    pub fn begin_frame(&mut self) {
        self.output.begin_frame();
    }

    /// After any source change (`SourceChanges::any`), subscription change or
    /// timer. Returns the keyframe the output still waits for, if any.
    pub fn reconcile(&mut self, rtc: &mut Rtc, source: &Source) -> Option<UpstreamKeyframe> {
        if !self.is_attached() {
            return None;
        }
        self.output.reconcile(source);
        self.refresh_goal(rtc, source);
        self.pending_keyframe()
    }

    /// Handles this connection's str0m events. Returns an upstream keyframe
    /// request for a subscriber PLI/FIR.
    pub fn handle_event(
        &mut self,
        rtc: &mut Rtc,
        source: &Source,
        event: &Event,
    ) -> Option<UpstreamKeyframe> {
        if self.is_closed() {
            return None;
        }
        match event {
            Event::KeyframeRequest(request) if request.mid == self.stream.mid => {
                let target = self.output.forwarder().target_layer();
                let current = self.output.forwarder().current_layer();
                let layer = if target.is_valid() { target } else { current };
                usize::try_from(layer.spatial)
                    .ok()
                    .filter(|_| layer.is_valid() && self.attached)
                    .map(|layer| UpstreamKeyframe {
                        layer,
                        force: false,
                    })
            }
            Event::EgressBitrateEstimate(BweKind::Twcc { estimate, .. }) => {
                self.on_estimate(rtc, source, *estimate);
                None
            }
            Event::MediaEgressStats(stats) if stats.mid == self.stream.mid => {
                // A receiver report proves the remote receives this stream, and
                // its loss is the only direct evidence of overuse for an output
                // that cannot resize what it forwards.
                self.congestion.observe_loss(stats.loss);
                if stats.rtt.is_some() && !self.observed {
                    self.set_observed(rtc, source);
                }
                None
            }
            _ => None,
        }
    }

    /// str0m's first TWCC event carries the configured start bitrate; later
    /// events are estimates from this connection's own feedback.
    fn on_estimate(&mut self, rtc: &mut Rtc, source: &Source, estimate: Bitrate) {
        self.estimates = self.estimates.saturating_add(1);
        let estimate = i64::try_from(estimate.as_u64()).unwrap_or(i64::MAX);
        self.last_estimate = estimate;
        if self.estimates > 1 {
            let committed = self.committed.value();
            // A source that follows the budget (a Native capture) can shrink
            // within a frame, so a fading estimate must revoke capacity early.
            // A forwarded raw source has no such knob: revoking its commitment
            // pauses it, which is far costlier than holding it, so it needs
            // reported loss (see `CongestionEvidence`).
            let change = if !self.forwarded {
                let usage = self.now.map_or(0, |now| self.meter.rate(now));
                classify(committed, estimate, usage, self.probe.is_probing())
            } else {
                self.congestion
                    .classify(committed, estimate, self.probe.is_probing())
            };
            let congestion = match change {
                EstimateChange::Raise(capacity) => {
                    self.committed.probe_started();
                    self.committed.probe_finished(true, capacity);
                    Congestion::None
                }
                EstimateChange::Congested(capacity) => {
                    self.committed
                        .on_congestion(Congestion::Congested, capacity);
                    Congestion::Congested
                }
                EstimateChange::Hold => Congestion::None,
            };
            self.output.set_congestion(congestion);
        }
        self.output.set_budget(source, self.committed.value());
        if self.estimates > 1 && !self.observed {
            self.set_observed(rtc, source);
        } else {
            self.refresh_goal(rtc, source);
        }
    }

    fn set_observed(&mut self, rtc: &mut Rtc, source: &Source) {
        self.observed = true;
        self.output.set_feedback_observed(Some(true));
        if self.attached {
            self.output.reconcile(source);
        }
        self.refresh_goal(rtc, source);
    }

    /// Probe demand becomes str0m's `(minimum, desired)`.
    ///
    /// A forwarded source sends at whatever rate its owner produces, which is
    /// often far below the committed capacity (a static screen is a few
    /// hundred kbps). str0m sizes its pacer from its own estimate, and an
    /// application-limited estimate settles at the sent rate; left at the
    /// allocation's demand that would throttle the pacer to what the source
    /// happens to produce right now and stop it from ever carrying a busier
    /// scene. The committed capacity is this output's own measurement of the
    /// link, so `desired` keeps the estimator aimed there.
    ///
    /// A source whose encoder follows the budget (a Native capture) fills the
    /// capacity with real media, so nothing more is needed. A forwarded raw
    /// source cannot: its content rate is whatever its owner produced. There
    /// `minimum` asks str0m to pad up to the commitment, which keeps the
    /// estimate — and with it str0m's pacer — from decaying to the current
    /// content rate. Padding is capped by the estimate, so a genuinely narrow
    /// link still limits it.
    ///
    /// Raising `minimum` to the commitment yields two equal rates, which is
    /// the right shape only when nothing larger is wanted: a pending request
    /// for another layer must keep that headroom visible. `prepare` is the
    /// layer this output could switch to, so it names that demand before any
    /// larger output has been published.
    ///
    /// `minimum` is a *sustain* level, never above what the estimator is
    /// currently willing to carry: str0m clamps `desired` to `minimum`, so a
    /// minimum left at the commitment would hold the pacer there no matter
    /// how far the measurement falls.
    fn refresh_goal(&mut self, rtc: &mut Rtc, source: &Source) {
        if self.is_attached() {
            let mut goal = self.output.bitrate_goal(source);
            if self.observed {
                let committed = self.committed.value();
                if committed > goal.desired {
                    goal.desired = committed;
                }
                let state = self.output.state(source);
                let acquiring = state.prepare > state.target;
                if self.forwarded && !acquiring {
                    goal.minimum = committed.min(self.last_estimate.max(goal.minimum));
                }
            }
            self.probe.apply(rtc, goal);
        }
    }

    fn pending_keyframe(&self) -> Option<UpstreamKeyframe> {
        let layer = usize::try_from(self.output.keyframe_request()?).ok()?;
        Some(UpstreamKeyframe {
            layer,
            force: false,
        })
    }

    /// Forwards one source packet. A frame that would exceed the send queue's
    /// drain bound is dropped whole; the output re-acquires from a keyframe.
    pub fn forward(
        &mut self,
        rtc: &mut Rtc,
        source: &Source,
        packet: &SourcePacket,
        now: Instant,
    ) -> Option<UpstreamKeyframe> {
        if !self.is_attached() {
            return None;
        }
        let frame = Frame {
            layer: packet.info.layer,
            timestamp: packet.info.ext_timestamp,
        };
        if self.refused == Some(frame) {
            return None;
        }
        let forwarder = self.output.forwarder();
        let selected = forwarder.target_layer().spatial == frame.layer
            || forwarder.current_layer().spatial == frame.layer;
        if selected && self.last_frame != Some(frame) && !packet.info.padding_only {
            self.last_frame = Some(frame);
            let admitted = self
                .stream
                .admits(rtc, self.output.budget(), source.max_packets());
            if !admitted {
                self.refused = Some(frame);
                self.stats.dropped_frames += 1;
                // Nothing of this frame entered the output's timeline, so the
                // next keyframe continues it without a sequence gap.
                self.output.resync();
                self.output.reconcile(source);
                return Some(UpstreamKeyframe {
                    layer: frame.layer.max(0) as usize,
                    force: false,
                });
            }
            self.refused = None;
        }
        let before = self.output.state(source).current;
        if let Some(forwarded) = self.output.forward(&packet.info)
            && self
                .stream
                .write(rtc, source, packet, &forwarded, now, &mut self.stats)
        {
            self.last_write = Some(now);
            self.meter.record(now, 12 + packet.payload.len() as u64);
        }
        self.now = Some(now);
        if self.output.state(source).current != before {
            // A completed frame changes the demand the probe goal follows.
            self.refresh_goal(rtc, source);
        }
        self.pending_keyframe()
    }

    /// Moves to a compatible replacement; the connection and its estimate stay.
    /// Returns forced recovery requests for every layer of `next`.
    pub fn replace_source(
        &mut self,
        rtc: &mut Rtc,
        current: &Source,
        next: &Source,
    ) -> Result<Vec<UpstreamKeyframe>, Error> {
        if self.is_closed() {
            return Err(Error::Closed);
        }
        if !current.compatible_with(next) {
            return Err(Error::Incompatible);
        }
        self.output.replace_source(next);
        self.last_frame = None;
        self.refused = None;
        if !self.attached {
            return Ok(Vec::new());
        }
        self.refresh_goal(rtc, next);
        Ok((0..next.layer_count())
            .map(|layer| UpstreamKeyframe { layer, force: true })
            .collect())
    }

    /// Timer path. A bandwidth-paused output sends nothing, so no feedback can
    /// raise the estimate; after the paused minimum wait without media the
    /// estimator restarts from the initial bitrate and allocation tries again.
    pub fn poll(&mut self, rtc: &mut Rtc, source: &Source, now: Instant) {
        if !self.is_attached() {
            return;
        }
        self.now = Some(now);
        self.probe.poll(rtc, self.output.budget(), now);
        // str0m derives its pacer rate from its own estimate, not the
        // committed target. A forwarded output cannot shrink to a
        // collapsed estimate: it would pause and stop producing the feedback
        // that could restore capacity. Re-arming the estimator at the
        // commitment restores pacing at the committed target. It applies only near that
        // collapse — an estimate merely below the commitment is the estimator
        // following the source, and reported loss still revokes capacity
        // through `CongestionEvidence`.
        let collapsed = self.last_estimate * 100 <= self.committed.value() * COLLAPSED_PCT;
        if self.forwarded
            && collapsed
            && !self.congestion.confirmed_loss()
            && self.last_estimate < self.committed.value()
        {
            self.estimates = 0;
            self.paused_since = None;
            rtc.bwe().reset(bitrate(self.committed.value()));
            self.output.set_budget(source, self.committed.value());
            self.refresh_goal(rtc, source);
            return;
        }
        // str0m paces at its own raw estimate, so a held committed budget
        // does not keep a collapsed estimator usable.
        let capacity = self.output.budget().min(self.last_estimate);
        if !self.output.state(source).paused || capacity >= self.initial_bitrate {
            self.paused_since = None;
            return;
        }
        let since = *self.paused_since.get_or_insert(now);
        let waited = |at: Instant| now.saturating_duration_since(at) >= PAUSED_MIN_WAIT;
        if waited(since) && self.last_write.is_none_or(waited) {
            self.paused_since = None;
            self.estimates = 0;
            rtc.bwe().reset(bitrate(self.initial_bitrate));
            self.committed.reset();
            self.committed.set(self.initial_bitrate);
            self.output.set_budget(source, self.initial_bitrate);
            self.refresh_goal(rtc, source);
        }
    }

    /// Earliest time [`Self::poll`] can act.
    pub fn poll_deadline(&self) -> Option<Instant> {
        let paused = self.paused_since.map(|since| since + PAUSED_MIN_WAIT);
        [paused, self.probe.deadline()].into_iter().flatten().min()
    }

    /// Retires forwarding and withdraws probe demand.
    pub fn close(&mut self, rtc: &mut Rtc) {
        if self.is_closed() {
            return;
        }
        self.output.close();
        self.probe.clear(rtc);
    }
}
