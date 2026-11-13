//! Source audio and microphone mixed into one share output.
//!
//! One share owns one output and encoder; replacing an input never replaces
//! that output. The session owns source capture; this owner owns the
//! microphone capture.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use piik_media_edge::AudioSource;
use piik_native_audio::{CHANNELS, FRAME_BYTES, FRAME_SAMPLES};
use piik_native_capture::FrameKind;
use piik_native_control::media::{MediaError, MediaEventKind, MediaResult};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::bridge::Emitter;
use crate::capture::{CaptureProvider, SharedStream};

pub(crate) const AUDIO_PERIOD: Duration = Duration::from_millis(20);
/// The bounded FIFO absorbs scheduling jitter; it never replays samples or
/// builds a latency tail, and does not claim to remove long-term drift.
const INPUT_FRAMES: usize = 4;

/// One device input. Identity is the `Arc` allocation of its token.
struct Input {
    token: Arc<()>,
    stream: SharedStream,
    frames: VecDeque<Vec<u8>>,
}

impl Input {
    fn new(stream: SharedStream) -> Self {
        Self {
            token: Arc::new(()),
            stream,
            frames: VecDeque::with_capacity(INPUT_FRAMES),
        }
    }

    fn push(&mut self, frame: Vec<u8>) {
        if self.frames.len() == INPUT_FRAMES {
            self.frames.pop_front();
        }
        self.frames.push_back(frame);
    }
}

fn pop(input: Option<&mut Input>) -> Option<Vec<u8>> {
    input.and_then(|input| input.frames.pop_front())
}

#[derive(Default)]
struct State {
    source: Option<Input>,
    microphone: Option<Input>,
    paused: bool,
    gain: f64,
    applied_gain: f64,
    microphone_device: String,
    readers: Vec<JoinHandle<()>>,
}

impl State {
    fn input(&self, microphone: bool) -> Option<&Input> {
        if microphone {
            self.microphone.as_ref()
        } else {
            self.source.as_ref()
        }
    }

    fn input_mut(&mut self, microphone: bool) -> Option<&mut Input> {
        if microphone {
            self.microphone.as_mut()
        } else {
            self.source.as_mut()
        }
    }

    fn event(&self, failed: bool) -> MediaEventKind {
        MediaEventKind::AudioState {
            source_audio: self.source.is_some(),
            microphone: self.microphone.is_some(),
            failed,
        }
    }
}

/// The share's mixed audio owner. State delivery is ordered: each audio-state
/// event is enqueued under the state lock, and enqueueing never waits.
pub(crate) struct AudioMix {
    ctx: CancellationToken,
    output: AudioSource,
    emitter: Emitter,
    state: Mutex<State>,
}

impl AudioMix {
    pub fn new(ctx: CancellationToken, output: AudioSource, emitter: Emitter) -> Arc<Self> {
        Arc::new(Self {
            ctx,
            output,
            emitter,
            state: Mutex::new(State {
                gain: 1.0,
                ..State::default()
            }),
        })
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }

    pub fn has_source(&self) -> bool {
        self.lock().source.is_some()
    }

    /// Replaces the source input; the session closes the previous stream.
    pub fn set_source(self: &Arc<Self>, stream: Option<SharedStream>) {
        let mut state = self.lock();
        if self.ctx.is_cancelled() {
            return;
        }
        state.source = None;
        if let Some(stream) = stream {
            let input = Input::new(stream);
            let reader = self.reader(&input, false);
            state.source = Some(input);
            state.readers.push(reader);
        }
        self.emitter.emit(state.event(false), None);
    }

    fn reader(self: &Arc<Self>, input: &Input, microphone: bool) -> JoinHandle<()> {
        let mix = self.clone();
        let token = input.token.clone();
        let stream = input.stream.clone();
        tokio::spawn(async move { mix.read(token, stream, microphone).await })
    }

    async fn read(&self, token: Arc<()>, stream: SharedStream, microphone: bool) {
        loop {
            let frame = stream.read().await;
            let ended = {
                let mut state = self.lock();
                let current = state
                    .input(microphone)
                    .is_some_and(|input| Arc::ptr_eq(&input.token, &token));
                if !current || self.ctx.is_cancelled() {
                    return;
                }
                match frame {
                    Ok(frame)
                        if frame.kind == FrameKind::Pcm
                            && frame.data.len() == FRAME_BYTES
                            && frame.duration == AUDIO_PERIOD =>
                    {
                        let paused = state.paused;
                        if !paused && let Some(input) = state.input_mut(microphone) {
                            input.push(frame.data);
                        }
                        None
                    }
                    frame => {
                        // The input ended: a microphone failure is reported as such.
                        if microphone {
                            state.microphone = None;
                        } else {
                            state.source = None;
                        }
                        self.emitter.emit(state.event(microphone), None);
                        Some(
                            frame
                                .err()
                                .map(|error| error.to_string())
                                .unwrap_or_default(),
                        )
                    }
                }
            };
            if let Some(error) = ended {
                tracing::debug!(event = "audio-input-ended", microphone,
                    error = %piik_diagnostics::safe_text(&error), "piik-client");
                let _ = stream.close().await;
                return;
            }
        }
    }

