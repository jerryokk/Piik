//! The share's single video reader: reads the installed capture, installs
//! replacements and recovers the device, and reports the startup result.

use std::sync::Arc;
use std::time::Duration;

use piik_native_control::media::{MediaError, MediaEventKind, MediaResult};
use tokio::sync::oneshot;

use super::capture::{RECOVERABLE_HRESULTS, apply_output_plan, configure_capture_outputs};
use super::{Inner, canceled, unavailable};
use crate::bridge;
use crate::capture::{SharedStream, same};

impl Inner {
    pub(super) async fn disable_capture_layer(
        &self,
        stream: &SharedStream,
        layer: usize,
    ) -> MediaResult<()> {
        let source = {
            let state = self.lock();
            if !Arc::ptr_eq(&state.stream, stream) || state.closed || self.ctx.is_cancelled() {
                return Ok(());
            }
            state.source.clone()
        };
        let Some(source) = source else { return Ok(()) };
        source
            .mark_layer_unavailable(layer)
            .await
            .map_err(bridge::media_error)
    }
}
type Ready = Option<oneshot::Sender<MediaResult<()>>>;

/// The single video reader's state.
struct VideoReader {
    current: Option<SharedStream>,
    ready: Ready,
    active_seen: bool,
}

impl Inner {
    fn mark_active(&self, reader: &mut VideoReader) {
        if !reader.active_seen {
            self.video_active.cancel();
        }
        reader.active_seen = true;
    }

    /// A final frame or control error can arrive before the process waiter
    /// joins: keep that error and the selected helper's exit cause.
    async fn fail(&self, reader: &mut VideoReader, error: MediaError) -> MediaError {
        let error = match &reader.current {
            Some(current) => bridge::join_close(error, current.close().await),
            None => error,
        };
        match reader.ready.take() {
            // Start reports this cause; the share never started.
            Some(ready) => {
                let _ = ready.send(Err(error));
                MediaError::failed("native capture did not start")
            }
            None => error,
        }
    }

    async fn change_stream(&self, reader: &mut VideoReader, next: SharedStream) -> MediaResult<()> {
        self.install_capture(&next).await?;
        reader.current = Some(next);
        // prepareVideo consumed and verified this replacement's active status.
        self.mark_active(reader);
        Ok(())
    }

    pub(crate) async fn run_video(
        self: &Arc<Self>,
        ready: oneshot::Sender<MediaResult<()>>,
    ) -> MediaResult<()> {
        let mut reader = VideoReader {
            current: self.current_stream(),
            ready: Some(ready),
            active_seen: false,
        };
        loop {
            let error = self.read_frames(&mut reader).await;
            let recoverable = error
                .failure_details()
                .is_some_and(|failure| RECOVERABLE_HRESULTS.contains(&failure.hresult.as_str()));
            if !reader.active_seen
                || self.ctx.is_cancelled()
                || error.source_canceled()
                || !recoverable
            {
                return Err(error);
            }
            if let Some(failure) = error.failure_details() {
                tracing::debug!(event = "capture-device-recovering",
                    share = %piik_diagnostics::id(&self.share_id), stage = %failure.stage,
                    hresult = %failure.hresult, "piik-client");
            }
            let Some(failed) = reader.current.clone() else {
                return Err(error);
            };
            let recovery = {
                let inner = self.clone();
                tokio::spawn(async move { inner.prepare_capture_recovery(failed).await })
            };
            tokio::pin!(recovery);
            // Updates hold the operation lock until this reader installs their
            // metadata. Keep serving that handoff while recovery waits.
            let recovered = loop {
                tokio::select! {
                    result = &mut recovery => {
                        break result.unwrap_or_else(|_| Err(unavailable()));
                    }
                    () = self.capture_changed.notified() => {
                        let next = self.current_stream();
                        if let Some(next) = next.filter(|next| !same(Some(next), reader.current.as_ref()))
                            && let Err(change) = self.change_stream(&mut reader, next).await
                        {
                            self.ctx.cancel();
                            let _ = (&mut recovery).await;
                            break Err(change);
                        }
                    }
                    () = self.ctx.cancelled() => {
                        let _ = (&mut recovery).await;
                        break Err(canceled());
                    }
                }
            };
            if let Err(recovery) = recovered {
                return Err(bridge::join(error, recovery));
            }
        }
    }

