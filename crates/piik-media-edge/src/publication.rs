//! Native Host to SFU simulcast. A publication reuses the edge's bounded
//! signaling owner without registering a P2P edge.

use std::time::{Instant, SystemTime};

use piik_media_forwarding::{Source, publication_rid};
use piik_media_session::{
    AudioSlot, AudioSlotOptions, PublicationCounters, PublicationOptions, SimulcastSend,
    SourcePacket, UpstreamKeyframe,
};
use piik_media_transport::sdp::{OPUS_PT, VideoCodec};
use piik_media_transport::str0m::Rtc;
use piik_media_transport::str0m::media::{Direction, MediaKind, Pt, Simulcast, SimulcastLayer};
use piik_media_transport::{ConnEvent, ConnId, Connection, Connections, IceCandidateInit, PcState};

use crate::codec;
use crate::edge::{AudioOutput, EdgeOptions, admit};
use crate::engine::{App, Engine, Owner, next_key};
use crate::error::{Error, Result};
use crate::events::{ChannelEvent, EdgeEvent, StateWatch, discard};
use crate::groups::MediaId;
use crate::quality::{
    PublicationLayer, PublicationMedia, PublicationQualitySample, unix_millis, window,
};
use crate::signal::{Link, Signal, trickle_sdp};
use crate::source::{Consumer, ORIGINAL, VideoSource};

/// The current and one overlapping publication per source.
const MAX_PUBLICATIONS: usize = 2;

pub(crate) struct PublicationState {
    pub source: u64,
    pub media: MediaId,
    pub audio: Option<u64>,
    pub conn: ConnId,
    pub signal: Signal,
    pub transport: Option<SimulcastSend>,
    pub audio_output: Option<AudioOutput>,
    codec: VideoCodec,
    initial_bitrate: i64,
    quality_at: Option<SystemTime>,
    quality: PublicationCounters,
    audio_bytes: u64,
    pub closed: bool,
}

impl PublicationState {
    pub fn forward(
        &mut self,
        rtc: &mut Rtc,
        source: &Source,
        packets: &[SourcePacket],
        changed: bool,
        now: Instant,
        requests: &mut Vec<UpstreamKeyframe>,
    ) {
        let Some(transport) = &mut self.transport else {
            return;
        };
        if changed {
            requests.extend(transport.reconcile(rtc, source));
        }
        for packet in packets {
            requests.extend(transport.forward(rtc, source, packet, now));
        }
    }

    pub fn replace_source(
        &mut self,
        rtc: &mut Rtc,
        current: &Source,
        next: &Source,
    ) -> Vec<UpstreamKeyframe> {
        self.transport
            .as_mut()
            .and_then(|transport| transport.replace_source(rtc, current, next).ok())
            .unwrap_or_default()
    }

    pub fn required_active_count(&self) -> usize {
        self.transport
            .as_ref()
            .map_or(0, SimulcastSend::required_active_count)
    }

    pub fn lowest_layer_budget(&self, source: &Source) -> i64 {
        self.transport
            .as_ref()
            .map_or(self.initial_bitrate, |transport| {
                transport.lowest_layer_budget(source)
            })
    }
}

/// Whether any declared base output has failed.
fn outputs_unavailable(bitrates: &[u32], formats: usize) -> bool {
    bitrates[..bitrates.len().min(formats)].contains(&0)
}

struct NewPublication {
    source: u64,
    audio: Option<u64>,
    codec: VideoCodec,
    signal: Signal,
}

impl App {
    fn admit_publication(&self, source: u64, audio: Option<u64>) -> Result<()> {
        if self.closed {
            return Err(Error::Invalid("native publication is unavailable"));
        }
        let state = self
            .sources
            .get(&source)
            .filter(|state| !state.closed)
            .ok_or(Error::Invalid("native publication is unavailable"))?;
        if outputs_unavailable(&state.output_bitrates, state.format_count) {
            return Err(Error::Invalid("native publication output is unavailable"));
        }
        if state.publications.len() >= MAX_PUBLICATIONS {
            return Err(Error::Invalid("native publication is unavailable"));
        }
        admit(&state.edges, state.reservations, state.capacity, false)?;
        if let Some(audio) = audio {
            let state = self
                .audios
                .get(&audio)
                .filter(|state| !state.closed)
                .ok_or(Error::Invalid("native audio source is closed"))?;
            admit(&state.edges, state.reservations, state.capacity, false)?;
        }
        Ok(())
    }

