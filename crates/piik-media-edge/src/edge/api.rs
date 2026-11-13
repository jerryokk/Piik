//! The public edge handle and its Engine constructor and operations.

use super::*;

/// One outbound Native edge. Cheap to clone.
#[derive(Clone)]
pub struct Edge {
    engine: Engine,
    pub(crate) key: u64,
    connection_id: String,
    local: bool,
    state: StateWatch,
}

impl std::fmt::Debug for Edge {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Edge")
            .field("connection_id", &self.connection_id)
            .field("state", &self.state.get())
            .finish()
    }
}

impl Engine {
    /// Opens an outbound edge carrying `source`.
    pub async fn new_edge(&self, source: &VideoSource, options: EdgeOptions) -> Result<Edge> {
        if !source.engine.same(self)
            || options.connection_id.is_empty()
            || options.connection_id.len() > 256
        {
            return Err(Error::Invalid("native media edge input is invalid"));
        }
        if options
            .audio
            .as_ref()
            .is_some_and(|audio| !audio.engine.same(self))
        {
            return Err(Error::Invalid(
                "native audio edge source belongs to another engine",
            ));
        }
        let source_key = source.key;
        let audio_key = options.audio.as_ref().map(|audio| audio.key);
        let local = options.local;
        self.call(move |app, _, _| app.admit_edge(source_key, audio_key, local))
            .await??;
        let punched = if options.punched && !local {
            Some(
                self.take_punched_socket(&options.connection_id, Some(source))
                    .ok_or(Error::Invalid("prepared connection is unavailable"))?,
            )
        } else {
            self.close_punch(&options.connection_id);
            None
        };
        let settings = self.settings();
        let link = Link::open(settings, punched, &options.ice_servers, !local)?;
        let codec = source.video_codec();
        let mut conn = Connection::new(
            codec::rtc_config(&[codec], audio_key.is_some(), settings.initial_bitrate),
            Instant::now(),
        );
        link.bind(self.inbox(), &mut conn);
        let state = StateWatch::default();
        let mut signal = Signal::new(
            link,
            options.events.unwrap_or_else(discard),
            options.ice_servers,
            state.clone(),
        );
        signal.local = local;
        let new = NewEdge {
            source: source_key,
            audio: audio_key,
            local,
            codec,
            signal,
        };
        let key = self
            .call(move |app, conns, _| app.insert_edge(conns, new, conn))
            .await??;
        Ok(Edge {
            engine: self.clone(),
            key,
            connection_id: options.connection_id,
            local,
            state,
        })
    }
}

impl Edge {
    pub fn connection_id(&self) -> &str {
        &self.connection_id
    }

    /// The current projected peer connection state.
    pub fn state(&self) -> PcState {
        self.state.get()
    }

    pub fn state_watch(&self) -> StateWatch {
        self.state.clone()
    }

    /// Adds a channel to the initial offer. Its handle
    /// arrives with [`ChannelEvent::Open`]; the edge's connection owns it.
    pub async fn open_data_channel(&self, label: &str) -> Result<()> {
        let key = self.key;
        let label = label.to_owned();
        self.engine
            .call(move |app, _, _| {
                let edge = app.edges.get_mut(&key).ok_or(Error::Closed)?;
                if edge.signal.offered {
                    return Err(Error::Invalid("data channels belong in the initial offer"));
                }
                edge.signal.channels.push(label);
                Ok(())
            })
            .await?
    }

    /// A media-component input, not a route change.
    pub async fn set_target_layer(&self, layer: usize) -> Result<()> {
        let key = self.key;
        self.engine
            .call(move |app, conns, _| app.edge_set_target(conns, key, layer))
            .await?
    }

    /// A trickled offer. Host candidates, the STUN survey, gateway mapping and
    /// end-of-candidates follow as [`EdgeEvent::LocalCandidate`].
    pub async fn create_offer(&self) -> Result<String> {
        if self.local {
            return Err(Error::Invalid("local media requires an identified offer"));
        }
        let key = self.key;
        let cancel = self.engine.inner.cancel.clone();
        self.engine
            .call(move |app, conns, _| {
                let offer = app.edge_offer(conns, key, false)?;
                let edge = app.edges.get_mut(&key).ok_or(Error::Closed)?;
                let ufrag = conns.get_mut(edge.conn).ok_or(Error::Closed)?.local_ufrag();
                let mid = edge.mid();
                let signal = &edge.signal;
                signal.link.start_gathering(
                    &cancel,
                    &signal.servers,
                    true,
                    &mid,
                    &ufrag,
                    signal.sink.clone(),
                );
                Ok(trickle_sdp(&offer))
            })
            .await?
    }

    /// A complete SDP for a local bridge, including ICE restarts. Local
    /// bridges have no STUN or gateway discovery.
    pub async fn create_local_offer(&self, offer_id: &str, restart: bool) -> Result<String> {
        let key = self.key;
        let offer_id = offer_id.to_owned();
        self.engine
            .call(move |app, conns, _| {
                let available = app.edges.get(&key).is_some_and(|edge| {
                    let signal = &edge.signal;
                    edge.local
                        && !signal.closed
                        && !offer_id.is_empty()
                        && offer_id != signal.local_offer_id
                        && (!restart && signal.local_offer_id.is_empty()
                            || restart && signal.remote_described)
                });
                if !available {
                    return Err(Error::Invalid("local media negotiation is unavailable"));
                }
                let offer = app.edge_offer(conns, key, restart)?;
                let edge = app.edges.get_mut(&key).ok_or(Error::Closed)?;
                edge.signal.local_offer_id = offer_id;
                Ok(strip_end_of_candidates(&offer))
            })
            .await?
    }

    /// Applies the remote answer. `offer_id` is empty for a trickled edge.
    pub async fn set_answer(&self, answer: &str, offer_id: &str) -> Result<()> {
        let key = self.key;
        let (answer, offer_id) = (answer.to_owned(), offer_id.to_owned());
        self.engine
            .call(move |app, conns, _| app.edge_set_answer(conns, key, &answer, &offer_id))
            .await?
    }

    /// Adds a trickled remote candidate; `None` is end-of-candidates.
    pub async fn add_remote_candidate(&self, candidate: Option<IceCandidateInit>) -> Result<()> {
        if self.local {
            return Err(Error::Invalid(
                "local media candidates belong in the SDP answer",
            ));
        }
        let key = self.key;
        let candidate = candidate.unwrap_or_default();
        self.engine
            .call(move |app, conns, _| app.edge_add_candidate(conns, key, candidate))
            .await?
    }

    /// The next quality sample: `None` on the first call (baseline), outside
    /// the one-to-five-second window, or after a counter reset.
    pub async fn quality_sample(&self, now: SystemTime) -> Option<QualitySample> {
        let key = self.key;
        self.engine
            .call(move |app, _, _| app.edge_quality(key, now))
            .await
            .ok()
            .flatten()
    }

    /// Closes the edge. Idempotent.
    pub async fn close(&self) {
        let key = self.key;
        let _ = self
            .engine
            .call(move |app, conns, now| app.close_edge(conns, key, now))
            .await;
        self.state.set(PcState::Closed);
    }
}
