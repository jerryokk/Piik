//! The media sessions behind the control session.
//!
//! [`HostMedia`] and [`ViewerMedia`] mirror exactly the native Host/Viewer
//! session methods that native control calls; their factories start one
//! session per role. Media owns WebRTC, capture and edges; control owns the
//! wire, role exclusivity, operation admission and event delivery.

use std::fmt;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::sync::Arc;

use futures_util::future::BoxFuture;
use piik_loopback::CancellationToken;
use piik_native_capture::{CaptureTarget, Failure, VideoOptions, VideoProfile};
use piik_protocol::{PunchPrediction, QualitySettings, RemoteInputPeer};
use piik_remote_input::State as RemoteInputState;
use serde::Serialize;

use crate::events::Outbox;
use crate::wire::IceCandidateInit;

/// One STUN server's URLs, already validated by control.
pub type IceServerUrls = Vec<String>;

/// A media failure. Only [`MediaError::source_canceled`] and
/// [`MediaError::failure_details`] reach the wire; messages stay in logs.
#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    /// The user dismissed the system source-selection dialog.
    #[error("capture source selection canceled")]
    SourceCanceled,
    #[error(transparent)]
    Capture(#[from] piik_native_capture::Error),
    #[error(transparent)]
    RemoteInput(#[from] piik_remote_input::Error),
    #[error("{0}")]
    Failed(String),
}

impl MediaError {
    pub fn failed(message: impl Into<String>) -> Self {
        Self::Failed(message.into())
    }

    pub fn source_canceled(&self) -> bool {
        matches!(
            self,
            Self::SourceCanceled | Self::Capture(piik_native_capture::Error::SourceCanceled)
        )
    }

    /// Validated helper failure codes of a capture process failure.
    pub fn failure_details(&self) -> Option<&Failure> {
        match self {
            Self::Capture(error) => error.failure_details(),
            _ => None,
        }
    }
}

pub type MediaResult<T> = Result<T, MediaError>;

/// The capture profile a Host share applies.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QualityProfile {
    pub video: VideoProfile,
    pub audio_bitrate: u32,
}

impl QualityProfile {
    pub fn valid(&self) -> bool {
        self.video.valid()
            && (piik_native_audio::MIN_BITRATE..=piik_native_audio::MAX_SEND_BITRATE)
                .contains(&self.audio_bitrate)
    }

    /// The wire can express more than the integer capture API. A fractional
    /// or wider value yields the invalid zero profile instead of a truncated
    /// or wrapped one.
    pub fn from_settings(settings: &QualitySettings) -> Self {
        let framerate = settings.max_framerate;
        let integral = |value: i64| u32::try_from(value).ok();
        let (Some(width), Some(height), Some(bitrate)) = (
            integral(settings.resolution.width),
            integral(settings.resolution.height),
            integral(settings.max_bitrate),
        ) else {
            return Self::default();
        };
        if !(1.0..=f64::from(u32::MAX)).contains(&framerate) || framerate.fract() != 0.0 {
            return Self::default();
        }
        let audio_bitrate = settings
            .max_audio_bitrate
            .filter(|bitrate| *bitrate != 0)
            .or(piik_protocol::DEFAULT_QUALITY_SETTINGS.max_audio_bitrate)
            .and_then(|bitrate| u32::try_from(bitrate).ok())
            .unwrap_or_default();
        Self {
            video: VideoProfile {
                width,
                height,
                framerate: framerate as u32,
                bitrate,
                preference: settings.degradation_preference.as_str().to_owned(),
            },
            audio_bitrate,
        }
    }
}

/// The peer mapping target of a run-punch prediction: a public IPv4 unicast
/// address and the stepped port.
pub fn punch_target(prediction: &PunchPrediction) -> MediaResult<SocketAddrV4> {
    let unsupported = || MediaError::failed("unsupported peer mapping observation");
    let address: Ipv4Addr = prediction.address.parse().map_err(|_| unsupported())?;
    if address.is_unspecified()
        || address.is_loopback()
        || address.is_multicast()
        || address.is_broadcast()
        || address.is_link_local()
        || address.is_private()
        || !(0..=16).contains(&prediction.step)
        || !(1024..=65535).contains(&prediction.port)
    {
        return Err(unsupported());
    }
    let port = u16::try_from(prediction.port + 4 * prediction.step).map_err(|_| unsupported())?;
    Ok(SocketAddrV4::new(address, port))
}

