//! The share's control-facing API: media queries, edge and punch handling,
//! profile/source updates, remote-input policy, and shutdown.

use std::sync::{Arc, Weak};

use piik_native_capture::{CaptureTarget, VideoOptions};
use piik_native_control::media::{HostEnded, MediaError, MediaResult, QualityProfile};

use super::{Inner, InputSession, Session, canceled, unavailable};
use crate::bridge;
impl Session {
    pub fn share_id(&self) -> &str {
        &self.inner.share_id
    }

    pub fn codec(&self) -> String {
        let state = self.inner.lock();
        state
            .source
            .as_ref()
            .map_or_else(String::new, |source| source.codec().to_owned())
    }

    pub fn has_audio(&self) -> bool {
        let state = self.inner.lock();
        !state.closed && state.audio_source.is_some()
    }

    /// Optional facts emitted only to a page which opted into mixed audio.
    pub fn source_audio(&self) -> Option<bool> {
        self.inner.mixer().map(|mixer| mixer.has_source())
    }

    pub async fn prepare_edge(
        &self,
        connection_id: &str,
        ice_servers: Vec<piik_media_edge::IceServerUrls>,
        punched: bool,
    ) -> MediaResult<String> {
        self.inner
            .prepare_edge(connection_id, ice_servers, "", punched)
            .await
    }

    pub async fn prepare_local_edge(
        &self,
        connection_id: &str,
        offer_id: &str,
        restart: bool,
    ) -> MediaResult<String> {
        if restart {
            let edge = self
                .inner
                .edge(connection_id)
                .ok_or_else(|| MediaError::failed("native local media edge does not exist"))?;
            return edge
                .create_local_offer(offer_id, true)
                .await
                .map_err(bridge::media_error);
        }
        self.inner
            .prepare_edge(connection_id, Vec::new(), offer_id, false)
            .await
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
            .ok_or_else(|| MediaError::failed("native media edge does not exist"))?;
        edge.set_answer(sdp, offer_id)
            .await
            .map_err(bridge::media_error)
    }

    /// Candidates can arrive after an edge has been retired. They are
    /// disposable input, not a reason to tear down the share.
    pub async fn add_candidate(
        &self,
        connection_id: &str,
        candidate: Option<piik_media_edge::IceCandidateInit>,
    ) -> MediaResult<()> {
        let Some(edge) = self.inner.edge(connection_id) else {
            return Ok(());
        };
        edge.add_remote_candidate(candidate)
            .await
            .map_err(bridge::media_error)
    }

    pub async fn close_edge(&self, connection_id: &str) {
        self.inner.close_edge(connection_id).await;
    }

    pub fn begin_punch(
        &self,
        connection_id: &str,
        ice_servers: Vec<piik_media_edge::IceServerUrls>,
        secret: &str,
    ) -> MediaResult<piik_media_edge::PunchPreparation> {
        let state = self.inner.lock();
        let source = state.source.clone();
        if state.closed || source.is_none() || state.edges.contains_key(connection_id) {
            return Err(unavailable());
        }
        // Held across admission so an edge cannot claim the identity meanwhile.
        self.inner
            .engine
            .begin_punch(connection_id, source.as_ref(), ice_servers, secret)
            .map_err(bridge::media_error)
    }

    pub fn punch(&self, connection_id: &str) -> Option<piik_media_edge::PunchPreparation> {
        self.inner.engine.punch(connection_id)
    }

    pub fn close_punch(&self, connection_id: &str) {
        self.inner.engine.close_punch(connection_id);
    }

    pub fn set_paused(&self, paused: bool) {
        let inner = self.inner.clone();
        if let Some(input) = &inner.remote_input {
            input.pause(paused);
        }
        let source = {
            let mut state = inner.lock();
            if state.closed {
                return;
            }
            state.paused = paused;
            if let Some(mixer) = inner.mixer() {
                mixer.set_paused(paused);
            }
            state.source.clone()
        };
        if !paused && let Some(source) = source {
            tokio::spawn(async move { source.request_recovery_frame().await });
        }
    }