    fn insert_publication(
        &mut self,
        conns: &mut Connections,
        new: NewPublication,
        conn: Connection,
    ) -> Result<u64> {
        self.admit_publication(new.source, new.audio)?;
        let key = next_key();
        let id = conns.insert(conn);
        self.owners.insert(id, Owner::Publication(key));
        self.publications.insert(
            key,
            PublicationState {
                source: new.source,
                media: ORIGINAL,
                audio: new.audio,
                conn: id,
                signal: new.signal,
                transport: None,
                audio_output: None,
                codec: new.codec,
                initial_bitrate: self.settings.initial_bitrate,
                quality_at: None,
                quality: PublicationCounters::default(),
                audio_bytes: 0,
                closed: false,
            },
        );
        if let Some(state) = self.sources.get_mut(&new.source) {
            state.publications.insert(key);
            state.reservations += 1;
        }
        if let Some(audio) = new.audio
            && let Some(state) = self.audios.get_mut(&audio)
        {
            state.publications.insert(key);
            state.reservations += 1;
        }
        Ok(key)
    }

    pub(crate) fn close_publication(&mut self, conns: &mut Connections, key: u64, _now: Instant) {
        let Some(mut publication) = self.publications.remove(&key) else {
            return;
        };
        publication.closed = true;
        if let Some(conn) = conns.get_mut(publication.conn)
            && let Some(transport) = &mut publication.transport
        {
            transport.close(conn.rtc());
        }
        if let Some(output) = &mut publication.audio_output {
            output.slot.close();
        }
        self.owners.remove(&publication.conn);
        conns.close(publication.conn);
        publication.signal.close();
        if let Some(state) = self.sources.get_mut(&publication.source) {
            state.publications.remove(&key);
            state.reservations = state.reservations.saturating_sub(1);
            state.groups.forget(&Consumer::Publication(key));
            state.request_recovery(None);
        }
        if let Some(audio) = publication.audio
            && let Some(state) = self.audios.get_mut(&audio)
        {
            state.publications.remove(&key);
            state.reservations = state.reservations.saturating_sub(1);
        }
    }

    fn connect_publication(&mut self, conns: &mut Connections, key: u64) -> bool {
        let App {
            publications,
            sources,
            audios,
            ..
        } = self;
        let Some(publication) = publications.get_mut(&key) else {
            return false;
        };
        let (Some(state), Some(conn)) = (
            sources.get(&publication.source),
            conns.get_mut(publication.conn),
        ) else {
            return false;
        };
        let (Some(source), Some(transport)) = (
            state.source_of(publication.media),
            &mut publication.transport,
        ) else {
            return false;
        };
        if transport.set_connected(conn.rtc(), source).is_err() {
            return false;
        }
        if let (Some(output), Some(audio)) = (
            &mut publication.audio_output,
            publication.audio.and_then(|key| audios.get(&key)),
        ) && output.attachment.is_none()
        {
            output.attachment = output.slot.replace_source(&audio.source).ok();
        }
        true
    }

    pub(crate) fn publication_event(
        &mut self,
        conns: &mut Connections,
        key: u64,
        event: ConnEvent,
        now: Instant,
    ) {
        let Some(publication) = self.publications.get_mut(&key) else {
            return;
        };
        match event {
            ConnEvent::State(state) => {
                let transport = !publication.signal.channels.is_empty();
                if state == PcState::Connected {
                    if !self.connect_publication(conns, key) {
                        if let Some(conn) = conns.get_mut(self.publications[&key].conn) {
                            conn.close();
                        }
                        return;
                    }
                    let source = self.publications[&key].source;
                    if let Some(state) = self.sources.get_mut(&source) {
                        state.request_recovery(None);
                    }
                }
                if let Some(publication) = self.publications.get_mut(&key) {
                    publication.signal.on_state(state, transport);
                }
            }
            ConnEvent::ChannelOpen(channel) => publication
                .signal
                .emit(EdgeEvent::Channel(ChannelEvent::Open(channel))),
            ConnEvent::ChannelData(channel, data) => {
                publication
                    .signal
                    .emit(EdgeEvent::Channel(ChannelEvent::Data {
                        channel,
                        binary: data.binary,
                        data: data.data,
                    }));
            }
            ConnEvent::ChannelClose(channel) => publication
                .signal
                .emit(EdgeEvent::Channel(ChannelEvent::Close(channel))),
            ConnEvent::Rtc(event) => {
                let App {
                    publications,
                    sources,
                    ..
                } = self;
                let Some(publication) = publications.get_mut(&key) else {
                    return;
                };
                let Some(state) = sources.get_mut(&publication.source) else {
                    return;
                };
                let request = {
                    let (Some(source), Some(transport), Some(conn)) = (
                        state.source_of(publication.media),
                        &mut publication.transport,
                        conns.get_mut(publication.conn),
                    ) else {
                        return;
                    };
                    transport.handle_event(conn.rtc(), source, &event)
                };
                if let Some(request) = request {
                    state.keyframe(publication.media, request, now);
                }
            }
        }
    }