/// `mediaedge.QualitySample`: one outbound edge's interval statistics.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct QualitySample {
    pub sample_timestamp_ms: i64,
    pub sample_window_ms: i64,
    pub rtp_stats_id: String,
    pub track_identifier: String,
    pub state: String,
    pub reason: Option<String>,
    pub interval_frames_sent: u64,
    pub frames_per_second: f64,
    pub bitrate_kbps: f64,
    pub available_outgoing_kbps: f64,
    pub width: u32,
    pub height: u32,
}

/// `mediaedge.PublicationQualitySample`, serialized flat into its event.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicationQualitySample {
    pub sample_timestamp_ms: i64,
    pub sample_window_ms: i64,
    pub rtp_stats_id: String,
    pub track_identifier: String,
    pub codec: String,
    pub video_encoding_count: i64,
    pub active_video_encoding_count: i64,
    pub rid: String,
    pub interval_frames_sent: u64,
    pub frames_per_second: f64,
    pub width: u32,
    pub height: u32,
    pub bitrate_kbps: f64,
    pub audio_bitrate_kbps: f64,
    pub available_outgoing_kbps: Option<f64>,
    pub state: String,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicationLayer {
    pub rid: String,
    pub width: u32,
    pub height: u32,
    pub bitrate: u32,
}

/// `mediaedge.PublicationMedia`: what one SFU publication carries.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicationMedia {
    pub codec: String,
    pub layers: Vec<PublicationLayer>,
    pub audio: bool,
    pub audio_bitrate: u32,
}

/// One media event; control adds the version and wire type.
pub struct MediaEvent {
    pub share_id: String,
    pub kind: MediaEventKind,
    /// Revalidates the event's owner when it reaches the socket writer.
    pub current: Option<Box<dyn Fn() -> bool + Send + Sync>>,
}

impl MediaEvent {
    pub fn new(share_id: impl Into<String>, kind: MediaEventKind) -> Self {
        Self {
            share_id: share_id.into(),
            kind,
            current: None,
        }
    }

    pub fn with_current(mut self, current: impl Fn() -> bool + Send + Sync + 'static) -> Self {
        self.current = Some(Box::new(current));
        self
    }
}

impl fmt::Debug for MediaEvent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MediaEvent")
            .field("share_id", &self.share_id)
            .field("kind", &self.kind)
            .field("current", &self.current.is_some())
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum MediaEventKind {
    /// `None` ends gathering.
    EdgeCandidate {
        connection_id: String,
        candidate: Option<IceCandidateInit>,
    },
    EdgeState {
        connection_id: String,
        state: String,
    },
    EdgePath {
        connection_id: String,
        local_type: String,
        remote_type: String,
        nat_traversal_path: String,
    },
    EdgeQuality {
        connection_id: String,
        quality: QualitySample,
    },
    /// Host only.
    PublicationCandidate {
        publication_generation: String,
        connection_id: String,
        candidate: Option<IceCandidateInit>,
    },
    /// Host only.
    PublicationState {
        publication_generation: String,
        connection_id: String,
        state: String,
    },
    /// Host only.
    PublicationQuality {
        publication_generation: String,
        connection_id: String,
        sample: PublicationQualitySample,
    },
    /// Host only.
    CaptureState { state: String },
    /// Host only.
    AudioState {
        source_audio: bool,
        microphone: bool,
        failed: bool,
    },
}

/// Delivers media events onto the owning control connection.
#[derive(Clone)]
pub struct MediaEvents {
    outbox: Arc<dyn Outbox>,
    cancel: CancellationToken,
}

impl MediaEvents {
    pub(crate) fn new(outbox: Arc<dyn Outbox>, cancel: CancellationToken) -> Self {
        Self { outbox, cancel }
    }

    /// Waits for queue space; returns false once the control session or its
    /// connection has retired.
    pub async fn send(&self, event: MediaEvent) -> bool {
        let control = crate::events::media_event(event);
        tokio::select! {
            sent = self.outbox.send(control) => sent,
            () = self.cancel.cancelled() => false,
        }
    }
}

impl fmt::Debug for MediaEvents {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MediaEvents")
            .finish_non_exhaustive()
    }
}

