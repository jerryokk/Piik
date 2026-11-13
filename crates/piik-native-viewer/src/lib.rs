//! The native Viewer share on `piik-media-edge`: up to two receivers (current
//! upstream and one prepared route), relay edges to downstream Viewers bound
//! to their source receiver, connection preparation and the input relay
//! between them.
//!
//! Room, participant, route and recovery authority remain in the page and
//! server; this session owns only native media.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::{Duration, SystemTime};

use piik_input_route::{InputChannel, Relay};
use piik_media_edge::{
    ChannelEvent, Edge, EdgeEvent, Engine, EngineOptions, IceServerUrls, Receiver, ReceiverOptions,
    RelayOptions,
};
use piik_media_transport::DataChannel;
use piik_native_capture::VideoProfile;
use piik_native_control::media::{MediaError, MediaEventKind, MediaResult, OfferResult};
use piik_native_control::wire::IceCandidateInit;
use piik_native_host::bridge::{self, Emitter, EventSink};
use tokio_util::sync::CancellationToken;

mod media;

pub use media::Factory;

/// One current upstream and one prepared route may overlap during a handoff.
const RECEIVER_CAPACITY: usize = 2;
const QUALITY_SAMPLE_PERIOD: Duration = Duration::from_secs(2);
const MAX_ID_BYTES: usize = 256;

type InputRelay = Relay<DataChannel>;

/// One receiver and the input relay toward its upstream, when the offer
/// carried an application section.
struct OwnedReceiver {
    receiver: Receiver,
    input: Option<Arc<InputRelay>>,
}

impl OwnedReceiver {
    async fn close(&self) {
        if let Some(input) = &self.input {
            input.close();
        }
        self.receiver.close().await;
    }
}

#[derive(Clone)]
struct OwnedEdge {
    edge: Edge,
    source_connection_id: String,
}

#[derive(Default)]
struct State {
    receivers: HashMap<String, Arc<OwnedReceiver>>,
    edges: HashMap<String, OwnedEdge>,
    profile: Option<VideoProfile>,
    closed: bool,
}

struct Inner {
    share_id: String,
    edge_capacity: usize,
    engine: Engine,
    emitter: Emitter,
    ctx: CancellationToken,
    relay: Option<RelayOptions>,
    state: Mutex<State>,
}

/// `nativeviewer.Options`.
pub struct Options {
    pub share_id: String,
    pub edge_capacity: usize,
    pub port_mapping: bool,
    pub events: Arc<dyn EventSink>,
    /// Where a capable Viewer derives relay layers; `None` relays the
    /// received layer only.
    pub relay: Option<RelayOptions>,
    /// Test seam: IPv4 `address:port`; empty binds every interface.
    pub bind_address: String,
}

/// `nativeviewer.Session`. Cheap to clone.
#[derive(Clone)]
pub struct Session {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Session")
            .field("share_id", &self.inner.share_id)
            .finish_non_exhaustive()
    }
}

fn closed() -> MediaError {
    MediaError::failed("native Viewer session is closed")
}

impl Inner {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }

    fn receiver(&self, connection_id: &str) -> Option<Arc<OwnedReceiver>> {
        let state = self.lock();
        if state.closed {
            return None;
        }
        state.receivers.get(connection_id).cloned()
    }

    fn edge(&self, connection_id: &str) -> Option<OwnedEdge> {
        let state = self.lock();
        if state.closed {
            return None;
        }
        state.edges.get(connection_id).cloned()
    }

    async fn close_edge(&self, connection_id: &str) {
        self.engine.close_punch(connection_id);
        let owned = self.lock().edges.remove(connection_id);
        if let Some(owned) = owned {
            owned.edge.close().await;
        }
    }

    async fn close_receiver(&self, connection_id: &str) {
        self.engine.close_punch(connection_id);
        let (receiver, edges) = {
            let mut state = self.lock();
            let receiver = state.receivers.remove(connection_id);
            let children: Vec<String> = state
                .edges
                .iter()
                .filter(|(_, owned)| owned.source_connection_id == connection_id)
                .map(|(id, _)| id.clone())
                .collect();
            let edges: Vec<Edge> = children
                .iter()
                .filter_map(|id| state.edges.remove(id))
                .map(|owned| owned.edge)
                .collect();
            (receiver, edges)
        };
        for edge in edges {
            edge.close().await;
        }
        if let Some(receiver) = receiver {
            receiver.close().await;
        }
    }

    async fn run_quality(self: Arc<Self>) {
        let mut ticker = tokio::time::interval(QUALITY_SAMPLE_PERIOD);
        ticker.tick().await;
        loop {
            tokio::select! {
                _ = ticker.tick() => {}
                () = self.ctx.cancelled() => return,
            }
            let now = SystemTime::now();
            let edges: Vec<Edge> = self
                .lock()
                .edges
                .values()
                .map(|owned| owned.edge.clone())
                .collect();
            for edge in edges {
                if let Some(sample) = edge.quality_sample(now).await {
                    self.emitter.emit(
                        MediaEventKind::EdgeQuality {
                            connection_id: edge.connection_id().to_owned(),
                            quality: bridge::quality_sample(sample),
                        },
                        None,
                    );
                }
            }
        }
    }
}

