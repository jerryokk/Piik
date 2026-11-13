//! The native Host share on `piik-media-edge`.
//!
//! [`Session`] owns one share's capture generation, audio, P2P edges, local
//! App edge, SFU publications and remote-input session. [`Factory`] implements
//! native control's [`HostMediaFactory`]; [`bridge`] holds the media/control
//! seams the native Viewer reuses.

mod audio_mix;
pub mod bridge;
mod capture;
mod publication;
mod session;

#[cfg(test)]
mod tests;

use std::path::PathBuf;
use std::sync::Arc;

use futures_util::future::BoxFuture;
use piik_native_capture::{CaptureTarget, VideoOptions};
use piik_native_control::media::{
    HostEnded, HostMedia, HostMediaFactory, HostOptions, IceServerUrls, MediaResult, OutboundMedia,
    PolicyApply, PunchPreparation, QualityProfile,
};
use piik_native_control::wire::IceCandidateInit;
use piik_protocol::RemoteInputPeer;
use piik_remote_input::Manager;
use tokio_util::sync::CancellationToken;

pub use capture::{CaptureProvider, CaptureState, CaptureStream, ProcessCapture, SharedStream};
pub use publication::HostPublication;
pub use session::{Options, Session};

/// Starts native Host shares on the packaged capture helper.
pub struct Factory {
    capture: Arc<dyn CaptureProvider>,
    remote_input: Option<Arc<Manager>>,
}

impl Factory {
    pub fn new(executable: PathBuf, remote_input: Option<Arc<Manager>>) -> Self {
        Self::with_capture(Arc::new(ProcessCapture { executable }), remote_input)
    }

    pub fn with_capture(
        capture: Arc<dyn CaptureProvider>,
        remote_input: Option<Arc<Manager>>,
    ) -> Self {
        Self {
            capture,
            remote_input,
        }
    }
}

impl HostMediaFactory for Factory {
    fn start(
        &self,
        cancel: CancellationToken,
        options: HostOptions,
    ) -> BoxFuture<'_, MediaResult<Arc<dyn HostMedia>>> {
        Box::pin(async move {
            // Input exists only for a share whose page asked for it.
            let remote_input = options
                .remote_input
                .then(|| self.remote_input.clone())
                .flatten();
            let session = Session::start(
                &cancel,
                Options {
                    share_id: options.share_id,
                    capture: self.capture.clone(),
                    video: options.video,
                    profile: options.profile,
                    audio_enabled: options.audio_enabled,
                    exclude_audio: options.exclude_audio,
                    microphone_mixing: options.microphone_mixing,
                    edge_capacity: options.edge_capacity,
                    bind_address: String::new(),
                    port_mapping: options.port_mapping,
                    remote_input,
                    events: Arc::new(options.events),
                },
            )
            .await?;
            Ok(Arc::new(session) as Arc<dyn HostMedia>)
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
        Box::pin(Session::add_candidate(
            self,
            connection_id,
            candidate.map(bridge::edge_candidate),
        ))
    }

    fn close_edge<'a>(&'a self, connection_id: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(Session::close_edge(self, connection_id))
    }

    fn punch(&self, connection_id: &str) -> Option<Arc<dyn PunchPreparation>> {
        Session::punch(self, connection_id).map(bridge::punch)
    }

    fn close_punch(&self, connection_id: &str) {
        Session::close_punch(self, connection_id);
    }
}

impl HostMedia for Session {
    fn codec(&self) -> String {
        Session::codec(self)
    }

    fn has_audio(&self) -> bool {
        Session::has_audio(self)
    }

    fn source_audio(&self) -> Option<bool> {
        Session::source_audio(self)
    }