    pub(crate) fn poll_publication(&mut self, conns: &mut Connections, key: u64, now: Instant) {
        let App {
            publications,
            sources,
            ..
        } = self;
        let Some(publication) = publications.get_mut(&key) else {
            return;
        };
        let (Some(state), Some(conn)) = (
            sources.get(&publication.source),
            conns.get_mut(publication.conn),
        ) else {
            return;
        };
        if let (Some(source), Some(transport)) = (
            state.source_of(publication.media),
            &mut publication.transport,
        ) {
            transport.poll(conn.rtc(), source, now);
        }
    }

    fn publication_offer(
        &mut self,
        conns: &mut Connections,
        key: u64,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<String> {
        let App {
            publications,
            sources,
            ..
        } = self;
        let publication = publications.get_mut(&key).ok_or(Error::Closed)?;
        let conn = conns.get_mut(publication.conn).ok_or(Error::Closed)?;
        let source = sources
            .get(&publication.source)
            .and_then(|state| state.source_of(publication.media))
            .ok_or(Error::Closed)?;
        let count = source.layer_count();
        let audio = publication.audio.is_some();
        let mut mids = (None, None);
        let offer = publication.signal.offer(conn, false, |api| {
            let simulcast = (count > 1).then(|| {
                let mut simulcast = Simulcast::new();
                for layer in 0..count {
                    simulcast.add_send_layer(SimulcastLayer::new(publication_rid(layer, count)));
                }
                simulcast
            });
            mids.0 =
                Some(api.add_media(MediaKind::Video, Direction::SendOnly, None, None, simulcast));
            if audio {
                mids.1 =
                    Some(api.add_media(MediaKind::Audio, Direction::SendOnly, None, None, None));
            }
        })?;
        if let Some(mid) = mids.0 {
            publication.transport = Some(SimulcastSend::new(
                PublicationOptions {
                    mid,
                    pt: Pt::new_with_value(codec::video_pt(publication.codec)),
                    initial_bitrate: publication.initial_bitrate,
                },
                source,
            )?);
        }
        if let Some(mid) = mids.1 {
            publication.audio_output = Some(AudioOutput {
                slot: AudioSlot::new(AudioSlotOptions {
                    mid,
                    pt: Pt::new_with_value(OPUS_PT),
                    source_ceiling: piik_native_audio::MAX_BITRATE,
                }),
                attachment: None,
            });
        }
        let ufrag = conn.local_ufrag();
        let mid = mids.0.map_or_else(|| "0".to_owned(), |mid| mid.to_string());
        let signal = &publication.signal;
        // SFU publications use the survey only; no gateway mapping.
        signal.link.start_gathering(
            cancel,
            &signal.servers,
            false,
            &mid,
            &ufrag,
            signal.sink.clone(),
        );
        Ok(trickle_sdp(&offer))
    }

    fn publication_active_count(
        &mut self,
        conns: &mut Connections,
        key: u64,
        count: usize,
    ) -> Result<()> {
        let App {
            publications,
            sources,
            ..
        } = self;
        let publication = publications.get_mut(&key).ok_or(Error::Closed)?;
        let state = sources.get_mut(&publication.source).ok_or(Error::Closed)?;
        let source = state.source_of(publication.media).ok_or(Error::Closed)?;
        let conn = conns.get_mut(publication.conn).ok_or(Error::Closed)?;
        let transport = publication
            .transport
            .as_mut()
            .ok_or(Error::Invalid("native publication requires an offer"))?;
        transport.set_active_count(conn.rtc(), source, count)?;
        state.request_recovery(None);
        Ok(())
    }

    fn publication_quality(
        &mut self,
        key: u64,
        now: SystemTime,
    ) -> Option<PublicationQualitySample> {
        let App {
            publications,
            sources,
            audios,
            ..
        } = self;
        let publication = publications.get_mut(&key)?;
        let state = sources.get(&publication.source)?;
        let source = state.source_of(publication.media)?;
        let current = publication.transport.as_ref()?.counters(source);
        let audio_bytes = publication
            .audio_output
            .as_ref()
            .map_or(0, |output| output.slot.egress().rtp_bytes);
        let _ = audios;
        let (previous, previous_audio, at) = (
            std::mem::replace(&mut publication.quality, current.clone()),
            std::mem::replace(&mut publication.audio_bytes, audio_bytes),
            publication.quality_at.replace(now),
        );
        let window = window(at, now)?;
        if current.egress.rtp_bytes < previous.egress.rtp_bytes || audio_bytes < previous_audio {
            return None;
        }
        let seconds = window.as_secs_f64();
        let video = current.egress.rtp_bytes - previous.egress.rtp_bytes;
        let audio = audio_bytes - previous_audio;
        let mut sample = PublicationQualitySample {
            sample_timestamp_ms: unix_millis(now),
            sample_window_ms: window.as_millis() as i64,
            rtp_stats_id: publication.conn.to_string(),
            track_identifier: source.id().to_owned(),
            codec: publication.codec.name().to_owned(),
            video_encoding_count: current.frames.len(),
            state: "unknown".into(),
            bitrate_kbps: ((video + audio) * 8) as f64 / seconds / 1000.0,
            audio_bitrate_kbps: (audio * 8) as f64 / seconds / 1000.0,
            ..PublicationQualitySample::default()
        };
        // The highest sent RID owns cadence and dimensions.
        for (index, frames) in current.frames.iter().enumerate() {
            let Some(before) = previous.frames.get(index) else {
                continue;
            };
            if frames <= before {
                continue;
            }
            sample.active_video_encoding_count += 1;
            sample.interval_frames_sent = frames - before;
            sample.rid = publication_rid(index, current.frames.len()).to_owned();
            let format = source.formats().get(index).copied().unwrap_or_default();
            sample.width = format.width;
            sample.height = format.height;
        }
        sample.frames_per_second = sample.interval_frames_sent as f64 / seconds;
        if current.observed {
            sample.available_outgoing_kbps = Some(current.target_bitrate as f64 / 1000.0);
            if publication.signal.state.is(PcState::Connected) && sample.interval_frames_sent > 0 {
                let limited = current.limited;
                sample.state = if limited { "degraded" } else { "healthy" }.into();
                sample.reason = Some(if limited { "bandwidth" } else { "none" }.into());
            }
        }
        Some(sample)
    }

    /// The descriptor the SFU admits.
    fn publication_media(&self, key: u64) -> Option<PublicationMedia> {
        let publication = self.publications.get(&key)?;
        let source = self
            .sources
            .get(&publication.source)?
            .source_of(publication.media)?;
        let formats = source.formats();
        let count = formats.len();
        Some(PublicationMedia {
            codec: publication.codec.name().to_owned(),
            layers: formats
                .iter()
                .enumerate()
                .map(|(index, format)| PublicationLayer {
                    rid: publication_rid(index, count).to_owned(),
                    width: format.width,
                    height: format.height,
                    bitrate: format.bitrate,
                })
                .collect(),
            audio: publication.audio.is_some(),
            audio_bitrate: publication
                .audio
                .and_then(|audio| self.audios.get(&audio))
                .map_or(0, |audio| audio.bitrate),
        })
    }
}

/// One Native Host to SFU publication. Cheap to clone.
#[derive(Clone)]
pub struct Publication {
    engine: Engine,
    key: u64,
    state: StateWatch,
}

impl std::fmt::Debug for Publication {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Publication")
            .field("state", &self.state.get())
            .finish()
    }
}

