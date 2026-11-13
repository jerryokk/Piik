//! One str0m `Rtc` with Piik's lifecycle projection and data channel handles.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use str0m::change::{SdpAnswer, SdpOffer, SdpPendingOffer};
use str0m::channel::{ChannelData, ChannelId};
use str0m::media::{MediaKind, Mid, Rid};
use str0m::net::Transmit;
use str0m::rtp::Ssrc;
use str0m::{Candidate, Event, IceConnectionState, Input, Output, Rtc, RtcConfig, RtcError};

use crate::candidate::{CandidateError, IceCandidateInit, parse_remote};
use crate::channel::{ChannelOp, DataChannel, OpQueue};
use crate::socket::Egress;
use crate::state::PcState;

/// Pion fails a connection that has not connected within its disconnected
/// plus failed timeouts (5 s + 25 s).
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// str0m reports ICE `Disconnected` only after its own consent timeout (15 s
/// for ICE-lite); `failed` follows this much later, so total loss to failure
/// stays near Pion's 30 s.
pub const FAILED_AFTER_DISCONNECTED: Duration = Duration::from_secs(15);

/// Unique for the process lifetime; never reused, so late datagrams and
/// handles for a replaced connection cannot reach its successor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConnId(u64);

impl ConnId {
    fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

impl fmt::Display for ConnId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "conn-{}", self.0)
    }
}

/// What a connection reports to its shard app.
#[derive(Debug)]
pub enum ConnEvent {
    /// The projected peer connection state changed.
    State(PcState),
    ChannelOpen(Arc<DataChannel>),
    ChannelData(Arc<DataChannel>, ChannelData),
    /// Final for this handle; it now reports `is_closed`.
    ChannelClose(Arc<DataChannel>),
    /// Any other str0m event (media, RTCP, BWE, stats).
    Rtc(Box<Event>),
}

pub(crate) enum Polled {
    Transmit(Box<Transmit>),
    Event(ConnEvent),
}

/// One RTP-mode peer connection. Negotiate through [`Connection::accept_offer`]
/// / [`Connection::create_offer`] so remote-description lifecycle checks hold;
/// use [`Connection::rtc`] for media and direct API calls.
pub struct Connection {
    id: ConnId,
    rtc: Rtc,
    egress: Option<Egress>,
    ops: Option<Arc<OpQueue>>,
    channels: HashMap<ChannelId, Arc<DataChannel>>,
    events: VecDeque<ConnEvent>,
    remote_described: bool,
    ice: IceConnectionState,
    dtls_connected: bool,
    /// Latched until ICE connects again, as after an ICE restart.
    failed: bool,
    closed: bool,
    state: PcState,
    /// When the current `connecting` or `disconnected` state began.
    since: Instant,
    /// Latest time handed to this connection.
    now: Instant,
    rtc_timeout: Instant,
}

impl Connection {
    /// Builds the connection in RTP mode; forwarding and the edges both
    /// operate on packets, not frames.
    pub fn new(config: RtcConfig, now: Instant) -> Self {
        Self {
            id: ConnId::next(),
            rtc: config.set_rtp_mode(true).build(now),
            egress: None,
            ops: None,
            channels: HashMap::new(),
            events: VecDeque::new(),
            remote_described: false,
            ice: IceConnectionState::New,
            dtls_connected: false,
            failed: false,
            closed: false,
            state: PcState::New,
            since: now,
            now,
            rtc_timeout: now,
        }
    }

    pub fn id(&self) -> ConnId {
        self.id
    }

    pub fn state(&self) -> PcState {
        self.state
    }

    pub fn rtc(&mut self) -> &mut Rtc {
        &mut self.rtc
    }

    /// Where the tokio driver sends this connection's transmits.
    pub fn set_egress(&mut self, egress: Egress) {
        self.egress = Some(egress);
    }

    pub fn egress(&self) -> Option<&Egress> {
        self.egress.as_ref()
    }

    /// The current local ufrag. A remote ICE restart replaces it, so a
    /// [`crate::SharedUdp`] route must be registered again after negotiation.
    pub fn local_ufrag(&mut self) -> String {
        self.rtc.direct_api().local_ice_credentials().ufrag
    }