    pub async fn update_profile(&self, profile: QualityProfile) -> MediaResult<()> {
        let inner = &self.inner;
        if !profile.valid() {
            return Err(MediaError::failed("native share profile is invalid"));
        }
        let _update = inner.update.lock().await;
        let (previous, mut options, audio_source) = {
            let state = inner.lock();
            if state.closed || inner.ctx.is_cancelled() {
                return Err(unavailable());
            }
            if state.profile == profile {
                return Ok(());
            }
            (
                state.profile.clone(),
                state.video_options.clone(),
                state.audio_source.clone(),
            )
        };
        options.profile = profile.video.clone();
        let audio_changed = profile.audio_bitrate != previous.audio_bitrate;
        if profile.video == previous.video {
            if let Some(audio) = audio_source.as_ref().filter(|_| audio_changed) {
                audio
                    .set_bitrate(profile.audio_bitrate)
                    .await
                    .map_err(bridge::media_error)?;
            }
            let mut state = inner.lock();
            if state.closed || inner.ctx.is_cancelled() {
                return Err(unavailable());
            }
            state.profile = profile;
            return Ok(());
        }
        // Updating an existing source never waits for an interactive picker.
        let (replacement, capture_state) = inner
            .prepare_video(&inner.ctx, options.clone(), false, true)
            .await?;
        if let Some(audio) = audio_source.as_ref().filter(|_| audio_changed)
            && let Err(error) = audio.set_bitrate(profile.audio_bitrate).await
        {
            let _ = replacement.close().await;
            return Err(bridge::media_error(error));
        }
        inner
            .commit_capture(
                options,
                profile,
                Some((replacement, capture_state)),
                None,
                false,
            )
            .await
    }

    pub async fn replace_source(
        &self,
        mut options: VideoOptions,
        audio_enabled: bool,
        exclude_audio: Option<CaptureTarget>,
    ) -> MediaResult<()> {
        let inner = &self.inner;
        let _update = inner.update.lock().await;
        let (profile, has_audio, audio_only) = {
            let state = inner.lock();
            if state.closed {
                return Err(unavailable());
            }
            let current = &state.video_options;
            options.codec = current.codec.clone();
            options.adapter_index = current.adapter_index;
            options.encoder_index = current.encoder_index;
            options.output_groups = inner
                .edge_capacity
                .min(piik_native_capture::MAX_OUTPUT_GROUPS);
            let target = &current.target;
            let audio_only = options.target.kind != "picker"
                && options.target.kind == target.kind
                && options.target.source_id == target.source_id
                && options.target.pid == target.pid
                && options.target.creation_time == target.creation_time
                && options.show_capture_border == current.show_capture_border;
            (
                state.profile.clone(),
                state.audio_source.is_some() && state.audio_stream.is_some(),
                audio_only,
            )
        };
        let mixer = inner.mixer();
        if mixer.is_none() && audio_enabled != has_audio {
            return Err(MediaError::failed(
                "native source audio availability cannot change while sharing",
            ));
        }
        if audio_enabled && !inner.video_active.is_cancelled() {
            return Err(MediaError::failed(
                "native source audio is unavailable until video becomes active",
            ));
        }
        if exclude_audio.is_some() {
            let Some(mixer) = &mixer else {
                return Err(MediaError::failed(
                    "native audio exclusion requires mixed audio",
                ));
            };
            // A privacy change retires the old source before preparation.
            // Failure must leave it silent rather than retain unfiltered
            // audio; video and microphone stay live.
            let previous = inner.lock().audio_stream.take();
            mixer.set_source(None);
            if let Some(previous) = previous {
                let _ = previous.close().await;
            }
        }
        options.profile = profile.video.clone();
        // Admit audio before candidate video can fill its unread queue and
        // block a platform handler that also starts audio.
        let replacement_audio = if audio_enabled {
            match inner
                .start_source_audio(&inner.ctx, &options.target, exclude_audio)
                .await
            {
                Ok(stream) => Some(stream),
                Err(_) => return Err(MediaError::failed("native source audio could not start")),
            }
        } else {
            None
        };
        let mut replacement = None;
        if !audio_only {
            let interactive = options.target.kind == "picker";
            match inner
                .prepare_video(&inner.ctx, options.clone(), interactive, false)
                .await
            {
                Ok(prepared) => replacement = Some(prepared),
                Err(error) => {
                    if let Some(audio) = replacement_audio {
                        let _ = audio.close().await;
                    }
                    return Err(error);
                }
            }
            // Preparation still belonged to the old source. Retire its
            // authorization only once the replacement enters the commit.
            inner.clear_remote_input_source();
        }
        let result = inner
            .commit_capture(options, profile, replacement, replacement_audio, true)
            .await;
        if !audio_only {
            inner.sync_remote_input_source();
        }
        result
    }

