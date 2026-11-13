//! Source creation, closed-state lookup and the retirement of one source
//! with every edge and publication attached to it.

use super::*;

impl App {
    pub(crate) fn new_source(
        &mut self,
        codec: VideoCodec,
        capacity: usize,
        layers: usize,
        relay: Option<RelayOptions>,
        callback: Option<RecoveryCallback>,
    ) -> Result<u64> {
        if !(1..=MAX_RELAY_CHILDREN + 1).contains(&capacity) {
            return Err(Error::Invalid(
                "native media source capacity is outside the route bound",
            ));
        }
        if !(1..=3).contains(&layers) {
            return Err(Error::Invalid(
                "native media source output count is invalid",
            ));
        }
        if self.closed {
            return Err(Error::EngineClosed);
        }
        let clock = if relay.is_some() {
            InputClock::Source { layer: layers - 1 }
        } else {
            InputClock::Local(self.anchor)
        };
        let media = EncodedSource::new(
            codec::source_options(codec, vec![LayerFormat::default(); layers]),
            clock,
        )?;
        let key = next_key();
        let relay_state = relay.map(RelayState::new);
        self.sources.insert(
            key,
            SourceState {
                codec,
                format_count: layers,
                capacity,
                media,
                group_media: HashMap::new(),
                groups: OutputGroups::new(ORIGINAL, relay_state.is_some()),
                next_media: 0,
                output_bitrates: Vec::new(),
                callback,
                upstream: None,
                pending_recovery: 0,
                throttles: HashMap::new(),
                input_pts: Duration::ZERO,
                input_at: None,
                has_input: false,
                edges: HashMap::new(),
                publications: HashSet::new(),
                reservations: 0,
                relay: relay_state,
                relay_profile: None,
                closed: false,
            },
        );
        Ok(key)
    }

    pub(crate) fn source(&mut self, key: u64) -> Result<&mut SourceState> {
        self.sources
            .get_mut(&key)
            .filter(|source| !source.closed)
            .ok_or(Error::Closed)
    }

    /// Delivers one source's pending recovery notification.
    pub(crate) fn flush_recovery(&mut self, conns: &mut Connections, key: u64, now: Instant) {
        let Some(source) = self.sources.get_mut(&key) else {
            return;
        };
        let Some(layers) = source.take_recovery() else {
            return;
        };
        if let Some(receiver) = source.upstream {
            // Local output slots all derive from one upstream encoded stream.
            self.receiver_request_keyframe(conns, receiver, now);
        } else if let Some(callback) = source.callback.clone() {
            callback(layers);
        }
    }

    /// An edge left; a no longer wanted derivation retires.
    pub(crate) fn detach_edge(
        &mut self,
        conns: &mut Connections,
        key: u64,
        edge: u64,
        now: Instant,
    ) {
        let Some(state) = self.sources.get_mut(&key) else {
            return;
        };
        state.edges.remove(&edge);
        state.request_recovery(None);
        if state.relay.is_some() && !self.relay_plan(key).wanted {
            self.relay_cancel(conns, key, now);
        }
    }

    pub(crate) fn close_source(&mut self, conns: &mut Connections, key: u64, now: Instant) {
        let Some(state) = self.sources.get_mut(&key) else {
            return;
        };
        if state.closed {
            return;
        }
        let edges: Vec<u64> = state.edges.keys().copied().collect();
        let publications: Vec<u64> = state.publications.iter().copied().collect();
        state.pending_recovery = 0;
        for edge in edges {
            self.close_edge(conns, edge, now);
        }
        for publication in publications {
            self.close_publication(conns, publication, now);
        }
        self.relay_cancel(conns, key, now);
        if let Some(engine) = self.engine() {
            engine.close_source_punch(key);
        }
        if let Some(state) = self.sources.get_mut(&key) {
            state.closed = true;
            state.media.close();
            for media in state.group_media.values_mut() {
                media.close();
            }
        }
        self.sources.remove(&key);
    }
}
