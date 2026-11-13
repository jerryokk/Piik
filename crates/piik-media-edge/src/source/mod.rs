//! One bounded encoded video source and its shared output groups.
//!
//! The source's original [`EncodedSource`] carries the capture or received
//! layers; derived lower-output groups are further `EncodedSource`s owned
//! here. Edges and publications attach to one of them at a time.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use piik_encoded::Frame;
use piik_media_forwarding::{
    InboundPacket, LayerFormat, PliThrottle, SenderReport, Source, SourceChanges,
};
use piik_media_session::{ClockAnchor, EncodedSource, InputClock, SourcePacket, UpstreamKeyframe};
use piik_media_transport::sdp::VideoCodec;
use piik_media_transport::{Connections, PcState};
use piik_native_capture::{MAX_OUTPUT_GROUPS, VideoProfile};

use crate::codec::{self, VIDEO_CLOCK_RATE};
use crate::engine::{App, Engine, MAX_RELAY_CHILDREN, next_key};
use crate::error::{Error, Result};
use crate::groups::{
    self, EdgeState as DemandState, GroupMedia, MediaId, OutputDemand, OutputGroups, OutputPlan,
};
use crate::relay::{RelayOptions, RelayState};

/// The original media of every source.
pub(crate) const ORIGINAL: MediaId = MediaId(0);
const ALL_RECOVERY: u32 = u32::MAX;

/// Requests encoder recovery for physical output slots; `[-1]` requests
/// every output. Runs on the Engine's shard and must not block.
pub type RecoveryCallback = Arc<dyn Fn(Vec<i32>) + Send + Sync>;

/// A transport attached to a source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Consumer {
    Edge(u64),
    Publication(u64),
}

pub(crate) struct SourceState {
    pub codec: VideoCodec,
    pub format_count: usize,
    pub capacity: usize,
    pub media: EncodedSource,
    pub group_media: HashMap<MediaId, EncodedSource>,
    pub groups: OutputGroups<Consumer>,
    next_media: u64,
    /// One validated generation's output ceilings; zero marks a failed producer.
    pub output_bitrates: Vec<u32>,
    pub callback: Option<RecoveryCallback>,
    /// The receiver whose upstream connection owns this source's recovery.
    pub upstream: Option<u64>,
    pending_recovery: u32,
    throttles: HashMap<MediaId, PliThrottle>,
    pub input_pts: Duration,
    pub input_at: Option<SystemTime>,
    pub has_input: bool,
    /// Attached edges and whether each is a local bridge.
    pub edges: HashMap<u64, bool>,
    pub publications: HashSet<u64>,
    /// Route capacity held by publications.
    pub reservations: usize,
    pub relay: Option<RelayState>,
    pub relay_profile: Option<(u64, VideoProfile)>,
    pub closed: bool,
}

impl SourceState {
    pub fn group_capacity(&self) -> usize {
        self.capacity.min(MAX_OUTPUT_GROUPS)
    }

    pub fn media_ref(&self, id: MediaId) -> Option<&EncodedSource> {
        if id == ORIGINAL {
            Some(&self.media)
        } else {
            self.group_media.get(&id)
        }
    }

    pub fn source_of(&self, id: MediaId) -> Option<&Source> {
        self.media_ref(id).map(EncodedSource::source)
    }

    /// The source format of a layer of the original media as `(w, h)`.
    pub fn format(&self, layer: usize) -> (u32, u32) {
        self.media
            .source()
            .formats()
            .get(layer)
            .map_or((0, 0), |format| (format.width, format.height))
    }

    pub fn failed_output(&self, layer: i32) -> bool {
        usize::try_from(layer)
            .ok()
            .and_then(|layer| self.output_bitrates.get(layer))
            .is_some_and(|bitrate| *bitrate == 0)
    }

    /// Physical output slot of a logical layer on `media`.
    fn physical(&self, media: MediaId, layer: usize) -> usize {
        if media == ORIGINAL {
            return layer;
        }
        match self.groups.group_for_media(media) {
            Some(group) if layer == 0 => group.slot,
            _ => 1,
        }
    }

    /// A relay's callback only wakes upstream; its local encoders also need
    /// recovery in the next frame's plan.
    pub fn request_layer_keyframe(&mut self, layer: usize) {
        if self.relay.is_some() || !self.has_recovery_owner() {
            self.groups.request_recovery(layer);
        }
        self.request_recovery(Some(layer));
    }

