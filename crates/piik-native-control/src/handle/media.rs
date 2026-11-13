//! Share capture, microphone and source-update commands.

use std::sync::Arc;

use piik_loopback::{CancellationToken, ControlEvent};
use piik_native_capture::VideoOptions;
use serde_json::Value;

use crate::events::to_value;
use crate::media::{HostOptions, MediaError, QualityProfile};
use crate::session::{Fail, Inner, Request, fail};
use crate::wire::*;

use super::{Handled, edge_capacity, no_share, ok, strict, value};

impl Inner {
    pub(super) fn handle_start_share(
        self: &Arc<Self>,
        request: &mut Request,
        payload: &[u8],
    ) -> Handled {
        const INVALID: &str = "native start-share request is invalid";
        let start: StartShareRequest = strict(payload, INVALID)?;
        let capacity = edge_capacity(start.edge_capacity)
            .filter(|_| {
                valid_identity(&start.share_id)
                    && start.source.valid()
                    && matches!(start.codec.as_str(), "auto" | "h264" | "vp8")
            })
            .ok_or(Fail::Violation(INVALID))?;
        let profile = start
            .profile
            .as_ref()
            .map(QualityProfile::from_settings)
            .unwrap_or_default();
        if !profile.valid() {
            return Err(fail("native sharing profile is unsupported"));
        }
        self.validate_audio_exclusion(&start.source, start.audio, start.exclude_audio.as_ref())?;
        if start.exclude_audio.is_some() && !start.microphone_mixing {
            return Err(fail("native audio exclusion requires mixed audio"));
        }
        tracing::debug!(
            target: "piik_native_control",
            event = "native-profile-requested",
            request_id = %request.id,
            share = %piik_diagnostics::id(&start.share_id),
            profile = ?profile,
            codec = %start.codec,
            audio = start.audio,
            source_kind = %start.source.kind,
            adapter_index = start.adapter_index,
            encoder_index = start.encoder_index,
            "piik-client"
        );
        request.report_source_cancellation = start.report_source_cancellation;
        // Startup has no Host yet to cancel. Reserve the mutation slot, and
        // transfer the start token to the Host watcher once installed.
        let cancel = self.cancel.child_token();
        let Some(slot) = self.begin_start(&start.share_id, &cancel) else {
            return Err(fail("native share is unavailable or already active"));
        };
        let inner = self.clone();
        let request = request.clone();
        tokio::spawn(async move {
            let result = inner
                .start_share(&request, start, profile, capacity, &cancel)
                .await;
            if result.is_err() {
                cancel.cancel();
            }
            let value = request.complete(result);
            inner.end_start();
            inner
                .finish(crate::session::SlotKind::Update, slot, value)
                .await;
        });
        Ok(None)
    }

    pub(super) async fn start_share(
        self: &Arc<Self>,
        request: &Request,
        start: StartShareRequest,
        profile: QualityProfile,
        edge_capacity: usize,
        cancel: &CancellationToken,
    ) -> Result<Value, MediaError> {
        let capabilities = &self.config.capabilities;
        if start.codec == "vp8" && !capabilities.software_vp8 {
            return Err(MediaError::failed("native VP8 encoding is unavailable"));
        }
        {
            let state = self.lock();
            if state.closed
                || cancel.is_cancelled()
                || state.host.is_some()
                || state.viewer.is_some()
            {
                return Err(MediaError::failed("native share is already active"));
            }
        }
        let options = HostOptions {
            share_id: start.share_id.clone(),
            video: VideoOptions {
                target: start.source.clone(),
                show_capture_border: start.show_capture_border
                    && capabilities.capture_border_control,
                codec: start.codec.clone(),
                adapter_index: start.adapter_index,
                encoder_index: start.encoder_index,
                profile: profile.video.clone(),
                ..VideoOptions::default()
            },
            audio_enabled: start.audio && capabilities.summary().audio_for(&start.source.kind),
            profile: profile.clone(),
            exclude_audio: start.exclude_audio,
            microphone_mixing: start.microphone_mixing,
            edge_capacity,
            port_mapping: self.config.port_mapping,
            // Input events exist only for a share that asked for them.
            remote_input: start.remote_input && self.config.remote_input.is_some(),
            events: self.media_events(),
        };
        let host = self.config.host.start(cancel.clone(), options).await?;
        if !self.install_host(&host, cancel) {
            let _ = host.close().await;
            return Err(MediaError::failed("native share is unavailable"));
        }
        let codec = host.codec();
        tracing::debug!(
            target: "piik_native_control",
            event = "native-profile-applied",
            request_id = %request.id,
            share = %piik_diagnostics::id(&start.share_id),
            profile = ?profile,
            codec = %codec,
            audio = host.has_audio(),
            "piik-client"
        );
        tokio::spawn(self.clone().watch_host(
            host.clone(),
            start.report_capture_failure,
            cancel.clone(),
        ));
        Ok(to_value(&ShareStartedResponse {
            envelope: request.envelope("share-started"),
            share_id: start.share_id,
            audio: host.has_audio(),
            source_audio: host.source_audio(),
            codec,
        }))
    }