/// One prepared UDP hole-punching attempt (`mediaedge.PunchPreparation`).
pub trait PunchPreparation: Send + Sync {
    /// Observes this side's mapping; `None` when no prediction is available.
    fn prepare(&self) -> BoxFuture<'_, MediaResult<Option<PunchPrediction>>>;
    /// Runs the probe toward `remote`; true when the path connected.
    fn run(&self, remote: PunchPrediction) -> BoxFuture<'_, bool>;
}

/// One SFU publication of a Host share (`mediaedge.Publication`).
pub trait Publication: Send + Sync {
    fn media(&self) -> PublicationMedia;
    fn set_answer<'a>(&'a self, sdp: &'a str) -> BoxFuture<'a, MediaResult<()>>;
    fn add_remote_candidate(
        &self,
        candidate: Option<IceCandidateInit>,
    ) -> BoxFuture<'_, MediaResult<()>>;
    /// `count` is in 0..=3.
    fn set_active_count(&self, count: usize) -> BoxFuture<'_, MediaResult<()>>;
}

/// The outbound edge operations shared by Host and relaying Viewer.
pub trait OutboundMedia: Send + Sync {
    fn share_id(&self) -> &str;
    fn set_answer<'a>(
        &'a self,
        connection_id: &'a str,
        sdp: &'a str,
        offer_id: &'a str,
    ) -> BoxFuture<'a, MediaResult<()>>;
    fn add_candidate<'a>(
        &'a self,
        connection_id: &'a str,
        candidate: Option<IceCandidateInit>,
    ) -> BoxFuture<'a, MediaResult<()>>;
    fn close_edge<'a>(&'a self, connection_id: &'a str) -> BoxFuture<'a, ()>;
    /// The prepared punch for `connection_id`, if still current.
    fn punch(&self, connection_id: &str) -> Option<Arc<dyn PunchPreparation>>;
    fn close_punch(&self, connection_id: &str);
}

/// How a Host share ended without its owner closing it.
pub type HostEnded = MediaResult<()>;

/// The policy application prepared by
/// [`HostMedia::prepare_remote_input_policy`]; it runs when polled.
pub type PolicyApply = BoxFuture<'static, MediaResult<()>>;

/// One Host share (`nativehost.Session`).
pub trait HostMedia: OutboundMedia {
    fn codec(&self) -> String;
    fn has_audio(&self) -> bool;
    /// Present when the source audio state is known.
    fn source_audio(&self) -> Option<bool>;

    fn prepare_edge<'a>(
        &'a self,
        connection_id: &'a str,
        ice_servers: Vec<IceServerUrls>,
        punched: bool,
    ) -> BoxFuture<'a, MediaResult<String>>;
    fn prepare_local_edge<'a>(
        &'a self,
        connection_id: &'a str,
        offer_id: &'a str,
        restart: bool,
    ) -> BoxFuture<'a, MediaResult<String>>;
    fn begin_punch(
        &self,
        connection_id: &str,
        ice_servers: Vec<IceServerUrls>,
        secret: &str,
    ) -> MediaResult<Arc<dyn PunchPreparation>>;

    fn set_paused(&self, paused: bool);
    fn update_profile(&self, profile: QualityProfile) -> BoxFuture<'_, MediaResult<()>>;
    fn replace_source(
        &self,
        options: VideoOptions,
        audio_enabled: bool,
        exclude_audio: Option<CaptureTarget>,
    ) -> BoxFuture<'_, MediaResult<()>>;
    fn set_microphone(
        &self,
        enabled: Option<bool>,
        gain: Option<f64>,
        device_id: Option<String>,
    ) -> BoxFuture<'_, MediaResult<()>>;
    fn set_volume(&self, volume: f64) -> MediaResult<()>;

    /// Resolves on the next remote-input state change.
    fn remote_input_changed(&self) -> BoxFuture<'_, ()>;
    fn remote_input_state(&self) -> RemoteInputState;
    /// Registers the authority-changing intent synchronously; the returned
    /// future applies it and may wait for the platform.
    fn prepare_remote_input_policy(
        &self,
        mode: &str,
        password: &str,
        co_play: bool,
    ) -> MediaResult<PolicyApply>;
    fn set_remote_input_peers(&self, peers: &[RemoteInputPeer]) -> MediaResult<()>;
    fn decide_remote_input(&self, request_id: &str, allow: bool) -> MediaResult<()>;
    fn set_remote_input_simultaneous(&self, enabled: bool) -> MediaResult<()>;

    /// Returns the offer SDP and the publication's media.
    fn prepare_publication<'a>(
        &'a self,
        generation: &'a str,
        connection_id: &'a str,
        ice_servers: Vec<IceServerUrls>,
    ) -> BoxFuture<'a, MediaResult<(String, PublicationMedia)>>;
    fn publication(
        &self,
        generation: &str,
        connection_id: &str,
    ) -> MediaResult<Arc<dyn Publication>>;
    fn close_publication(&self, generation: &str, connection_id: &str);

    /// Resolves once when the share ends. `None` means its owner closed it
    /// and no share-ended event follows.
    fn done(&self) -> BoxFuture<'_, Option<HostEnded>>;
    fn close(&self) -> BoxFuture<'_, MediaResult<()>>;
}

