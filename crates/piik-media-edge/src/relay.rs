//! Optional derivation of a relay source's missing lower representation
//! through the capture process's encoded-video relay encoder.
//!
//! Raw RTP never waits for it: the shard feeds a bounded queue, and one
//! off-shard run per source assembles access units, writes them to the
//! process and returns its encoded outputs to the source. A retired run must
//! finish before another can start.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

use piik_encoded::Frame as EncodedFrame;
use piik_media_forwarding::LayerFormat;
use piik_media_transport::Connections;
use piik_media_transport::sdp::VideoCodec;
use piik_native_capture::{
    Capabilities, EncodedVideoOptions, FrameKind, OutputProfile, Stream, VideoProfile,
    screen_share_outputs, start_encoded_video, valid_video_size,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::access_unit::{Assembler, Codec as AssemblyCodec, RELAY_PACKET_LIMIT, RtpPacket};
use crate::engine::{App, Engine, next_key};
use crate::error::{Error, Result};
use crate::groups::ORIGINAL_RECOVERY;
use crate::receiver::ReceivedRtp;
use crate::relay_input::{RelayClock, RelayInput};

/// Where a capable Viewer's relay encoder runs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RelayOptions {
    pub capture_process: PathBuf,
    pub capabilities: Capabilities,
}

/// The first adapter with a usable encoder for `codec`.
pub(crate) fn relay_backend(
    codec: VideoCodec,
    options: Option<&RelayOptions>,
) -> Option<EncodedVideoOptions> {
    let options = options.filter(|options| !options.capture_process.as_os_str().is_empty())?;
    let mut backend = EncodedVideoOptions {
        codec: codec.name().to_owned(),
        ..EncodedVideoOptions::default()
    };
    for adapter in &options.capabilities.adapters {
        match codec {
            VideoCodec::Vp8 if options.capabilities.software_vp8 => {
                backend.adapter_index = adapter.index;
                return Some(backend);
            }
            VideoCodec::H264 if !adapter.hardware_h264.is_empty() => {
                backend.adapter_index = adapter.index;
                backend.encoder_index = adapter.hardware_h264[0].index;
                return Some(backend);
            }
            _ => {}
        }
    }
    None
}

/// One run's identity and inputs.
struct Run {
    id: u64,
    profile: u64,
    format: (u32, u32),
    packets: mpsc::Sender<ReceivedRtp>,
    cancel: CancellationToken,
    /// A failure (not a plain cancellation) retires the derived slots.
    failed: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
}

pub(crate) struct RelayState {
    options: RelayOptions,
    run: Option<Run>,
}

