//! The seams between `piik-media-edge` and native control: one ordered event
//! queue per share, wire conversions, error mapping and the punch wrapper.
//! The native Viewer reuses these so both roles map media facts identically.

use std::sync::Arc;

use futures_util::future::BoxFuture;
use piik_media_edge::{EdgeEvent, PcState, StateWatch};
use piik_native_control::media::{
    MediaError, MediaEvent, MediaEventKind, MediaEvents, MediaResult, PublicationQualitySample,
    PunchPreparation, QualitySample,
};
use piik_native_control::wire::IceCandidateInit as WireCandidate;
use piik_protocol::PunchPrediction;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Whether an event's owner is still current.
pub type Current = Arc<dyn Fn() -> bool + Send + Sync>;

/// Where a share's events go: native control's [`MediaEvents`] in the App.
pub trait EventSink: Send + Sync + 'static {
    /// Waits for queue space; false once the receiver has retired.
    fn send(&self, event: MediaEvent) -> BoxFuture<'_, bool>;
}

impl EventSink for MediaEvents {
    fn send(&self, event: MediaEvent) -> BoxFuture<'_, bool> {
        Box::pin(MediaEvents::send(self, event))
    }
}

/// Enqueues one share's events in order without blocking the caller. Media
/// sinks run on the engine shard and the audio mix holds its state lock, so
/// neither may wait for socket backpressure. Events stop once the share's
/// context ends.
#[derive(Clone)]
pub struct Emitter {
    share_id: Arc<str>,
    tx: mpsc::UnboundedSender<MediaEvent>,
    ctx: CancellationToken,
}

impl Emitter {
    /// The emitter and its forwarder; the caller awaits the forwarder before
    /// reporting the share's end so no media event follows it.
    pub fn new(
        share_id: &str,
        events: Arc<dyn EventSink>,
        ctx: CancellationToken,
    ) -> (Self, tokio::task::JoinHandle<()>) {
        let (tx, mut rx) = mpsc::unbounded_channel::<MediaEvent>();
        let forward_ctx = ctx.clone();
        let forwarder = tokio::spawn(async move {
            loop {
                let event = tokio::select! {
                    biased;
                    () = forward_ctx.cancelled() => return,
                    event = rx.recv() => event,
                };
                let Some(event) = event else { return };
                tokio::select! {
                    biased;
                    () = forward_ctx.cancelled() => return,
                    sent = events.send(event) => if !sent { return },
                }
            }
        });
        (
            Self {
                share_id: share_id.into(),
                tx,
                ctx,
            },
            forwarder,
        )
    }

    pub fn emit(&self, kind: MediaEventKind, current: Option<Current>) {
        if self.ctx.is_cancelled() {
            return;
        }
        let mut event = MediaEvent::new(&*self.share_id, kind);
        if let Some(current) = current {
            event = event.with_current(move || current());
        }
        let _ = self.tx.send(event);
    }
}

pub fn wire_candidate(candidate: piik_media_edge::IceCandidateInit) -> WireCandidate {
    WireCandidate {
        candidate: candidate.candidate,
        sdp_mid: candidate.sdp_mid,
        sdp_m_line_index: candidate.sdp_m_line_index,
        username_fragment: candidate.username_fragment,
    }
}

pub fn edge_candidate(candidate: WireCandidate) -> piik_media_edge::IceCandidateInit {
    piik_media_edge::IceCandidateInit {
        candidate: candidate.candidate,
        sdp_mid: candidate.sdp_mid,
        sdp_m_line_index: candidate.sdp_m_line_index,
        username_fragment: candidate.username_fragment,
    }
}

pub fn quality_sample(sample: piik_media_edge::QualitySample) -> QualitySample {
    QualitySample {
        sample_timestamp_ms: sample.sample_timestamp_ms,
        sample_window_ms: sample.sample_window_ms,
        rtp_stats_id: sample.rtp_stats_id,
        track_identifier: sample.track_identifier,
        state: sample.state,
        reason: sample.reason,
        interval_frames_sent: sample.interval_frames_sent,
        frames_per_second: sample.frames_per_second,
        bitrate_kbps: sample.bitrate_kbps,
        available_outgoing_kbps: sample.available_outgoing_kbps,
        width: sample.width,
        height: sample.height,
    }
}

pub fn publication_quality_sample(
    sample: piik_media_edge::PublicationQualitySample,
) -> PublicationQualitySample {
    let count = |value: usize| i64::try_from(value).unwrap_or(i64::MAX);
    PublicationQualitySample {
        sample_timestamp_ms: sample.sample_timestamp_ms,
        sample_window_ms: sample.sample_window_ms,
        rtp_stats_id: sample.rtp_stats_id,
        track_identifier: sample.track_identifier,
        codec: sample.codec,
        video_encoding_count: count(sample.video_encoding_count),
        active_video_encoding_count: count(sample.active_video_encoding_count),
        rid: sample.rid,
        interval_frames_sent: sample.interval_frames_sent,
        frames_per_second: sample.frames_per_second,
        width: sample.width,
        height: sample.height,
        bitrate_kbps: sample.bitrate_kbps,
        audio_bitrate_kbps: sample.audio_bitrate_kbps,
        available_outgoing_kbps: sample.available_outgoing_kbps,
        state: sample.state,
        reason: sample.reason,
    }
}