    fn has_recovery_owner(&self) -> bool {
        self.callback.is_some() || self.upstream.is_some()
    }

    /// Coalesces a recovery notification without dropping another output's.
    pub fn request_recovery(&mut self, layer: Option<usize>) {
        if self.has_recovery_owner() {
            self.pending_recovery |= layer.map_or(ALL_RECOVERY, |layer| 1 << layer);
        }
    }

    /// The pending notification, as the callback's layer list.
    #[cfg(test)]
    pub fn pending_recovery(&self) -> u32 {
        self.pending_recovery
    }

    pub(crate) fn take_recovery(&mut self) -> Option<Vec<i32>> {
        let mask = std::mem::take(&mut self.pending_recovery);
        if mask == 0 || self.closed {
            return None;
        }
        if mask == ALL_RECOVERY {
            return Some(vec![-1]);
        }
        Some(
            (0..32i32)
                .filter(|layer| mask & (1 << layer) != 0)
                .collect(),
        )
    }

    /// Routes one upstream request through the bootstrap throttle.
    pub fn keyframe(&mut self, media: MediaId, request: UpstreamKeyframe, now: Instant) {
        let throttle = self.throttles.entry(media).or_default();
        if !throttle.request(request.layer, request.force, now) {
            return;
        }
        let physical = self.physical(media, request.layer);
        self.request_layer_keyframe(physical);
    }

    pub fn keyframe_received(&mut self, media: MediaId, layer: usize) {
        if let Some(throttle) = self.throttles.get_mut(&media) {
            throttle.keyframe_received(layer);
        }
    }

    /// One shared capture clock across generations.
    pub(crate) fn begin_group_frame(&mut self, pts: Duration, mut at: SystemTime) -> Result<()> {
        if self.has_input {
            if let Some(input_at) = self.input_at {
                // New groups inherit the established capture clock.
                at = add_signed(input_at, pts, self.input_pts);
            }
        } else if self.relay.is_none()
            && let Some(input_at) = self.input_at
        {
            // Normalize a new generation's origin beyond the last input by one
            // RTP tick; relay input supplies its authoritative upstream clock.
            let earliest =
                input_at + Duration::from_nanos(1_000_000_000 / u64::from(VIDEO_CLOCK_RATE) + 1);
            if at < earliest {
                at = earliest;
            }
        }
        self.media.begin_frame(pts, at)?;
        self.input_pts = pts;
        self.input_at = Some(at);
        self.has_input = true;
        for media in self.group_media.values_mut() {
            media.begin_frame(pts, at)?;
        }
        Ok(())
    }

    fn ceiling(&self) -> u32 {
        self.media
            .source()
            .formats()
            .first()
            .map_or(0, |format| format.bitrate)
    }
}

/// `at + (pts - base)` without underflow.
fn add_signed(at: SystemTime, pts: Duration, base: Duration) -> SystemTime {
    if pts >= base {
        at + (pts - base)
    } else {
        at - (base - pts)
    }
}

mod lifecycle;
mod media;
mod outputs;
mod transport;

#[cfg(test)]
pub(crate) use outputs::{SharedDemand, shared_plan};

/// One bounded encoded video source on an [`Engine`].
#[derive(Clone)]
pub struct VideoSource {
    pub(crate) engine: Engine,
    pub(crate) key: u64,
    codec: VideoCodec,
}

impl std::fmt::Debug for VideoSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Source")
            .field("key", &self.key)
            .field("codec", &self.codec.name())
            .finish()
    }
}

impl PartialEq for VideoSource {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key && self.engine.same(&other.engine)
    }
}

impl Engine {
    /// Opens a video source. `capacity` bounds route edges plus publications;
    /// `layers` is the original format count (1..=3).
    pub async fn new_source(
        &self,
        codec: &str,
        capacity: usize,
        layers: usize,
        request_keyframe: Option<RecoveryCallback>,
    ) -> Result<VideoSource> {
        self.new_source_with(codec, capacity, layers, None, request_keyframe)
            .await
    }

    pub(crate) async fn new_source_with(
        &self,
        codec: &str,
        capacity: usize,
        layers: usize,
        relay: Option<RelayOptions>,
        request_keyframe: Option<RecoveryCallback>,
    ) -> Result<VideoSource> {
        let video =
            codec::parse_codec(codec).ok_or(Error::Invalid("native video codec is unsupported"))?;
        if self.is_closed() {
            return Err(Error::EngineClosed);
        }
        let key = self
            .call(move |app, _, _| app.new_source(video, capacity, layers, relay, request_keyframe))
            .await??;
        Ok(VideoSource {
            engine: self.clone(),
            key,
            codec: video,
        })
    }
}