impl RelayState {
    pub fn new(options: RelayOptions) -> Self {
        Self { options, run: None }
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct RelayPlan {
    pub profile: Option<(u64, VideoProfile)>,
    pub output: OutputProfile,
    pub format: (u32, u32),
    pub wanted: bool,
}

impl App {
    /// Records the room's relay ceiling. It never
    /// creates an encoder or starts the process by itself.
    pub(crate) fn set_relay_profile(
        &mut self,
        conns: &mut Connections,
        key: u64,
        profile: VideoProfile,
        now: Instant,
    ) -> Result<()> {
        let state = self.source(key)?;
        if state
            .relay_profile
            .as_ref()
            .is_some_and(|(_, known)| *known == profile)
        {
            return Ok(());
        }
        state.relay_profile = Some((next_key(), profile));
        if state.relay.is_some() {
            self.relay_cancel(conns, key, now);
            self.configure_relay_formats(key)?;
        }
        Ok(())
    }

    /// Declared ceilings, not measured quality.
    pub(crate) fn configure_relay_formats(&mut self, key: u64) -> Result<()> {
        let state = self.source(key)?;
        let Some((_, profile)) = state.relay_profile.clone() else {
            return Ok(());
        };
        let base = state.format_count - 1;
        let (width, height) = state.format(base);
        let mut lower = screen_share_outputs(&profile)[0];
        if width > 0 && height > 0 {
            lower.width = (width / 4 * 2).min(lower.width);
            lower.height = (height / 4 * 2).min(lower.height);
        }
        let slots = 2 + state.group_capacity();
        state.output_bitrates = vec![lower.bitrate; slots];
        state.output_bitrates[1] = profile.bitrate;
        let formats = vec![
            LayerFormat {
                width: lower.width,
                height: lower.height,
                bitrate: lower.bitrate,
            },
            LayerFormat {
                width,
                height,
                bitrate: profile.bitrate,
            },
        ];
        state.media.source_mut().update_formats(formats.clone())?;
        for media in state.group_media.values_mut() {
            media.source_mut().update_formats(formats.clone())?;
        }
        Ok(())
    }

    /// Planning also updates the group memberships.
    pub(crate) fn relay_plan(&mut self, key: u64) -> RelayPlan {
        let mut plan = RelayPlan::default();
        let Some(state) = self.sources.get(&key).filter(|state| !state.closed) else {
            return plan;
        };
        let Some(profile) = state.relay_profile.clone() else {
            return plan;
        };
        if state.output_bitrates.is_empty() {
            return plan;
        }
        let base = state.format_count - 1;
        plan.format = state.format(base);
        plan.output = screen_share_outputs(&profile.1)[0];
        plan.profile = Some(profile);
        let (width, height) = plan.format;
        plan.output.width = (width / 4 * 2).min(plan.output.width);
        plan.output.height = (height / 4 * 2).min(plan.output.height);
        if !plan.output.valid() || !valid_video_size(width, height) {
            return plan;
        }
        let edges: Vec<u64> = state.edges.keys().copied().collect();
        let demands = self.collect_demands(key, &edges, &[]);
        plan.wanted = demands.iter().any(|demand| demand.active && demand.lower);
        let _ = self.plan_groups(key, &demands);
        plan
    }

    /// Feeds the derivation after the raw path has forwarded `packet`.
    pub(crate) fn relay_push(
        &mut self,
        conns: &mut Connections,
        key: u64,
        packet: &ReceivedRtp,
        now: Instant,
    ) {
        let Some(state) = self.sources.get_mut(&key) else {
            return;
        };
        let Some(relay) = &mut state.relay else {
            return;
        };
        if relay
            .run
            .as_ref()
            .is_some_and(|run| run.done.load(Ordering::Acquire))
        {
            relay.run = None;
        }
        if packet.marker {
            let plan = self.relay_plan(key);
            let Some(state) = self.sources.get_mut(&key) else {
                return;
            };
            if !plan.wanted && state.groups.recovery() & ORIGINAL_RECOVERY != 0 {
                state.groups.clear_recovery(ORIGINAL_RECOVERY);
                state.request_recovery(None);
            }
            let profile = plan.profile.as_ref().map(|(id, _)| *id);
            let relay = state.relay.as_mut().expect("relay source");
            if let Some(run) = &relay.run
                && (!plan.wanted || Some(run.profile) != profile || plan.format != run.format)
            {
                run.cancel.cancel();
                return;
            }
            if relay.run.is_none() && plan.wanted {
                let layer = state.format_count - 1;
                let Some(at) = state.media.source().reference_time(layer, packet.timestamp) else {
                    return;
                };
                let Some(engine) = self.engine() else {
                    return;
                };
                let state = self.sources.get_mut(&key).expect("source exists");
                let (codec, group_capacity) = (state.codec, state.group_capacity());
                let relay = state.relay.as_mut().expect("relay source");
                let (packets, input) = mpsc::channel(RELAY_PACKET_LIMIT);
                let run = Run {
                    id: next_key(),
                    profile: profile.unwrap_or_default(),
                    format: plan.format,
                    packets,
                    cancel: engine.inner.cancel.child_token(),
                    failed: Arc::new(AtomicBool::new(false)),
                    done: Arc::new(AtomicBool::new(false)),
                };
                let task = RunTask {
                    engine,
                    key,
                    id: run.id,
                    codec,
                    backend: relay_backend(codec, Some(&relay.options)),
                    capture_process: relay.options.capture_process.clone(),
                    profile: plan
                        .profile
                        .clone()
                        .map(|(_, profile)| profile)
                        .unwrap_or_default(),
                    output: plan.output,
                    group_capacity,
                    layer,
                    anchor_rtp: packet.timestamp,
                    anchor_time: at,
                    cancel: run.cancel.clone(),
                    failed: run.failed.clone(),
                    done: run.done.clone(),
                };
                relay.run = Some(run);
                tokio::spawn(task.serve(input));
                // Earlier packets of this frame used the raw path only; begin
                // assembly with the next complete frame.
                return;
            }
        }
        let Some(state) = self.sources.get_mut(&key) else {
            return;
        };
        let Some(run) = state.relay.as_ref().and_then(|relay| relay.run.as_ref()) else {
            return;
        };
        if run.cancel.is_cancelled() {
            return;
        }
        if run.packets.try_send(packet.clone()).is_err() {
            // Discarding a compressed reference frame silently poisons the
            // decoder. Retire this derived output instead.
            run.failed.store(true, Ordering::Release);
            run.cancel.cancel();
        }
        let _ = (conns, now);
    }

    /// Cancels the derivation; the run exits on its own.
    pub(crate) fn relay_cancel(&mut self, _conns: &mut Connections, key: u64, _now: Instant) {
        if let Some(run) = self
            .sources
            .get(&key)
            .and_then(|state| state.relay.as_ref())
            .and_then(|relay| relay.run.as_ref())
        {
            run.cancel.cancel();
        }
    }

    /// Whether run `id` still owns the source's derivation for its profile
    /// and input format.
    fn relay_current(&self, key: u64, id: u64) -> bool {
        let Some(state) = self.sources.get(&key).filter(|state| !state.closed) else {
            return false;
        };
        let Some(run) = state.relay.as_ref().and_then(|relay| relay.run.as_ref()) else {
            return false;
        };
        run.id == id
            && !run.cancel.is_cancelled()
            && state.relay_profile.as_ref().map(|(profile, _)| *profile) == Some(run.profile)
            && state.format(state.format_count - 1) == run.format
    }

    /// The same as [`Self::relay_current`] without the cancellation check:
    /// a real failure owns isolation until its run finished retiring.
    fn relay_owner(&self, key: u64, id: u64) -> bool {
        let Some(state) = self.sources.get(&key).filter(|state| !state.closed) else {
            return false;
        };
        let Some(run) = state.relay.as_ref().and_then(|relay| relay.run.as_ref()) else {
            return false;
        };
        run.id == id
            && run.failed.load(Ordering::Acquire)
            && state.relay_profile.as_ref().map(|(profile, _)| *profile) == Some(run.profile)
            && state.format(state.format_count - 1) == run.format
    }

    fn relay_mark_unavailable(
        &mut self,
        conns: &mut Connections,
        key: u64,
        id: u64,
        slot: usize,
        failure: bool,
        now: Instant,
    ) -> Result<()> {
        let owner = if failure {
            self.relay_owner(key, id)
        } else {
            self.relay_current(key, id)
        };
        if !owner {
            return Ok(());
        }
        self.mark_layer_unavailable(conns, key, slot, now)
    }
}

/// Physical output slot of a relay encoder output: slot 1 is the received
/// original.
fn physical(slot: usize) -> usize {
    if slot > 0 { slot + 1 } else { slot }
}

struct RunTask {
    engine: Engine,
    key: u64,
    id: u64,
    codec: VideoCodec,
    backend: Option<EncodedVideoOptions>,
    capture_process: PathBuf,
    profile: VideoProfile,
    output: OutputProfile,
    group_capacity: usize,
    layer: usize,
    anchor_rtp: u32,
    anchor_time: SystemTime,
    cancel: CancellationToken,
    failed: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
}

impl RunTask {
    async fn serve(self, input: mpsc::Receiver<ReceivedRtp>) {
        let result = self.run(input).await;
        if result.is_err() {
            self.failed.store(true, Ordering::Release);
        }
        self.cancel.cancel();
        let (key, id, slots) = (self.key, self.id, 2 + self.group_capacity);
        let failed = self.failed.load(Ordering::Acquire);
        self.engine
            .send(move |app, conns, now| {
                app.set_lowest_layer_rate_controlled(key, false);
                if failed {
                    for slot in (0..slots).filter(|slot| *slot != 1) {
                        let _ = app.relay_mark_unavailable(conns, key, id, slot, true, now);
                    }
                }
            })
            .await;
        self.done.store(true, Ordering::Release);
    }

    async fn run(&self, input: mpsc::Receiver<ReceivedRtp>) -> Result<()> {
        let mut backend = self
            .backend
            .clone()
            .ok_or(Error::Invalid("native relay encoder is unavailable"))?;
        backend.preference = self.profile.preference.clone();
        backend.outputs = vec![self.output; self.group_capacity + 1];
        let stream = start_encoded_video(&self.cancel, &self.capture_process, &backend).await?;
        let key = self.key;
        let layer = self.layer;
        self.engine
            .call(move |app, _, _| {
                app.set_lowest_layer_rate_controlled(key, true);
                app.begin_generation(key);
                if let Ok(state) = app.source(key) {
                    state.request_recovery(Some(layer));
                }
            })
            .await?;
        let stream = Arc::new(stream);
        let reader = tokio::spawn(read(
            self.engine.clone(),
            stream.clone(),
            self.key,
            self.id,
            self.codec,
            self.group_capacity,
            self.anchor_time,
            self.cancel.clone(),
            self.failed.clone(),
        ));
        let written = tokio::select! {
            () = self.cancel.cancelled() => Ok(()),
            result = self.write(&stream, input) => result,
        };
        let _ = stream.close().await;
        let _ = reader.await;
        written
    }

    async fn write(&self, stream: &Stream, mut input: mpsc::Receiver<ReceivedRtp>) -> Result<()> {
        let codec = match self.codec {
            VideoCodec::H264 => AssemblyCodec::H264,
            VideoCodec::Vp8 => AssemblyCodec::Vp8,
        };
        let mut assembler = Assembler::new(codec);
        let mut decoder = RelayInput::new(self.codec);
        let mut clock = RelayClock::new(self.anchor_rtp);
        let mut waiting_recovery = true;
        let fallback = Duration::from_secs(1) / self.profile.framerate.max(1);
        while let Some(packet) = input.recv().await {
            let retained = assembler.push(RtpPacket {
                sequence_number: packet.sequence_number,
                timestamp: packet.timestamp,
                marker: packet.marker,
                payload: packet.payload.to_vec(),
            });
            // Retirement keeps complete configuration NALs only.
            for nal in retained {
                decoder.frame(nal)?;
            }
            loop {
                let unit = assembler.pop();
                if assembler.take_losses() > 0 {
                    waiting_recovery = true;
                    self.request_upstream();
                }
                let Some(unit) = unit else {
                    break;
                };
                let mut frame = decoder.frame(unit.data)?;
                if waiting_recovery && !frame.key_frame || frame.width == 0 || frame.height == 0 {
                    continue;
                }
                let Some((pts, duration)) = clock.timing(unit.timestamp) else {
                    continue;
                };
                waiting_recovery = false;
                frame.timestamp = pts;
                frame.duration = if duration.is_zero() {
                    fallback
                } else {
                    duration
                };
                let (key, id) = (self.key, self.id);
                let current = self
                    .engine
                    .call(move |app, _, _| {
                        let wanted = app.relay_plan(key).wanted;
                        wanted && app.relay_current(key, id)
                    })
                    .await?;
                if !current {
                    return Ok(());
                }
                stream.write_frame(&frame).await?;
            }
        }
        Ok(())
    }

    fn request_upstream(&self) {
        let (key, layer) = (self.key, self.layer);
        self.engine.post(move |app, _, _| {
            if let Ok(state) = app.source(key) {
                state.request_recovery(Some(layer));
            }
        });
    }
}

#[allow(clippy::too_many_arguments)]
async fn read(
    engine: Engine,
    stream: Arc<Stream>,
    key: u64,
    id: u64,
    codec: VideoCodec,
    group_capacity: usize,
    anchor: SystemTime,
    cancel: CancellationToken,
    failed: Arc<AtomicBool>,
) {
    let result = read_frames(
        &engine,
        &stream,
        key,
        id,
        codec,
        group_capacity,
        anchor,
        &cancel,
    )
    .await;
    if result.is_err() && !cancel.is_cancelled() {
        failed.store(true, Ordering::Release);
    }
    cancel.cancel();
}

#[allow(clippy::too_many_arguments)]
async fn read_frames(
    engine: &Engine,
    stream: &Stream,
    key: u64,
    id: u64,
    codec: VideoCodec,
    group_capacity: usize,
    anchor: SystemTime,
    cancel: &CancellationToken,
) -> Result<()> {
    loop {
        let frame = stream.read().await?;
        if cancel.is_cancelled() {
            return Ok(());
        }
        match frame.kind {
            FrameKind::Status => {}
            FrameKind::Begin => {
                let at = anchor + frame.timestamp;
                let pts = frame.timestamp;
                let plan = engine
                    .call(move |app, conns, now| app.begin_frame(conns, key, pts, at, now))
                    .await?;
                let Ok(plan) = plan else {
                    continue;
                };
                for slot in 0..stream.outputs().len() {
                    let physical = physical(slot);
                    if let Some(bitrate) = plan
                        .bitrates
                        .get(physical)
                        .copied()
                        .filter(|bitrate| *bitrate > 0)
                    {
                        stream.set_output_bitrate(slot, bitrate).await?;
                    }
                    stream
                        .set_output_active(
                            slot,
                            plan.active.get(physical).copied().unwrap_or(false),
                        )
                        .await?;
                }
                for physical in plan.recovery_layers {
                    if physical == 1 {
                        engine.post(move |app, _, _| {
                            if let Ok(state) = app.source(key) {
                                state.request_recovery(None);
                            }
                        });
                        continue;
                    }
                    let slot = if physical > 1 { physical - 1 } else { physical };
                    stream.request_key_frame(slot as i32).await?;
                }
            }
            FrameKind::H264 | FrameKind::Vp8 => {
                let layer = usize::from(frame.layer);
                if layer > group_capacity
                    || (frame.kind == FrameKind::H264) != (codec == VideoCodec::H264)
                {
                    return Err(Error::Invalid(
                        "native relay output does not match its source",
                    ));
                }
                let slot = physical(layer);
                let accepted = engine
                    .call(move |app, conns, now| {
                        if !app.relay_current(key, id) {
                            return Err(Error::Closed);
                        }
                        app.set_format(key, slot, frame.width, frame.height)?;
                        app.write_video(
                            conns,
                            key,
                            slot,
                            EncodedFrame {
                                data: frame.data,
                                pts: frame.timestamp,
                                duration: frame.duration,
                                recovery: frame.key_frame,
                            },
                            now,
                        )
                    })
                    .await?;
                accepted?;
            }
            FrameKind::LayerUnavailable => {
                let slot = physical(usize::from(frame.layer));
                engine
                    .call(move |app, conns, now| {
                        app.relay_mark_unavailable(conns, key, id, slot, false, now)
                    })
                    .await??;
            }
            _ => return Err(Error::Invalid("native relay output failed")),
        }
    }
}

#[cfg(test)]
impl App {
    /// Installs a run without a process for the current profile and format.
    /// `failed` marks a real failure rather than a plain cancellation.
    pub(crate) fn install_test_run(&mut self, key: u64, failed: bool) -> u64 {
        let state = self.sources.get_mut(&key).expect("relay source");
        let profile = state.relay_profile.as_ref().map_or(0, |(id, _)| *id);
        let format = state.format(state.format_count - 1);
        let relay = state.relay.as_mut().expect("relay source");
        let (packets, _) = mpsc::channel(1);
        let run = Run {
            id: next_key(),
            profile,
            format,
            packets,
            cancel: CancellationToken::new(),
            failed: Arc::new(AtomicBool::new(failed)),
            done: Arc::new(AtomicBool::new(false)),
        };
        if failed {
            run.cancel.cancel();
        }
        let id = run.id;
        relay.run = Some(run);
        id
    }

    /// The failure path of a finished run `id`: retire its derived slots.
    pub(crate) fn relay_test_failure(
        &mut self,
        conns: &mut Connections,
        key: u64,
        id: u64,
        now: Instant,
    ) {
        let slots = 2 + self.sources[&key].group_capacity();
        for slot in (0..slots).filter(|slot| *slot != 1) {
            let _ = self.relay_mark_unavailable(conns, key, id, slot, true, now);
        }
    }

    pub(crate) fn relay_test_cancelled(&self, key: u64) -> bool {
        self.sources[&key]
            .relay
            .as_ref()
            .and_then(|relay| relay.run.as_ref())
            .is_some_and(|run| run.cancel.is_cancelled() && !run.failed.load(Ordering::Acquire))
    }
}

impl crate::source::SourceState {
    /// Whether a derivation run is installed.
    #[cfg(test)]
    pub(crate) fn relay_running(&self) -> bool {
        self.relay.as_ref().is_some_and(|relay| relay.run.is_some())
    }
}