pub fn publication_media(
    media: piik_media_edge::PublicationMedia,
) -> piik_native_control::media::PublicationMedia {
    piik_native_control::media::PublicationMedia {
        codec: media.codec,
        layers: media
            .layers
            .into_iter()
            .map(|layer| piik_native_control::media::PublicationLayer {
                rid: layer.rid,
                width: layer.width,
                height: layer.height,
                bitrate: layer.bitrate,
            })
            .collect(),
        audio: media.audio,
        audio_bitrate: media.audio_bitrate,
    }
}

/// Media failures keep a capture process failure typed so its validated
/// codes still reach `share-ended`; every other message stays in logs.
pub fn media_error(error: piik_media_edge::Error) -> MediaError {
    match error {
        piik_media_edge::Error::Capture(error) => MediaError::Capture(error),
        error => MediaError::failed(error.to_string()),
    }
}

/// Joins two errors for the two facts the wire can carry: the first
/// structured process failure wins, then a source cancellation; other
/// causes are kept as text for logs.
pub fn join(first: MediaError, second: MediaError) -> MediaError {
    if first.failure_details().is_some() {
        return first;
    }
    if second.failure_details().is_some() {
        return second;
    }
    if first.source_canceled() {
        return first;
    }
    if second.source_canceled() {
        return second;
    }
    MediaError::failed(format!("{first}\n{second}"))
}

/// Joins an optional close failure, as `errors.Join(err, stream.Close())`.
pub fn join_close(error: MediaError, close: Result<(), piik_native_capture::Error>) -> MediaError {
    match close {
        Ok(()) => error,
        Err(close) => join(error, MediaError::Capture(close)),
    }
}

/// The candidate, state and path facts every outbound edge reports.
pub enum EdgeFact {
    Candidate(Option<WireCandidate>, Current),
    State(PcState, StateWatch),
    Path(piik_media_edge::SelectedPair, StateWatch),
}

/// Projects the media facts of an edge event; channel and transport events
/// belong to the input owner.
pub fn edge_fact(event: &EdgeEvent) -> Option<EdgeFact> {
    match event {
        EdgeEvent::LocalCandidate { candidate, current } => Some(EdgeFact::Candidate(
            candidate.clone().map(wire_candidate),
            current.clone(),
        )),
        EdgeEvent::State { state, current } => Some(EdgeFact::State(*state, current.clone())),
        EdgeEvent::Path { pair, current } => Some(EdgeFact::Path(pair.clone(), current.clone())),
        EdgeEvent::Channel(_) | EdgeEvent::InputTransport(_) => None,
    }
}

/// Emits an edge's candidate, state and path events for `connection_id`.
/// `owns` reports whether this exact edge is still the connection's owner;
/// state-carrying events also require the connection to remain in that state.
pub fn emit_edge_fact(
    emitter: &Emitter,
    connection_id: &str,
    fact: EdgeFact,
    owns: Option<Current>,
) {
    let owned = move |extra: Current| -> Current {
        match &owns {
            Some(owns) => {
                let owns = owns.clone();
                Arc::new(move || owns() && extra())
            }
            None => extra,
        }
    };
    let connection_id = connection_id.to_owned();
    match fact {
        EdgeFact::Candidate(candidate, current) => emitter.emit(
            MediaEventKind::EdgeCandidate {
                connection_id,
                candidate,
            },
            Some(owned(current)),
        ),
        EdgeFact::State(state, watch) => emitter.emit(
            MediaEventKind::EdgeState {
                connection_id,
                state: state.as_str().to_owned(),
            },
            Some(owned(Arc::new(move || watch.is(state)))),
        ),
        EdgeFact::Path(pair, watch) => emitter.emit(
            MediaEventKind::EdgePath {
                connection_id,
                local_type: pair.local.as_str().to_owned(),
                remote_type: pair.remote.as_str().to_owned(),
                nat_traversal_path: pair.nat_traversal_path,
            },
            // The pair is reported for the connected period it describes.
            Some(owned(Arc::new(move || watch.is(PcState::Connected)))),
        ),
    }
}

/// `mediaedge.PunchPreparation` behind the control contract.
pub struct Punch(pub piik_media_edge::PunchPreparation);

impl PunchPreparation for Punch {
    fn prepare(&self) -> BoxFuture<'_, MediaResult<Option<PunchPrediction>>> {
        Box::pin(async move {
            let prediction = self.0.prepare().await.map_err(media_error)?;
            Ok(Some(PunchPrediction {
                address: prediction.address,
                port: prediction.port,
                step: prediction.step,
            }))
        })
    }

    fn run(&self, remote: PunchPrediction) -> BoxFuture<'_, bool> {
        Box::pin(self.0.run(piik_media_edge::PunchPrediction {
            address: remote.address,
            port: remote.port,
            step: remote.step,
        }))
    }
}

pub fn punch(preparation: piik_media_edge::PunchPreparation) -> Arc<dyn PunchPreparation> {
    Arc::new(Punch(preparation))
}