    pub async fn set_microphone(
        &self,
        enabled: Option<bool>,
        gain: Option<f64>,
        device_id: Option<String>,
    ) -> MediaResult<()> {
        let inner = &self.inner;
        let _update = if enabled.is_some() || device_id.is_some() {
            Some(inner.update.lock().await)
        } else {
            None
        };
        let Some(mixer) = inner.mixer().filter(|_| !inner.ctx.is_cancelled()) else {
            return Err(MediaError::failed("native microphone is unavailable"));
        };
        mixer
            .set_microphone(inner.capture.as_ref(), enabled, gain, device_id)
            .await
    }

    /// Validated synchronously; the share's volume task applies the latest
    /// value to the encoder in order.
    pub fn set_volume(&self, volume: f64) -> MediaResult<()> {
        if self.inner.ctx.is_cancelled() {
            return Err(canceled());
        }
        let has_audio = self.inner.lock().audio_source.is_some();
        if !has_audio || !volume.is_finite() || !(0.0..=2.0).contains(&volume) {
            return Err(MediaError::failed("native source volume is invalid"));
        }
        self.inner.volume.send_replace(volume);
        Ok(())
    }

    /// Resolves on the next remote-input change; never without a session.
    pub async fn remote_input_changed(&self) {
        match &self.inner.remote_input {
            Some(input) => input.changed().await,
            None => std::future::pending().await,
        }
    }

    pub fn remote_input_state(&self) -> piik_remote_input::State {
        self.inner
            .remote_input
            .as_ref()
            .map(InputSession::state)
            .unwrap_or_default()
    }

    fn input(&self) -> MediaResult<&InputSession> {
        self.inner
            .remote_input
            .as_ref()
            .ok_or(MediaError::RemoteInput(
                piik_remote_input::Error::Unavailable,
            ))
    }

    /// Reserves the intent synchronously, before asynchronous OS work. On
    /// Windows the capture never owns input permission, so applying the
    /// change needs no capture replacement.
    pub fn prepare_remote_input_policy(
        &self,
        mode: &str,
        password: &str,
        co_play: bool,
    ) -> MediaResult<piik_native_control::media::PolicyApply> {
        let change = self
            .input()?
            .begin_policy(&self.inner.ctx, mode, password, co_play)?;
        Ok(Box::pin(async move { Ok(change.apply()?) }))
    }

    pub fn set_remote_input_peers(
        &self,
        peers: &[piik_protocol::RemoteInputPeer],
    ) -> MediaResult<()> {
        Ok(self.input()?.set_peers(peers)?)
    }

    pub fn decide_remote_input(&self, request_id: &str, allow: bool) -> MediaResult<()> {
        Ok(self.input()?.decide(request_id, allow)?)
    }

    pub fn set_remote_input_simultaneous(&self, enabled: bool) -> MediaResult<()> {
        Ok(self.input()?.set_simultaneous(enabled)?)
    }

    /// Resolves once when the share ends. `None` means its owner closed it.
    pub async fn done(&self) -> Option<HostEnded> {
        self.inner.wait_ended().await;
        self.inner
            .ended_result
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
            .flatten()
    }

    pub async fn close_session(&self) -> MediaResult<()> {
        let inner = &self.inner;
        inner.ctx.cancel();
        let first = {
            let _update = inner.update.lock().await;
            let mut state = inner.lock();
            let first = !state.closed;
            state.closed = true;
            // The engine retires the edges themselves.
            state.edges.clear();
            first
            // Closed and cancelled reject further updates. Pending startup must
            // acquire the update lock and finish before run can retire.
        };
        if first {
            inner.retire().await;
        }
        inner.wait_ended().await;
        Ok(())
    }

    /// The SFU publications are in `publication.rs`.
    pub(crate) fn weak(&self) -> Weak<Inner> {
        Arc::downgrade(&self.inner)
    }
}
