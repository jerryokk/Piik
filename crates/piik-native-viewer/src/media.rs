//! The native Viewer media factory: shares, relay capabilities and the media
//! trait surface.

use std::path::PathBuf;
use std::sync::Arc;

use futures_util::future::BoxFuture;
use piik_media_edge::{IceServerUrls, RelayOptions};
use piik_native_capture::{Capabilities, VideoProfile};
use piik_native_control::media::{
    MediaResult, OfferResult, OutboundMedia, PunchPreparation, ViewerMedia, ViewerMediaFactory,
    ViewerOptions,
};
use piik_native_control::wire::IceCandidateInit;
use piik_native_host::bridge;
use tokio_util::sync::CancellationToken;

use crate::{Options, Session};

/// Starts native Viewer shares.
pub struct Factory {
    capture_process: PathBuf,
    capabilities: Capabilities,
}

impl Factory {
    /// `capture_process` and `capabilities` let a capable Viewer derive
    /// relay layers on its own encoder.
    pub fn new(capture_process: PathBuf, capabilities: Capabilities) -> Self {
        Self {
            capture_process,
            capabilities,
        }
    }
}

impl ViewerMediaFactory for Factory {
    fn start(
        &self,
        cancel: CancellationToken,
        options: ViewerOptions,
    ) -> BoxFuture<'_, MediaResult<Arc<dyn ViewerMedia>>> {
        Box::pin(async move {
            let session = Session::start(
                &cancel,
                Options {
                    share_id: options.share_id,
                    edge_capacity: options.edge_capacity,
                    port_mapping: options.port_mapping,
                    events: Arc::new(options.events),
                    relay: Some(RelayOptions {
                        capture_process: self.capture_process.clone(),
                        capabilities: self.capabilities.clone(),
                    }),
                    bind_address: String::new(),
                },
            )?;
            Ok(Arc::new(session) as Arc<dyn ViewerMedia>)
        })
    }
}

impl OutboundMedia for Session {
    fn share_id(&self) -> &str {
        Session::share_id(self)
    }

    fn set_answer<'a>(
        &'a self,
        connection_id: &'a str,
        sdp: &'a str,
        offer_id: &'a str,
    ) -> BoxFuture<'a, MediaResult<()>> {
        Box::pin(Session::set_answer(self, connection_id, sdp, offer_id))
    }

    fn add_candidate<'a>(
        &'a self,
        connection_id: &'a str,
        candidate: Option<IceCandidateInit>,
    ) -> BoxFuture<'a, MediaResult<()>> {
        Box::pin(Session::add_candidate(self, connection_id, candidate))
    }

    fn close_edge<'a>(&'a self, connection_id: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(Session::close_edge(self, connection_id))
    }

    fn punch(&self, connection_id: &str) -> Option<Arc<dyn PunchPreparation>> {
        self.inner.engine.punch(connection_id).map(bridge::punch)
    }

    fn close_punch(&self, connection_id: &str) {
        self.inner.engine.close_punch(connection_id);
    }
}

impl ViewerMedia for Session {
    fn update_profile(&self, profile: VideoProfile) -> BoxFuture<'_, MediaResult<()>> {
        Box::pin(Session::update_profile(self, profile))
    }

    fn accept_offer<'a>(
        &'a self,
        connection_id: &'a str,
        sdp: &'a str,
        ice_servers: Vec<IceServerUrls>,
        reuse: bool,
        punched: bool,
    ) -> BoxFuture<'a, MediaResult<OfferResult>> {
        Box::pin(Session::accept_offer(
            self,
            connection_id,
            sdp,
            ice_servers,
            reuse,
            punched,
        ))
    }

    fn add_receiver_candidate<'a>(
        &'a self,
        connection_id: &'a str,
        candidate: Option<IceCandidateInit>,
    ) -> BoxFuture<'a, MediaResult<()>> {
        Box::pin(Session::add_receiver_candidate(
            self,
            connection_id,
            candidate,
        ))
    }

    fn close_receiver<'a>(&'a self, connection_id: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(Session::close_receiver(self, connection_id))
    }

    fn prepare_edge<'a>(
        &'a self,
        source_connection_id: &'a str,
        connection_id: &'a str,
        ice_servers: Vec<IceServerUrls>,
        punched: bool,
    ) -> BoxFuture<'a, MediaResult<String>> {
        Box::pin(Session::prepare_edge(
            self,
            source_connection_id,
            connection_id,
            ice_servers,
            punched,
        ))
    }

    fn prepare_local_edge<'a>(
        &'a self,
        source_connection_id: &'a str,
        connection_id: &'a str,
        offer_id: &'a str,
        restart: bool,
    ) -> BoxFuture<'a, MediaResult<String>> {
        Box::pin(Session::prepare_local_edge(
            self,
            source_connection_id,
            connection_id,
            offer_id,
            restart,
        ))
    }

    fn begin_punch(
        &self,
        connection_id: &str,
        source_connection_id: &str,
        ice_servers: Vec<IceServerUrls>,
        secret: &str,
    ) -> MediaResult<Arc<dyn PunchPreparation>> {
        Session::begin_punch(
            self,
            connection_id,
            source_connection_id,
            ice_servers,
            secret,
        )
        .map(bridge::punch)
    }

    fn close(&self) -> BoxFuture<'_, MediaResult<()>> {
        Box::pin(Session::close_session(self))
    }
}