impl VideoSource {
    pub(crate) fn handle(engine: Engine, key: u64, codec: VideoCodec) -> Self {
        Self { engine, key, codec }
    }

    pub fn codec(&self) -> &'static str {
        self.codec.name()
    }

    pub(crate) fn video_codec(&self) -> VideoCodec {
        self.codec
    }

    /// Runs once before outputs of one capture input and returns the plan the
    /// caller applies to that exact capture stream.
    pub async fn begin_frame(&self, pts: Duration) -> Result<OutputPlan> {
        self.begin_frame_at(pts, SystemTime::now()).await
    }

    pub(crate) async fn begin_frame_at(&self, pts: Duration, at: SystemTime) -> Result<OutputPlan> {
        let key = self.key;
        self.engine
            .call(move |app, conns, now| app.begin_frame(conns, key, pts, at, now))
            .await?
    }

    /// Applies one validated capture generation's ceilings.
    pub async fn configure_outputs(&self, ceilings: Vec<u32>) -> Result<()> {
        let key = self.key;
        self.engine
            .call(move |app, _, _| app.configure_outputs(key, &ceilings))
            .await?
    }

    /// Marks a failed output layer unavailable. The retirement of affected
    /// consumers runs within this call, on the shard.
    pub async fn mark_layer_unavailable(&self, layer: usize) -> Result<()> {
        let key = self.key;
        self.engine
            .call(move |app, conns, now| app.mark_layer_unavailable(conns, key, layer, now))
            .await?
    }

    /// Writes one encoded frame of output `layer`.
    pub async fn write_video(&self, layer: usize, frame: Frame) -> Result<()> {
        let key = self.key;
        self.engine
            .call(move |app, conns, now| app.write_video(conns, key, layer, frame, now))
            .await?
    }

    pub async fn set_format(&self, layer: usize, width: u32, height: u32) -> Result<()> {
        let key = self.key;
        self.engine
            .call(move |app, _, _| app.set_format(key, layer, width, height))
            .await?
    }

    pub async fn begin_generation(&self) {
        let key = self.key;
        let _ = self
            .engine
            .call(move |app, _, _| app.begin_generation(key))
            .await;
    }

    pub async fn set_lowest_layer_rate_controlled(&self, controlled: bool) {
        let key = self.key;
        let _ = self
            .engine
            .call(move |app, _, _| app.set_lowest_layer_rate_controlled(key, controlled))
            .await;
    }

    /// Records the room's relay ceiling. It never
    /// creates an encoder or starts the capture process by itself.
    pub async fn set_relay_profile(&self, profile: VideoProfile) -> Result<()> {
        if !profile.valid() {
            return Err(Error::Invalid("native relay profile is invalid"));
        }
        let key = self.key;
        self.engine
            .call(move |app, conns, now| app.set_relay_profile(conns, key, profile, now))
            .await?
    }

    pub async fn request_recovery_frame(&self) {
        let key = self.key;
        let _ = self
            .engine
            .call(move |app, _, _| {
                if let Ok(state) = app.source(key) {
                    state.request_recovery(None);
                }
            })
            .await;
    }

    pub async fn close(&self) {
        let key = self.key;
        let _ = self
            .engine
            .call(move |app, conns, now| app.close_source(conns, key, now))
            .await;
    }

    pub async fn is_closed(&self) -> bool {
        let key = self.key;
        self.engine
            .call(move |app, _, _| app.sources.get(&key).is_none_or(|state| state.closed))
            .await
            .unwrap_or(true)
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
impl App {
    /// A new group for a fixed slot, outside planning.
    pub(crate) fn new_test_group(&mut self, key: u64, slot: usize) -> MediaId {
        let anchor = self.anchor;
        let state = self.sources.get_mut(&key).expect("source");
        let relay = state.relay.is_some().then_some(state.format_count - 1);
        let bitrate = state.output_bitrates.get(slot).copied().unwrap_or(150_000);
        let media =
            outputs::new_group_media(&state.media, bitrate, relay, anchor).expect("group media");
        state.next_media += 1;
        let id = MediaId(state.next_media);
        state.group_media.insert(id, media);
        state.groups.insert_test_group(slot, id);
        id
    }
}