    pub fn add_local_candidate(&mut self, candidate: Candidate) {
        self.rtc.add_local_candidate(candidate);
    }

    /// Answers a remote offer. The text should already carry Piik's SDP
    /// policy (see [`crate::sdp`]).
    pub fn accept_offer(&mut self, offer: &str) -> Result<String, RtcError> {
        let offer = SdpOffer::from_sdp_string(offer)?;
        let answer = self.rtc.sdp_api().accept_offer(offer)?;
        self.remote_described = true;
        Ok(answer.to_sdp_string())
    }

    /// Applies pending local changes made through `change` and returns the
    /// offer text, or `None` when nothing needs negotiation.
    pub fn create_offer(
        &mut self,
        change: impl FnOnce(&mut str0m::change::SdpApi),
    ) -> Option<(String, SdpPendingOffer)> {
        let mut api = self.rtc.sdp_api();
        change(&mut api);
        api.apply()
            .map(|(offer, pending)| (offer.to_sdp_string(), pending))
    }

    pub fn accept_answer(
        &mut self,
        pending: SdpPendingOffer,
        answer: &str,
    ) -> Result<(), RtcError> {
        let answer = SdpAnswer::from_sdp_string(answer)?;
        self.rtc.sdp_api().accept_answer(pending, answer)?;
        self.remote_described = true;
        Ok(())
    }

    /// Adds a remote ICE candidate. Lifecycle errors
    /// ([`CandidateError::NoRemoteDescription`], [`CandidateError::Closed`])
    /// stay distinct from disposable content errors
    /// ([`CandidateError::Invalid`]); end-of-candidates and candidates Piik
    /// cannot use are accepted without effect.
    pub fn add_ice_candidate(&mut self, init: &IceCandidateInit) -> Result<(), CandidateError> {
        if self.closed {
            return Err(CandidateError::Closed);
        }
        if !self.remote_described {
            return Err(CandidateError::NoRemoteDescription);
        }
        if let Some(candidate) = parse_remote(init)? {
            self.rtc.add_remote_candidate(candidate);
        }
        Ok(())
    }

    /// The receive stream an SSRC belongs to. str0m owns the SSRC and mid
    /// tables (including RTX and RID binding); this is a view of them.
    pub fn rx_stream(&mut self, ssrc: Ssrc) -> Option<(Mid, Option<Rid>)> {
        self.rtc
            .direct_api()
            .stream_rx(&ssrc)
            .map(|stream| (stream.mid(), stream.rid()))
    }

    pub fn media_kind(&self, mid: Mid) -> Option<MediaKind> {
        self.rtc.media(mid).map(|media| media.kind())
    }

    pub fn channel(&self, id: ChannelId) -> Option<&Arc<DataChannel>> {
        self.channels.get(&id)
    }

    pub fn channels(&self) -> impl Iterator<Item = &Arc<DataChannel>> {
        self.channels.values()
    }

    /// Stops the connection at once. Its channels become closed and a final
    /// `closed` state is reported.
    pub fn close(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        self.rtc.disconnect();
        for (_, channel) in self.channels.drain() {
            channel.set_closed();
            self.events.push_back(ConnEvent::ChannelClose(channel));
        }
        self.update_state(self.now);
    }

    pub(crate) fn attach(&mut self, ops: Arc<OpQueue>) {
        self.ops = Some(ops);
    }

    pub(crate) fn next_timeout(&self) -> Instant {
        let state_deadline = match self.state {
            PcState::Connecting if !self.failed => Some(self.since + CONNECT_TIMEOUT),
            PcState::Disconnected => Some(self.since + FAILED_AFTER_DISCONNECTED),
            _ => None,
        };
        state_deadline.map_or(self.rtc_timeout, |at| at.min(self.rtc_timeout))
    }

    pub(crate) fn handle_input(&mut self, input: Input) {
        if self.closed {
            return;
        }
        let (Input::Timeout(now) | Input::Receive(now, _)) = &input;
        self.now = self.now.max(*now);
        if let Err(error) = self.rtc.handle_input(input) {
            self.fail(error);
        }
    }

    pub(crate) fn handle_timeout(&mut self, now: Instant) {
        self.now = self.now.max(now);
        if now >= self.rtc_timeout {
            self.handle_input(Input::Timeout(now));
        }
        self.update_state(now);
    }