    /// Reads the current capture until it fails. Returns the failure.
    async fn read_frames(self: &Arc<Self>, reader: &mut VideoReader) -> MediaError {
        use piik_native_capture::FrameKind;
        while let Some(current) = reader.current.clone() {
            let frame = match current.read().await {
                Ok(frame) => frame,
                Err(read) => {
                    // Retirement joins the helper's exit and its failure codes.
                    // Recheck ownership afterward: a replacement can commit
                    // during the close.
                    let closed = current.close().await;
                    let next = self.current_stream();
                    if let Some(next) = next.filter(|next| !Arc::ptr_eq(next, &current)) {
                        if let Err(error) = self.change_stream(reader, next).await {
                            return self.fail(reader, error).await;
                        }
                        continue;
                    }
                    tracing::debug!(event = "capture-video-read-ended",
                        share = %piik_diagnostics::id(&self.share_id),
                        canceled = self.ctx.is_cancelled(),
                        eof = matches!(read, piik_native_capture::Error::Eof),
                        error = %piik_diagnostics::safe_text(&read.to_string()), "piik-client");
                    let error = if matches!(read, piik_native_capture::Error::SourceCanceled) {
                        MediaError::SourceCanceled
                    } else {
                        MediaError::failed(format!("native capture stopped unexpectedly\n{read}"))
                    };
                    return self.fail(reader, bridge::join_close(error, closed)).await;
                }
            };
            let next = self.current_stream();
            if !same(next.as_ref(), Some(&current)) {
                if let Some(next) = next
                    && let Err(error) = self.change_stream(reader, next).await
                {
                    return self.fail(reader, error).await;
                }
                if self.current_stream().is_none() {
                    reader.current = None;
                }
                continue;
            }
            let result = match frame.kind {
                FrameKind::Status => self.read_status(reader, &current, &frame.data).await,
                FrameKind::Begin => self.read_begin(reader, &current, frame.timestamp).await,
                FrameKind::H264 | FrameKind::Vp8 => self.read_video(frame).await,
                FrameKind::LayerUnavailable => {
                    tracing::debug!(event = "capture-output-unavailable",
                        share = %piik_diagnostics::id(&self.share_id), layer = frame.layer,
                        detail = %piik_diagnostics::safe_text(&String::from_utf8_lossy(&frame.data)),
                        "piik-client");
                    if self.lock().source.is_none() {
                        Err(MediaError::failed(
                            "native output ended before its source state",
                        ))
                    } else {
                        self.disable_capture_layer(&current, usize::from(frame.layer))
                            .await
                    }
                }
                FrameKind::Pcm => Err(MediaError::failed("native video process emitted audio")),
                FrameKind::Control => Err(MediaError::failed(
                    "native video process emitted an unknown frame",
                )),
            };
            if let Err(error) = result {
                return self.fail(reader, error).await;
            }
        }
        let error = MediaError::failed("native capture stopped unexpectedly");
        self.fail(reader, error).await
    }