/// A receiver's sink: candidates and states for its connection, and the
/// upstream input channel for its relay. The receiver's own state watch
/// fences its events; a same-ID replacement is a new receiver.
fn receiver_sink(
    weak: Weak<Inner>,
    connection_id: String,
    input: Option<Arc<InputRelay>>,
) -> piik_media_edge::EventSink {
    Arc::new(move |event: EdgeEvent| {
        let Some(inner) = weak.upgrade() else { return };
        if let Some(fact) = bridge::edge_fact(&event) {
            bridge::emit_edge_fact(&inner.emitter, &connection_id, fact, None);
            return;
        }
        let Some(input) = &input else {
            if let EdgeEvent::Channel(ChannelEvent::Open(channel)) = event {
                channel.close();
            }
            return;
        };
        match event {
            EdgeEvent::Channel(ChannelEvent::Open(channel)) => {
                if input.attach_up(channel.clone()) {
                    input.up_opened(&channel);
                }
            }
            EdgeEvent::Channel(ChannelEvent::Data {
                channel,
                data,
                binary,
            }) => input.up_message(&channel, &data, !binary),
            EdgeEvent::Channel(ChannelEvent::Close(channel)) => input.up_closed(&channel),
            // ICE loss releases all descendant input even before SCTP closes.
            EdgeEvent::InputTransport(available) => input.set_up_available(available),
            _ => {}
        }
    })
}

type Lost = Box<dyn Fn() + Send + Sync>;
/// The bound downstream input channel and its transport-loss action.
type Bound = Option<(Arc<DataChannel>, Lost)>;

/// A relay edge's sink: media facts for its connection and the downstream
/// leg of its source receiver's input relay.
fn edge_sink(
    weak: Weak<Inner>,
    connection_id: String,
    input: Option<Arc<InputRelay>>,
) -> piik_media_edge::EventSink {
    let leg: Arc<Mutex<Bound>> = Arc::default();
    Arc::new(move |event: EdgeEvent| {
        let Some(inner) = weak.upgrade() else { return };
        if let Some(fact) = bridge::edge_fact(&event) {
            // Relay edge candidates are fenced by their gathering generation
            // only; the state watch fences state and path.
            bridge::emit_edge_fact(&inner.emitter, &connection_id, fact, None);
            return;
        }
        let Some(input) = &input else { return };
        let mut current = leg.lock().unwrap_or_else(|error| error.into_inner());
        match event {
            EdgeEvent::Channel(ChannelEvent::Open(channel)) => {
                if channel.label() != piik_input_route::LABEL || current.is_some() {
                    drop(current);
                    channel.close();
                    return;
                }
                let lost = input.attach_down(&connection_id, channel.clone());
                *current = Some((channel.clone(), lost));
                drop(current);
                input.down_opened(&connection_id, &channel);
            }
            EdgeEvent::Channel(ChannelEvent::Data {
                channel,
                data,
                binary,
            }) => {
                drop(current);
                input.down_message(&connection_id, &channel, &data, !binary);
            }
            EdgeEvent::Channel(ChannelEvent::Close(channel)) => {
                if current
                    .as_ref()
                    .is_some_and(|(owned, _)| Arc::ptr_eq(owned, &channel))
                {
                    *current = None;
                }
                drop(current);
                input.down_closed(&connection_id, &channel);
            }
            EdgeEvent::InputTransport(false) => {
                if let Some((_, lost)) = current.as_ref() {
                    lost();
                }
            }
            _ => {}
        }
    })
}