    pub(crate) fn apply(&mut self, channel: &DataChannel, op: ChannelOp) {
        let id = channel.id();
        match op {
            ChannelOp::Send(text) => {
                channel.sent(text.len());
                if let Some(mut handle) = self.rtc.channel(id) {
                    // A refused or failed write is a lost message.
                    let _ = handle.write(false, text.as_bytes());
                }
            }
            ChannelOp::Close => self.rtc.direct_api().close_data_channel(id),
        }
    }

    pub(crate) fn refresh_buffered(&mut self) {
        for (id, channel) in &self.channels {
            if let Some(mut handle) = self.rtc.channel(*id) {
                channel.set_buffered(handle.buffered_amount());
            }
        }
    }

    /// The next transmit or event, or `None` once str0m only waits on time.
    pub(crate) fn poll(&mut self, now: Instant) -> Option<Polled> {
        loop {
            if let Some(event) = self.events.pop_front() {
                return Some(Polled::Event(event));
            }
            if self.closed {
                return None;
            }
            match self.rtc.poll_output() {
                Ok(Output::Timeout(at)) => {
                    self.rtc_timeout = at;
                    self.update_state(now);
                    if self.events.is_empty() {
                        return None;
                    }
                }
                Ok(Output::Transmit(transmit)) => {
                    return Some(Polled::Transmit(Box::new(transmit)));
                }
                Ok(Output::Event(event)) => self.on_event(event, now),
                Err(error) => self.fail(error),
            }
        }
    }

    fn on_event(&mut self, event: Event, now: Instant) {
        match event {
            Event::Connected => {
                self.dtls_connected = true;
                self.update_state(now);
            }
            Event::IceConnectionStateChange(ice) => {
                self.ice = ice;
                self.update_state(now);
            }
            Event::ChannelOpen(id, label) => {
                let Some(ops) = &self.ops else {
                    return;
                };
                let channel = DataChannel::new(self.id, id, label, ops.clone());
                self.channels.insert(id, channel.clone());
                self.events.push_back(ConnEvent::ChannelOpen(channel));
            }
            Event::ChannelData(data) => {
                if let Some(channel) = self.channels.get(&data.id) {
                    self.events
                        .push_back(ConnEvent::ChannelData(channel.clone(), data));
                }
            }
            Event::ChannelClose(id) => {
                if let Some(channel) = self.channels.remove(&id) {
                    channel.set_closed();
                    self.events.push_back(ConnEvent::ChannelClose(channel));
                }
            }
            event => self.events.push_back(ConnEvent::Rtc(Box::new(event))),
        }
    }

    /// A str0m error is fatal for its session (DTLS, SCTP or SRTP failure).
    fn fail(&mut self, _error: RtcError) {
        self.failed = true;
        self.rtc.disconnect();
        for (_, channel) in self.channels.drain() {
            channel.set_closed();
            self.events.push_back(ConnEvent::ChannelClose(channel));
        }
        self.update_state(self.now);
    }

    fn update_state(&mut self, now: Instant) {
        let ice_connected = self.ice.is_connected();
        if ice_connected && self.rtc.is_alive() {
            self.failed = false;
        }
        let elapsed = now.saturating_duration_since(self.since);
        let next = if self.closed {
            PcState::Closed
        } else if self.failed {
            PcState::Failed
        } else {
            match self.ice {
                _ if !self.remote_described => PcState::New,
                IceConnectionState::Disconnected => PcState::Disconnected,
                _ if ice_connected && self.dtls_connected => PcState::Connected,
                _ => PcState::Connecting,
            }
        };
        let next = match next {
            PcState::Connecting
                if self.state == PcState::Connecting && elapsed >= CONNECT_TIMEOUT =>
            {
                PcState::Failed
            }
            PcState::Disconnected
                if self.state == PcState::Disconnected && elapsed >= FAILED_AFTER_DISCONNECTED =>
            {
                PcState::Failed
            }
            next => next,
        };
        if next == PcState::Failed && !self.closed {
            self.failed = true;
        }
        if next != self.state {
            self.state = next;
            self.since = now;
            self.events.push_back(ConnEvent::State(next));
        }
    }
}
