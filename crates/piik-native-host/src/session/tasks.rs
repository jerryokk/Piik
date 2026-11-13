//! Share background tasks: the video reader's startup handoff, quality
//! sampling, volume application and the source-audio reader.

use std::sync::{Arc, Weak};
use std::time::SystemTime;

use piik_media_edge::Edge;
use piik_native_control::media::{MediaError, MediaEventKind, MediaResult};
use tokio::sync::oneshot;

use super::{Inner, OwnedPublication, PublicationKey, QUALITY_SAMPLE_PERIOD};
use crate::bridge;
use crate::capture::same;

impl Inner {
    pub(super) async fn run(
        self: Arc<Self>,
        video_done: oneshot::Receiver<MediaResult<()>>,
        audio_starting: Option<tokio::task::JoinHandle<()>>,
    ) {
        let quality = tokio::spawn(self.clone().run_quality());
        if let Some(output) = self.lock().audio_source.clone() {
            let mut volume = self.volume.subscribe();
            let ctx = self.ctx.clone();
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        changed = volume.changed() => if changed.is_err() { return },
                        () = ctx.cancelled() => return,
                    }
                    let value = *volume.borrow_and_update();
                    let _ = output.set_volume(value).await;
                }
            });
        }
        let audio = match self.mixer() {
            Some(mixer) => Some(tokio::spawn(async move { mixer.run().await })),
            // The share's output shape is fixed; inputs can change concurrently.
            None if self.lock().audio_source.is_none() => None,
            None => Some(tokio::spawn(self.clone().run_audio())),
        };
        let mut result = video_done
            .await
            .unwrap_or_else(|_| Err(MediaError::failed("native capture stopped unexpectedly")));
        let closed = self.lock().closed;
        if closed || self.ctx.is_cancelled() {
            // Closing this owner or its parent is a clean end, even if stopping
            // the capture made the video reader return an error.
            result = Ok(());
        }
        self.ctx.cancel();
        if let Some(starting) = audio_starting {
            let _ = starting.await;
        }
        let audio_stream = self.lock().audio_stream.clone();
        if let Some(stream) = audio_stream {
            let _ = stream.close().await;
        }
        if let Some(audio) = audio {
            let _ = audio.await;
        }
        let _ = quality.await;
        self.retire().await;
        let failed = result.is_err();
        let error = result
            .as_ref()
            .err()
            .map(ToString::to_string)
            .unwrap_or_default();
        let share = piik_diagnostics::id(&self.share_id);
        if failed
            && !result
                .as_ref()
                .err()
                .is_some_and(MediaError::source_canceled)
        {
            tracing::warn!(event = "share-ended", share = %share, failed,
                error = %piik_diagnostics::safe_text(&error), "piik-client");
        } else {
            tracing::debug!(event = "share-ended", share = %share, failed,
                error = %piik_diagnostics::safe_text(&error), "piik-client");
        }
        // The owner's close reports no share-ended event.
        let closed = self.lock().closed;
        *self
            .ended_result
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some((!closed).then_some(result));
        self.ended.send_replace(true);
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
            let (edges, publications): (Vec<Edge>, Vec<(PublicationKey, OwnedPublication)>) = {
                let state = self.lock();
                (
                    state
                        .edges
                        .values()
                        .map(|owned| owned.edge.clone())
                        .collect(),
                    state
                        .publications
                        .iter()
                        .map(|(key, owned)| (key.clone(), owned.clone()))
                        .collect(),
                )
            };
            for edge in edges {
                if let Some(sample) = edge.quality_sample(now).await {
                    self.emit(
                        MediaEventKind::EdgeQuality {
                            connection_id: edge.connection_id().to_owned(),
                            quality: bridge::quality_sample(sample),
                        },
                        None,
                    );
                }
            }
            for (key, owned) in publications {
                owned.publication.refresh_media().await;
                if let Some(sample) = owned.publication.quality_sample(now).await {
                    let weak = Arc::downgrade(&self);
                    let token = Arc::downgrade(&owned.token);
                    let owner = key.clone();
                    self.emit(
                        MediaEventKind::PublicationQuality {
                            publication_generation: key.0,
                            connection_id: key.1,
                            sample,
                        },
                        Some(Arc::new(move || owns_publication(&weak, &owner, &token))),
                    );
                }
            }
        }
    }

    /// Shares without the microphone opt-in retain their original audio shape.
    async fn run_audio(self: Arc<Self>) {
        let Some(output) = self.lock().audio_source.clone() else {
            return;
        };
        // The stream stays current across frames. Only a replacement or the
        // stream's end takes it out of this loop.
        let mut current = self.current_audio_stream();
        while let Some(stream) = current.clone() {
            let frame = stream.read().await;
            let next = self.current_audio_stream();
            if !same(next.as_ref(), Some(&stream)) {
                current = next;
                continue;
            }
            let ended;
            match &frame {
                Ok(frame) if frame.kind == piik_native_capture::FrameKind::Pcm => {
                    if self.lock().paused {
                        continue;
                    }
                    match output.write_pcm(&frame.data, frame.duration).await {
                        Ok(()) => continue,
                        Err(error) => ended = error.to_string(),
                    }
                }
                // A non-PCM frame is not this reader's end; keep reading.
                Ok(_) => continue,
                Err(error) => ended = error.to_string(),
            }
            tracing::debug!(event = "capture-audio-stream-ended",
                share = %piik_diagnostics::id(&self.share_id),
                canceled = self.ctx.is_cancelled(),
                error = %piik_diagnostics::safe_text(&ended), "piik-client");
            // Audio failure leaves video live. Only an explicit source
            // replacement changes this owner; its stored wake also covers an
            // earlier commit.
            while same(self.current_audio_stream().as_ref(), Some(&stream)) {
                tokio::select! {
                    () = self.audio_changed.notified() => {}
                    () = self.ctx.cancelled() => return,
                }
            }
            current = self.current_audio_stream();
        }
    }
}

pub(crate) fn owns_publication(weak: &Weak<Inner>, key: &PublicationKey, token: &Weak<()>) -> bool {
    let (Some(inner), Some(token)) = (weak.upgrade(), token.upgrade()) else {
        return false;
    };
    let state = inner.lock();
    // A current encoder failure still needs its terminal event: retirement
    // is map ownership, not the transport's closed state.
    !state.closed
        && state
            .publications
            .get(key)
            .is_some_and(|owned| Arc::ptr_eq(&owned.token, &token))
}