impl Engine {
    /// A new publication starts on the base source; independent lower-output
    /// groups are not part of its descriptor.
    pub async fn new_publication(
        &self,
        source: &VideoSource,
        options: EdgeOptions,
    ) -> Result<Publication> {
        if !source.engine.same(self)
            || options.local
            || options.connection_id.is_empty()
            || options.connection_id.len() > 256
            || options
                .audio
                .as_ref()
                .is_some_and(|audio| !audio.engine.same(self))
        {
            return Err(Error::Invalid("native publication input is invalid"));
        }
        let source_key = source.key;
        let audio_key = options.audio.as_ref().map(|audio| audio.key);
        self.call(move |app, _, _| app.admit_publication(source_key, audio_key))
            .await??;
        let settings = self.settings();
        let link = Link::open(settings, None, &options.ice_servers, false)?;
        let codec = source.video_codec();
        let mut conn = Connection::new(
            codec::rtc_config(&[codec], audio_key.is_some(), settings.initial_bitrate),
            Instant::now(),
        );
        link.bind(self.inbox(), &mut conn);
        let state = StateWatch::default();
        let mut signal = Signal::new(
            link,
            options.events.unwrap_or_else(discard),
            options.ice_servers,
            state.clone(),
        );
        // Publications report no selected pair.
        signal.report_path = false;
        let new = NewPublication {
            source: source_key,
            audio: audio_key,
            codec,
            signal,
        };
        let key = self
            .call(move |app, conns, _| app.insert_publication(conns, new, conn))
            .await??;
        Ok(Publication {
            engine: self.clone(),
            key,
            state,
        })
    }
}