impl Session {
    /// Starts the share; `parent` owns it.
    pub fn start(parent: &CancellationToken, options: Options) -> MediaResult<Self> {
        if options.share_id.is_empty()
            || options.share_id.len() > MAX_ID_BYTES
            || !(1..=piik_media_edge::MAX_RELAY_CHILDREN).contains(&options.edge_capacity)
        {
            return Err(MediaError::failed("native Viewer session input is invalid"));
        }
        let engine = Engine::new(EngineOptions {
            bind_address: options.bind_address,
            include_loopback: true,
            port_mapping: options.port_mapping,
            initial_bitrate: 0,
        })
        .map_err(bridge::media_error)?;
        let ctx = parent.child_token();
        let (emitter, _forwarder) = Emitter::new(&options.share_id, options.events, ctx.clone());
        let inner = Arc::new(Inner {
            share_id: options.share_id,
            edge_capacity: options.edge_capacity,
            engine,
            emitter,
            ctx,
            relay: options.relay,
            state: Mutex::new(State::default()),
        });
        tokio::spawn(inner.clone().run_quality());
        Ok(Self { inner })
    }

    pub fn share_id(&self) -> &str {
        &self.inner.share_id
    }

    /// Records the Host ceiling; receiving alone never starts derivation.
    pub async fn update_profile(&self, profile: VideoProfile) -> MediaResult<()> {
        if !profile.valid() {
            return Err(MediaError::failed("native Viewer profile is invalid"));
        }
        let receivers: Vec<Arc<OwnedReceiver>> = {
            let state = self.inner.lock();
            if state.closed {
                return Err(closed());
            }
            if state.profile.as_ref() == Some(&profile) {
                return Ok(());
            }
            state.receivers.values().cloned().collect()
        };
        for receiver in receivers {
            receiver
                .receiver
                .source()
                .set_relay_profile(profile.clone())
                .await
                .map_err(bridge::media_error)?;
        }
        let mut state = self.inner.lock();
        if state.closed {
            return Err(closed());
        }
        state.profile = Some(profile);
        Ok(())
    }

    pub async fn accept_offer(
        &self,
        connection_id: &str,
        sdp: &str,
        ice_servers: Vec<IceServerUrls>,
        reuse: bool,
        punched: bool,
    ) -> MediaResult<OfferResult> {
        let inner = &self.inner;
        if connection_id.is_empty() || connection_id.len() > MAX_ID_BYTES {
            return Err(MediaError::failed(
                "native Viewer receiver identity is invalid",
            ));
        }
        let (current, at_capacity) = {
            let state = inner.lock();
            if state.closed {
                return Err(closed());
            }
            let current = state.receivers.get(connection_id).cloned();
            let full = state.receivers.len() >= RECEIVER_CAPACITY && current.is_none();
            (current, full)
        };
        if at_capacity {
            return Err(MediaError::failed(
                "native Viewer receiver capacity reached",
            ));
        }
        if reuse && let Some(current) = &current {
            let answer = current
                .receiver
                .renegotiate(sdp, ice_servers.clone())
                .await
                .map_err(bridge::media_error)?;
            if let Some(answer_sdp) = answer {
                return Ok(OfferResult {
                    answer_sdp,
                    audio: current.receiver.has_audio(),
                    codec: current.receiver.codec().to_owned(),
                    reused: true,
                });
            }
        }
        let punched_socket = if punched {
            Some(
                inner
                    .engine
                    .take_punched_socket(connection_id, None)
                    .ok_or_else(|| MediaError::failed("prepared connection is unavailable"))?,
            )
        } else {
            None
        };
        inner.close_receiver(connection_id).await;
        let input = sdp
            .contains("\nm=application ")
            .then(|| Arc::new(InputRelay::new(None)));
        let created = inner
            .engine
            .new_receiver(ReceiverOptions {
                offer: sdp.to_owned(),
                ice_servers,
                edge_capacity: piik_media_edge::physical_source_capacity(inner.edge_capacity),
                relay: inner.relay.clone(),
                punched_socket,
                events: Some(receiver_sink(
                    Arc::downgrade(inner),
                    connection_id.to_owned(),
                    input.clone(),
                )),
            })
            .await;
        let (receiver, answer_sdp) = match created {
            Ok(created) => created,
            Err(error) => {
                if let Some(input) = &input {
                    input.close();
                }
                return Err(bridge::media_error(error));
            }
        };
        let owned = Arc::new(OwnedReceiver { receiver, input });
        let profile = {
            let state = inner.lock();
            let free = !state.closed
                && !state.receivers.contains_key(connection_id)
                && state.receivers.len() < RECEIVER_CAPACITY;
            free.then(|| state.profile.clone())
        };
        let Some(profile) = profile else {
            owned.close().await;
            return Err(MediaError::failed("native Viewer receiver is unavailable"));
        };
        if let Some(profile) = profile
            && let Err(error) = owned.receiver.source().set_relay_profile(profile).await
        {
            owned.close().await;
            return Err(bridge::media_error(error));
        }
        let installed = {
            let mut state = inner.lock();
            let free = !state.closed
                && !state.receivers.contains_key(connection_id)
                && state.receivers.len() < RECEIVER_CAPACITY;
            if free {
                state
                    .receivers
                    .insert(connection_id.to_owned(), owned.clone());
            }
            free
        };
        if !installed {
            owned.close().await;
            return Err(MediaError::failed("native Viewer receiver is unavailable"));
        }
        Ok(OfferResult {
            answer_sdp,
            audio: owned.receiver.has_audio(),
            codec: owned.receiver.codec().to_owned(),
            reused: false,
        })
    }