    fn prepare_edge<'a>(
        &'a self,
        connection_id: &'a str,
        ice_servers: Vec<IceServerUrls>,
        punched: bool,
    ) -> BoxFuture<'a, MediaResult<String>> {
        Box::pin(Session::prepare_edge(
            self,
            connection_id,
            ice_servers,
            punched,
        ))
    }

    fn prepare_local_edge<'a>(
        &'a self,
        connection_id: &'a str,
        offer_id: &'a str,
        restart: bool,
    ) -> BoxFuture<'a, MediaResult<String>> {
        Box::pin(Session::prepare_local_edge(
            self,
            connection_id,
            offer_id,
            restart,
        ))
    }

    fn begin_punch(
        &self,
        connection_id: &str,
        ice_servers: Vec<IceServerUrls>,
        secret: &str,
    ) -> MediaResult<Arc<dyn PunchPreparation>> {
        Session::begin_punch(self, connection_id, ice_servers, secret).map(bridge::punch)
    }

    fn set_paused(&self, paused: bool) {
        Session::set_paused(self, paused);
    }

    fn update_profile(&self, profile: QualityProfile) -> BoxFuture<'_, MediaResult<()>> {
        Box::pin(Session::update_profile(self, profile))
    }

    fn replace_source(
        &self,
        options: VideoOptions,
        audio_enabled: bool,
        exclude_audio: Option<CaptureTarget>,
    ) -> BoxFuture<'_, MediaResult<()>> {
        Box::pin(Session::replace_source(
            self,
            options,
            audio_enabled,
            exclude_audio,
        ))
    }

    fn set_microphone(
        &self,
        enabled: Option<bool>,
        gain: Option<f64>,
        device_id: Option<String>,
    ) -> BoxFuture<'_, MediaResult<()>> {
        Box::pin(Session::set_microphone(self, enabled, gain, device_id))
    }

    /// The volume applies on the engine; the call returns once validated.
    fn set_volume(&self, volume: f64) -> MediaResult<()> {
        if !volume.is_finite() || !(0.0..=2.0).contains(&volume) || !self.has_audio() {
            return Err(piik_native_control::media::MediaError::failed(
                "native source volume is invalid",
            ));
        }
        Session::set_volume(self, volume)
    }

    fn remote_input_changed(&self) -> BoxFuture<'_, ()> {
        Box::pin(Session::remote_input_changed(self))
    }

    fn remote_input_state(&self) -> piik_remote_input::State {
        Session::remote_input_state(self)
    }

    fn prepare_remote_input_policy(
        &self,
        mode: &str,
        password: &str,
        co_play: bool,
    ) -> MediaResult<PolicyApply> {
        Session::prepare_remote_input_policy(self, mode, password, co_play)
    }

    fn set_remote_input_peers(&self, peers: &[RemoteInputPeer]) -> MediaResult<()> {
        Session::set_remote_input_peers(self, peers)
    }

    fn decide_remote_input(&self, request_id: &str, allow: bool) -> MediaResult<()> {
        Session::decide_remote_input(self, request_id, allow)
    }

    fn set_remote_input_simultaneous(&self, enabled: bool) -> MediaResult<()> {
        Session::set_remote_input_simultaneous(self, enabled)
    }

    fn prepare_publication<'a>(
        &'a self,
        generation: &'a str,
        connection_id: &'a str,
        ice_servers: Vec<IceServerUrls>,
    ) -> BoxFuture<'a, MediaResult<(String, piik_native_control::media::PublicationMedia)>> {
        Box::pin(Session::prepare_publication(
            self,
            generation,
            connection_id,
            ice_servers,
        ))
    }

    fn publication(
        &self,
        generation: &str,
        connection_id: &str,
    ) -> MediaResult<Arc<dyn piik_native_control::media::Publication>> {
        Session::publication(self, generation, connection_id)
            .map(|publication| publication as Arc<dyn piik_native_control::media::Publication>)
    }

    fn close_publication(&self, generation: &str, connection_id: &str) {
        let session = self.clone();
        let (generation, connection_id) = (generation.to_owned(), connection_id.to_owned());
        // Removal is synchronous so the identity is free at once; the
        // transport retires behind it.
        let owned = session
            .inner
            .lock()
            .publications
            .remove(&(generation, connection_id));
        if let Some(owned) = owned {
            tokio::spawn(async move { owned.publication.close().await });
        }
    }

    fn done(&self) -> BoxFuture<'_, Option<HostEnded>> {
        Box::pin(Session::done(self))
    }

    fn close(&self) -> BoxFuture<'_, MediaResult<()>> {
        Box::pin(Session::close_session(self))
    }
}