impl Publication {
    pub fn state(&self) -> PcState {
        self.state.get()
    }

    pub fn state_watch(&self) -> StateWatch {
        self.state.clone()
    }

    /// Adds a data channel before the offer.
    pub async fn open_data_channel(&self, label: &str) -> Result<()> {
        let key = self.key;
        let label = label.to_owned();
        self.engine
            .call(move |app, _, _| {
                let publication = app.publications.get_mut(&key).ok_or(Error::Closed)?;
                if publication.signal.offered {
                    return Err(Error::Invalid("data channels belong in the initial offer"));
                }
                publication.signal.channels.push(label);
                Ok(())
            })
            .await?
    }

    /// A trickled simulcast offer, one RID per output.
    pub async fn create_offer(&self) -> Result<String> {
        let key = self.key;
        let cancel = self.engine.inner.cancel.clone();
        self.engine
            .call(move |app, conns, _| app.publication_offer(conns, key, &cancel))
            .await?
    }

    pub async fn set_answer(&self, answer: &str) -> Result<()> {
        let key = self.key;
        let answer = answer.to_owned();
        self.engine
            .call(move |app, conns, _| {
                let publication = app
                    .publications
                    .get_mut(&key)
                    .ok_or(Error::Invalid("native media edge is closed"))?;
                let conn = conns.get_mut(publication.conn).ok_or(Error::Closed)?;
                let audio = publication.audio.is_some();
                publication.signal.answer(conn, &answer, "", audio)
            })
            .await?
    }

    pub async fn add_remote_candidate(&self, candidate: Option<IceCandidateInit>) -> Result<()> {
        let key = self.key;
        let candidate = candidate.unwrap_or_default();
        self.engine
            .call(move |app, conns, _| {
                let Some(publication) = app.publications.get_mut(&key) else {
                    return Ok(());
                };
                let Some(conn) = conns.get_mut(publication.conn) else {
                    return Ok(());
                };
                publication.signal.candidate(conn, candidate)
            })
            .await?
    }

    /// Applies the SFU's requested RID count.
    pub async fn set_active_count(&self, count: usize) -> Result<()> {
        let key = self.key;
        self.engine
            .call(move |app, conns, _| app.publication_active_count(conns, key, count))
            .await?
    }

    pub async fn required_active_count(&self) -> usize {
        let key = self.key;
        self.engine
            .call(move |app, _, _| {
                app.publications
                    .get(&key)
                    .map_or(0, PublicationState::required_active_count)
            })
            .await
            .unwrap_or(0)
    }

    pub async fn lowest_layer_budget(&self) -> i64 {
        let key = self.key;
        self.engine
            .call(move |app, _, _| {
                let publication = app.publications.get(&key)?;
                let source = app
                    .sources
                    .get(&publication.source)?
                    .source_of(publication.media)?;
                Some(publication.lowest_layer_budget(source))
            })
            .await
            .ok()
            .flatten()
            .unwrap_or(0)
    }

    /// The next publication quality sample.
    pub async fn quality_sample(&self, now: SystemTime) -> Option<PublicationQualitySample> {
        let key = self.key;
        self.engine
            .call(move |app, _, _| app.publication_quality(key, now))
            .await
            .ok()
            .flatten()
    }

    /// The descriptor the SFU admits.
    pub async fn media(&self) -> Option<PublicationMedia> {
        let key = self.key;
        self.engine
            .call(move |app, _, _| app.publication_media(key))
            .await
            .ok()
            .flatten()
    }

    /// Closes the publication. Idempotent; releases the reservation and
    /// requests source recovery for the remaining consumers.
    pub async fn close(&self) {
        let key = self.key;
        let _ = self
            .engine
            .call(move |app, conns, now| app.close_publication(conns, key, now))
            .await;
        self.state.set(PcState::Closed);
    }
}