    pub async fn add_receiver_candidate(
        &self,
        connection_id: &str,
        candidate: Option<IceCandidateInit>,
    ) -> MediaResult<()> {
        let Some(receiver) = self.inner.receiver(connection_id) else {
            return Ok(());
        };
        receiver
            .receiver
            .add_remote_candidate(candidate.map(bridge::edge_candidate))
            .await
            .map_err(bridge::media_error)
    }

    pub async fn close_receiver(&self, connection_id: &str) {
        self.inner.close_receiver(connection_id).await;
    }

    pub async fn prepare_edge(
        &self,
        source_connection_id: &str,
        connection_id: &str,
        ice_servers: Vec<IceServerUrls>,
        punched: bool,
    ) -> MediaResult<String> {
        self.prepare(
            source_connection_id,
            connection_id,
            ice_servers,
            "",
            punched,
        )
        .await
    }

    pub async fn prepare_local_edge(
        &self,
        source_connection_id: &str,
        connection_id: &str,
        offer_id: &str,
        restart: bool,
    ) -> MediaResult<String> {
        if restart {
            let edge = self
                .inner
                .edge(connection_id)
                .filter(|owned| owned.source_connection_id == source_connection_id)
                .ok_or_else(|| MediaError::failed("native local Viewer edge does not exist"))?;
            return edge
                .edge
                .create_local_offer(offer_id, true)
                .await
                .map_err(bridge::media_error);
        }
        self.prepare(
            source_connection_id,
            connection_id,
            Vec::new(),
            offer_id,
            false,
        )
        .await
    }

    async fn prepare(
        &self,
        source_connection_id: &str,
        connection_id: &str,
        ice_servers: Vec<IceServerUrls>,
        local_offer_id: &str,
        punched: bool,
    ) -> MediaResult<String> {
        let inner = &self.inner;
        let unavailable = || MediaError::failed("native Viewer edge is unavailable");
        let local = !local_offer_id.is_empty();
        let receiver = inner
            .receiver(source_connection_id)
            .ok_or_else(|| MediaError::failed("native Viewer source does not exist"))?;
        {
            let state = inner.lock();
            if state.closed || state.edges.contains_key(connection_id) {
                return Err(unavailable());
            }
            if !local && state.profile.is_none() {
                return Err(MediaError::failed("native relay requires the Host profile"));
            }
        }
        let edge = inner
            .engine
            .new_edge(
                receiver.receiver.source(),
                piik_media_edge::EdgeOptions {
                    connection_id: connection_id.to_owned(),
                    ice_servers,
                    audio: receiver.receiver.audio_source().cloned(),
                    local,
                    punched,
                    events: Some(edge_sink(
                        Arc::downgrade(inner),
                        connection_id.to_owned(),
                        receiver.input.clone(),
                    )),
                },
            )
            .await
            .map_err(bridge::media_error)?;
        let installed = {
            let mut state = inner.lock();
            let free = !state.closed
                && !state.edges.contains_key(connection_id)
                && state
                    .receivers
                    .get(source_connection_id)
                    .is_some_and(|current| Arc::ptr_eq(current, &receiver));
            if free {
                state.edges.insert(
                    connection_id.to_owned(),
                    OwnedEdge {
                        edge: edge.clone(),
                        source_connection_id: source_connection_id.to_owned(),
                    },
                );
            }
            free
        };
        if !installed {
            edge.close().await;
            return Err(unavailable());
        }
        if receiver.input.is_some()
            && let Err(error) = edge.open_data_channel(piik_input_route::LABEL).await
        {
            inner.close_edge(connection_id).await;
            return Err(bridge::media_error(error));
        }
        let offer = if local {
            edge.create_local_offer(local_offer_id, false).await
        } else {
            edge.create_offer().await
        };
        match offer {
            Ok(offer) => Ok(offer),
            Err(error) => {
                inner.close_edge(connection_id).await;
                Err(bridge::media_error(error))
            }
        }
    }