    async fn read_status(
        self: &Arc<Self>,
        reader: &mut VideoReader,
        current: &SharedStream,
        data: &[u8],
    ) -> MediaResult<()> {
        let status = crate::capture::decode_capture_state(data).map_err(MediaError::failed)?;
        if status.outputs != current.outputs() {
            return Err(MediaError::failed(
                "native capture outputs were not applied",
            ));
        }
        let (needs_source, codec) = {
            let state = self.lock();
            if state.closed {
                return Err(MediaError::failed("native share stopped"));
            }
            if !Arc::ptr_eq(&state.stream, current) {
                return Ok(());
            }
            (state.source.is_none(), state.video_options.codec.clone())
        };
        if needs_source {
            if codec != "auto" && codec != status.codec {
                return Err(MediaError::failed("native capture codec was not applied"));
            }
            let weak = Arc::downgrade(self);
            let recovery: piik_media_edge::RecoveryCallback = Arc::new(move |layers: Vec<i32>| {
                let Some(inner) = weak.upgrade() else { return };
                let Some(stream) = inner.current_stream() else {
                    return;
                };
                tokio::spawn(async move {
                    for layer in layers {
                        let _ = stream.request_key_frame(layer).await;
                    }
                });
            });
            let capacity = piik_media_edge::physical_source_capacity(self.edge_capacity);
            let source = self
                .engine
                .new_source(
                    &status.codec,
                    capacity,
                    status.outputs.len().min(2),
                    Some(recovery),
                )
                .await
                .map_err(bridge::media_error)?;
            if let Err(error) = configure_capture_outputs(&source, &status.outputs).await {
                source.close().await;
                return Err(error);
            }
            let installed = {
                let mut state = self.lock();
                if state.closed {
                    false
                } else {
                    state.source = Some(source.clone());
                    state.video_options.codec = status.codec.clone();
                    true
                }
            };
            if !installed {
                source.close().await;
                return Err(MediaError::failed("native share stopped"));
            }
        } else {
            let state = self.lock();
            if state
                .source
                .as_ref()
                .is_some_and(|source| source.codec() != status.codec)
            {
                return Err(MediaError::failed(
                    "native capture codec changed within a share",
                ));
            }
        }
        {
            let mut state = self.lock();
            status.apply_backend(&mut state.video_options);
        }
        if status.state == "active" {
            self.mark_active(reader);
        }
        tracing::debug!(event = "capture-state", state = %status.state, codec = %status.codec,
            share = %piik_diagnostics::id(&self.share_id), width = status.width,
            height = status.height, fps = status.fps, hardware = status.hardware_only,
            adapterIndex = ?status.adapter_index, encoderIndex = ?status.encoder_index,
            adapterName = %piik_diagnostics::safe_text(&status.adapter_name),
            encoderName = %piik_diagnostics::safe_text(&status.encoder_name),
            profileLevelId = %status.profile_level_id, "piik-client");
        self.emit(
            MediaEventKind::CaptureState {
                state: status.state,
            },
            None,
        );
        if let Some(ready) = reader.ready.take() {
            let _ = ready.send(Ok(()));
        }
        Ok(())
    }

    async fn read_begin(
        &self,
        reader: &VideoReader,
        current: &SharedStream,
        timestamp: Duration,
    ) -> MediaResult<()> {
        let Some(source) = self.lock().source.clone() else {
            return Err(MediaError::failed(
                "native capture input arrived before its state",
            ));
        };
        let mut plan = source
            .begin_frame(timestamp)
            .await
            .map_err(bridge::media_error)?;
        if self.lock().paused {
            plan.active.fill(false);
        }
        let control = apply_output_plan(current, &plan, reader.active_seen).await;
        match control {
            Err(error) if same(self.current_stream().as_ref(), Some(current)) => {
                Err(MediaError::Capture(error))
            }
            _ => Ok(()),
        }
    }

    async fn read_video(&self, frame: piik_native_capture::Frame) -> MediaResult<()> {
        let (source, paused) = {
            let state = self.lock();
            (state.source.clone(), state.paused)
        };
        let Some(source) = source.filter(|source| {
            (frame.kind == piik_native_capture::FrameKind::H264) == (source.codec() == "h264")
        }) else {
            return Err(MediaError::failed(
                "native video frame does not match its source codec",
            ));
        };
        let layer = usize::from(frame.layer);
        source
            .set_format(layer, frame.width, frame.height)
            .await
            .map_err(bridge::media_error)?;
        if paused {
            return Ok(());
        }
        // Edge state owns write failures; one retired edge cannot stop siblings.
        let written = source
            .write_video(
                layer,
                piik_encoded::Frame {
                    data: frame.data,
                    pts: frame.timestamp,
                    duration: frame.duration,
                    recovery: frame.key_frame,
                },
            )
            .await;
        match written {
            Err(error) if error.is_invalid_timestamp() => Err(bridge::media_error(error)),
            _ => Ok(()),
        }
    }
}