    pub fn set_paused(&self, paused: bool) {
        let mut state = self.lock();
        let state = &mut *state;
        state.paused = paused;
        for input in [state.source.as_mut(), state.microphone.as_mut()]
            .into_iter()
            .flatten()
        {
            input.frames.clear();
        }
        // Never carry a pre-pause ramp into the resumed share.
        state.applied_gain = 0.0;
    }

    pub async fn set_microphone(
        self: &Arc<Self>,
        capture: &dyn CaptureProvider,
        enabled: Option<bool>,
        gain: Option<f64>,
        device_id: Option<String>,
    ) -> MediaResult<()> {
        if device_id
            .as_deref()
            .is_some_and(|id| !piik_native_capture::valid_device_id(id))
        {
            return Err(MediaError::failed("microphone device is invalid"));
        }
        if gain.is_some_and(|gain| !gain.is_finite() || !(0.0..=2.0).contains(&gain)) {
            return Err(MediaError::failed("microphone volume is invalid"));
        }
        let (wanted, device) = {
            let mut state = self.lock();
            if let Some(gain) = gain {
                state.gain = gain;
            }
            let wanted = enabled.unwrap_or(state.microphone.is_some());
            let device = device_id.unwrap_or_else(|| state.microphone_device.clone());
            if wanted == state.microphone.is_some() && device == state.microphone_device {
                return Ok(());
            }
            (wanted, device)
        };
        let stream = if wanted {
            Some(capture.microphone(&self.ctx, device.clone()).await?)
        } else {
            None
        };
        let previous = {
            let mut state = self.lock();
            if self.ctx.is_cancelled() {
                Err(stream)
            } else {
                Ok(self.install_microphone(&mut state, stream, device))
            }
        };
        match previous {
            Ok(Some(previous)) => {
                let _ = previous.stream.close().await;
            }
            Ok(None) => {}
            Err(stream) => {
                if let Some(stream) = stream {
                    let _ = stream.close().await;
                }
                return Err(MediaError::failed("native share is unavailable"));
            }
        }
        Ok(())
    }

    fn install_microphone(
        self: &Arc<Self>,
        state: &mut State,
        stream: Option<SharedStream>,
        device: String,
    ) -> Option<Input> {
        {
            let previous = state.microphone.take();
            state.microphone_device = device;
            if let Some(stream) = stream {
                let input = Input::new(stream);
                let reader = self.reader(&input, true);
                state.microphone = Some(input);
                state.readers.push(reader);
            }
            self.emitter.emit(state.event(false), None);
            previous
        }
    }

    /// Writes one mixed frame per period until the share ends, then retires
    /// the microphone and waits for every reader. Session retirement closes
    /// the source stream.
    pub async fn run(&self) {
        let mut ticker = tokio::time::interval(AUDIO_PERIOD);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        ticker.tick().await;
        let mut previous = tokio::time::Instant::now();
        let mut output = vec![0u8; FRAME_BYTES];
        loop {
            tokio::select! {
                _ = ticker.tick() => {}
                () = self.ctx.cancelled() => break,
            }
            let now = tokio::time::Instant::now();
            // The RTP clock advances across missed ticks; do not burst old speech.
            let missed = (now - previous).as_nanos() / AUDIO_PERIOD.as_nanos();
            let dropped = u16::try_from(missed.saturating_sub(1)).unwrap_or(u16::MAX);
            previous = now;
            {
                let mut state = self.lock();
                let source = pop(state.source.as_mut());
                let microphone = pop(state.microphone.as_mut());
                let gain = if microphone.is_some() {
                    state.gain
                } else {
                    0.0
                };
                mix_pcm(
                    &mut output,
                    source.as_deref(),
                    microphone.as_deref(),
                    state.applied_gain,
                    gain,
                );
                state.applied_gain = gain;
                if state.paused {
                    output.fill(0);
                }
            }
            // A retired edge cannot end healthy sibling media.
            let _ = self
                .output
                .write_pcm_with_dropped(&output, AUDIO_PERIOD, dropped)
                .await;
        }
        let (microphone, readers) = {
            let mut state = self.lock();
            (state.microphone.take(), std::mem::take(&mut state.readers))
        };
        if let Some(microphone) = microphone {
            let _ = microphone.stream.close().await;
        }
        for reader in readers {
            let _ = reader.await;
        }
    }

    #[cfg(test)]
    pub(crate) fn readers(&self) -> usize {
        self.lock().readers.len()
    }
}

/// Mixes once in a wider type, then saturates. Source sound is never
/// attenuated because the microphone was enabled; a gain change ramps
/// across one frame.
pub(crate) fn mix_pcm(
    output: &mut [u8],
    source: Option<&[u8]>,
    microphone: Option<&[u8]>,
    from: f64,
    to: f64,
) {
    let sample_at =
        |pcm: &[u8], offset: usize| f64::from(i16::from_le_bytes([pcm[offset], pcm[offset + 1]]));
    for sample in 0..FRAME_SAMPLES {
        let gain = from + (to - from) * (sample + 1) as f64 / FRAME_SAMPLES as f64;
        for channel in 0..CHANNELS {
            let offset = (sample * CHANNELS + channel) * 2;
            let mut value = source.map_or(0.0, |pcm| sample_at(pcm, offset));
            if let Some(pcm) = microphone {
                value += gain * sample_at(pcm, offset);
            }
            let mixed = value.clamp(-32768.0, 32767.0) as i16;
            output[offset..offset + 2].copy_from_slice(&mixed.to_le_bytes());
        }
    }
}