/// What a Viewer receiver answered.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OfferResult {
    pub answer_sdp: String,
    pub audio: bool,
    pub codec: String,
    pub reused: bool,
}

/// One Viewer share and its relay edges (`nativeviewer.Session`).
pub trait ViewerMedia: OutboundMedia {
    fn update_profile(&self, profile: VideoProfile) -> BoxFuture<'_, MediaResult<()>>;
    fn accept_offer<'a>(
        &'a self,
        connection_id: &'a str,
        sdp: &'a str,
        ice_servers: Vec<IceServerUrls>,
        reuse: bool,
        punched: bool,
    ) -> BoxFuture<'a, MediaResult<OfferResult>>;
    fn add_receiver_candidate<'a>(
        &'a self,
        connection_id: &'a str,
        candidate: Option<IceCandidateInit>,
    ) -> BoxFuture<'a, MediaResult<()>>;
    fn close_receiver<'a>(&'a self, connection_id: &'a str) -> BoxFuture<'a, ()>;
    fn prepare_edge<'a>(
        &'a self,
        source_connection_id: &'a str,
        connection_id: &'a str,
        ice_servers: Vec<IceServerUrls>,
        punched: bool,
    ) -> BoxFuture<'a, MediaResult<String>>;
    fn prepare_local_edge<'a>(
        &'a self,
        source_connection_id: &'a str,
        connection_id: &'a str,
        offer_id: &'a str,
        restart: bool,
    ) -> BoxFuture<'a, MediaResult<String>>;
    /// An empty `source_connection_id` prepares a receiver.
    fn begin_punch(
        &self,
        connection_id: &str,
        source_connection_id: &str,
        ice_servers: Vec<IceServerUrls>,
        secret: &str,
    ) -> MediaResult<Arc<dyn PunchPreparation>>;
    fn close(&self) -> BoxFuture<'_, MediaResult<()>>;
}

/// `nativehost.Options` without the capture provider and remote-input
/// manager, which the factory owns.
#[derive(Debug, Clone)]
pub struct HostOptions {
    pub share_id: String,
    pub video: VideoOptions,
    pub profile: QualityProfile,
    pub audio_enabled: bool,
    pub exclude_audio: Option<CaptureTarget>,
    pub microphone_mixing: bool,
    pub edge_capacity: usize,
    pub port_mapping: bool,
    /// The opted-in page can own input requests; this advertises support,
    /// not permission for any Viewer.
    pub remote_input: bool,
    pub events: MediaEvents,
}

#[derive(Debug, Clone)]
pub struct ViewerOptions {
    pub share_id: String,
    pub edge_capacity: usize,
    pub port_mapping: bool,
    pub events: MediaEvents,
}

pub trait HostMediaFactory: Send + Sync {
    /// Starts one Host share. `cancel` fires when stop-share or close
    /// overtakes the start.
    fn start(
        &self,
        cancel: CancellationToken,
        options: HostOptions,
    ) -> BoxFuture<'_, MediaResult<Arc<dyn HostMedia>>>;
}

pub trait ViewerMediaFactory: Send + Sync {
    /// Starts one Viewer share. `cancel` fires when the control session closes.
    fn start(
        &self,
        cancel: CancellationToken,
        options: ViewerOptions,
    ) -> BoxFuture<'_, MediaResult<Arc<dyn ViewerMedia>>>;
}