    pub async fn set_answer(
        &self,
        connection_id: &str,
        sdp: &str,
        offer_id: &str,
    ) -> MediaResult<()> {
        let edge = self
            .inner
            .edge(connection_id)
            .ok_or_else(|| MediaError::failed("native Viewer edge does not exist"))?;
        edge.edge
            .set_answer(sdp, offer_id)
            .await
            .map_err(bridge::media_error)
    }

    pub async fn add_candidate(
        &self,
        connection_id: &str,
        candidate: Option<IceCandidateInit>,
    ) -> MediaResult<()> {
        let Some(edge) = self.inner.edge(connection_id) else {
            return Ok(());
        };
        edge.edge
            .add_remote_candidate(candidate.map(bridge::edge_candidate))
            .await
            .map_err(bridge::media_error)
    }

    pub async fn close_edge(&self, connection_id: &str) {
        self.inner.close_edge(connection_id).await;
    }

    /// An empty `source_connection_id` prepares a receiver.
    pub fn begin_punch(
        &self,
        connection_id: &str,
        source_connection_id: &str,
        ice_servers: Vec<IceServerUrls>,
        secret: &str,
    ) -> MediaResult<piik_media_edge::PunchPreparation> {
        let state = self.inner.lock();
        if state.closed
            || state.edges.contains_key(connection_id)
            || state.receivers.contains_key(connection_id)
        {
            return Err(MediaError::failed(
                "native connection is unavailable for preparation",
            ));
        }
        let source = if source_connection_id.is_empty() {
            None
        } else {
            let receiver = state
                .receivers
                .get(source_connection_id)
                .ok_or_else(|| MediaError::failed("native Viewer source is unavailable"))?;
            Some(receiver.receiver.source().clone())
        };
        self.inner
            .engine
            .begin_punch(connection_id, source.as_ref(), ice_servers, secret)
            .map_err(bridge::media_error)
    }

    pub async fn close_session(&self) -> MediaResult<()> {
        let inner = &self.inner;
        let (receivers, edges) = {
            let mut state = inner.lock();
            if state.closed {
                return Ok(());
            }
            state.closed = true;
            (
                std::mem::take(&mut state.receivers),
                std::mem::take(&mut state.edges),
            )
        };
        inner.ctx.cancel();
        for owned in edges.into_values() {
            owned.edge.close().await;
        }
        for receiver in receivers.into_values() {
            receiver.close().await;
        }
        inner.engine.close().await.map_err(bridge::media_error)
    }

    #[cfg(test)]
    fn receiver_count(&self) -> usize {
        self.inner.lock().receivers.len()
    }

    #[cfg(test)]
    fn edge_source(&self, connection_id: &str) -> Option<String> {
        self.inner
            .lock()
            .edges
            .get(connection_id)
            .map(|owned| owned.source_connection_id.clone())
    }

    #[cfg(test)]
    fn receiver_source(&self, connection_id: &str) -> Option<piik_media_edge::VideoSource> {
        self.inner
            .receiver(connection_id)
            .map(|owned| owned.receiver.source().clone())
    }
}

#[cfg(test)]
mod tests;