    pub(super) fn handle_stop_shortcut(&self, request: &Request, payload: &[u8]) -> Handled {
        const INVALID: &str = "native remote-control shortcut is invalid";
        let shortcut: RemoteStopShortcutRequest = strict(payload, INVALID)?;
        let get = request.kind == "get-remote-stop-shortcut";
        let key = i32::try_from(shortcut.key).ok();
        let valid = if get {
            shortcut.key == 0
        } else {
            key.is_some_and(piik_remote_input::valid_stop_key)
        };
        if !valid_identity(&shortcut.share_id) || !valid {
            return Err(Fail::Violation(INVALID));
        }
        let manager = self
            .config
            .remote_input
            .as_ref()
            .filter(|manager| {
                self.current(&shortcut.share_id).is_some() && manager.supports_stop_shortcut()
            })
            .ok_or_else(|| fail("native remote-control shortcut is unavailable"))?;
        let state = if get {
            manager.read_stop_shortcut()?
        } else {
            manager.set_stop_shortcut(key.unwrap_or_default())?
        };
        value(RemoteStopShortcutResponse {
            envelope: request.envelope("remote-stop-shortcut"),
            shortcut: state,
        })
    }

    pub(super) async fn handle_microphone(
        self: &Arc<Self>,
        request: &Request,
        payload: &[u8],
    ) -> Handled {
        const INVALID: &str = "native microphone request is invalid";
        let microphone: MicrophoneRequest = strict(payload, INVALID)?;
        if !valid_identity(&microphone.share_id)
            || (microphone.enabled.is_none()
                && microphone.volume.is_none()
                && microphone.device_id.is_none())
            || microphone
                .device_id
                .as_deref()
                .is_some_and(|id| !piik_native_capture::valid_device_id(id))
            || microphone
                .volume
                .is_some_and(|volume| !volume.is_finite() || !(0.0..=2.0).contains(&volume))
        {
            return Err(Fail::Violation(INVALID));
        }
        let host = self.current(&microphone.share_id).ok_or_else(no_share)?;
        // Commit gain in command order; asynchronous device setup must not
        // later overwrite a newer slider value received while it was pending.
        if microphone.volume.is_some() {
            host.set_microphone(None, microphone.volume, None).await?;
        }
        if microphone.enabled.is_none() && microphone.device_id.is_none() {
            return ok(request.ack("microphone-set"));
        }
        let ack = request.response("microphone-set");
        let target = host.clone();
        Ok(self.run_host_operation(&host, request, move || {
            Ok(Box::pin(async move {
                target
                    .set_microphone(microphone.enabled, None, microphone.device_id)
                    .await
                    .map(|()| ack)
            }))
        }))
    }

    pub(super) async fn handle_update_share(
        self: &Arc<Self>,
        request: &Request,
        payload: &[u8],
    ) -> Handled {
        const INVALID: &str = "native update-share request is invalid";
        let update: UpdateShareRequest = strict(payload, INVALID)?;
        if !valid_identity(&update.share_id) {
            return Err(Fail::Violation(INVALID));
        }
        let profile = update
            .profile
            .as_ref()
            .map(QualityProfile::from_settings)
            .unwrap_or_default();
        if !profile.valid() {
            return Err(fail("native sharing profile is unsupported"));
        }
        tracing::debug!(
            target: "piik_native_control",
            event = "native-profile-requested",
            request_id = %request.id,
            share = %piik_diagnostics::id(&update.share_id),
            profile = ?profile,
            "piik-client"
        );
        let updated = to_value(&ShareResponse {
            envelope: request.envelope("share-updated"),
            share_id: update.share_id.clone(),
        });
        if let Some(host) = self.current(&update.share_id) {
            let target = host.clone();
            return Ok(self.run_host_operation(&host, request, move || {
                Ok(Box::pin(async move {
                    target.update_profile(profile).await.map(|()| updated)
                }))
            }));
        }
        let viewer = self.current_viewer(&update.share_id).ok_or_else(no_share)?;
        viewer.update_profile(profile.video).await?;
        ok(ControlEvent::new(updated))
    }

    pub(super) fn handle_replace_source(
        self: &Arc<Self>,
        request: &mut Request,
        payload: &[u8],
    ) -> Handled {
        const INVALID: &str = "native replace-share-source request is invalid";
        let replace: ReplaceShareSourceRequest = strict(payload, INVALID)?;
        if !valid_identity(&replace.share_id) || !replace.source.valid() {
            return Err(Fail::Violation(INVALID));
        }
        request.report_source_cancellation = replace.report_source_cancellation;
        self.validate_audio_exclusion(
            &replace.source,
            replace.audio,
            replace.exclude_audio.as_ref(),
        )?;
        let host = self.current(&replace.share_id).ok_or_else(no_share)?;
        let capabilities = &self.config.capabilities;
        let audio = replace.audio && capabilities.summary().audio_for(&replace.source.kind);
        tracing::debug!(
            target: "piik_native_control",
            event = "native-source-requested",
            request_id = %request.id,
            share = %piik_diagnostics::id(&replace.share_id),
            source_kind = %replace.source.kind,
            audio,
            adapter_index = replace.adapter_index,
            encoder_index = replace.encoder_index,
            "piik-client"
        );
        let options = VideoOptions {
            target: replace.source,
            show_capture_border: replace.show_capture_border && capabilities.capture_border_control,
            adapter_index: replace.adapter_index,
            encoder_index: replace.encoder_index,
            ..VideoOptions::default()
        };
        let replaced = to_value(&ShareResponse {
            envelope: request.envelope("share-source-replaced"),
            share_id: replace.share_id,
        });
        let target = host.clone();
        let exclude = replace.exclude_audio;
        Ok(self.run_host_operation(&host, request, move || {
            Ok(Box::pin(async move {
                target
                    .replace_source(options, audio, exclude)
                    .await
                    .map(|()| replaced)
            }))
        }))
    }
}
