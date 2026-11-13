//! One Opus source with the same bounded edge capacity as its video source.
//! Local PCM is encoded off the shard by the caller's task; relayed Opus RTP
//! arrives from a receiver on the shard.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use piik_media_forwarding::{CodecKind, InboundPacket, Source};
use piik_media_session::SourcePacket;
use piik_media_transport::Connections;
use piik_media_transport::sdp::OPUS_PT;
use piik_native_audio::{CHANNELS, Encoder, FRAME_BYTES, FRAME_SAMPLES, SAMPLE_RATE};

use crate::codec;
use crate::engine::{App, Engine, MAX_RELAY_CHILDREN, next_key};
use crate::error::{Error, Result};

/// The one SSRC every audio source binds; outputs rewrite it.
const AUDIO_SSRC: u32 = 1;

pub(crate) struct AudioState {
    pub source: Source,
    pub capacity: usize,
    /// Attached edges and whether each is a local bridge.
    pub edges: HashMap<u64, bool>,
    pub publications: HashSet<u64>,
    /// Route capacity held by publications.
    pub reservations: usize,
    /// Encoded payload bytes, for quality samples.
    pub bytes: u64,
    /// The configured encoder rate; zero for relayed audio.
    pub bitrate: u32,
    pub closed: bool,
    /// Test record of written payloads.
    #[cfg(test)]
    pub written: Vec<Arc<[u8]>>,
}

/// One received or encoded Opus packet.
pub(crate) struct AudioPacket {
    pub ext_seq: u64,
    pub ext_timestamp: u64,
    pub marker: bool,
    pub payload: Arc<[u8]>,
    pub arrival: Instant,
}

impl App {
    pub(crate) fn new_audio(&mut self, capacity: usize, bitrate: u32) -> Result<u64> {
        if !(1..=MAX_RELAY_CHILDREN + 1).contains(&capacity) {
            return Err(Error::Invalid(
                "native audio source capacity is outside the route bound",
            ));
        }
        if self.closed {
            return Err(Error::EngineClosed);
        }
        let mut source = Source::new(codec::opus_options("audio"))?;
        source.bind_layer(0, AUDIO_SSRC)?;
        let key = next_key();
        self.audios.insert(
            key,
            AudioState {
                source,
                capacity,
                edges: HashMap::new(),
                publications: HashSet::new(),
                reservations: 0,
                bytes: 0,
                bitrate,
                closed: false,
                #[cfg(test)]
                written: Vec::new(),
            },
        );
        Ok(key)
    }

    /// Records one packet and forwards it to every attached output. One
    /// output's write failure belongs to its connection, not this source.
    pub(crate) fn write_audio(
        &mut self,
        conns: &mut Connections,
        key: u64,
        packet: &AudioPacket,
        now: Instant,
    ) -> Result<()> {
        let App {
            audios,
            edges,
            publications,
            ..
        } = self;
        let state = audios
            .get_mut(&key)
            .filter(|state| !state.closed)
            .ok_or(Error::Invalid("native audio source is closed"))?;
        let info = InboundPacket::new(
            CodecKind::Opus,
            0,
            packet.ext_seq,
            packet.ext_timestamp,
            &packet.payload,
            packet.arrival,
        )
        .marker(packet.marker)
        .ssrc(AUDIO_SSRC)
        .payload_type(OPUS_PT);
        state.source.write_rtp(0, &info)?;
        state.bytes += packet.payload.len() as u64;
        #[cfg(test)]
        state.written.push(packet.payload.clone());
        let shared = SourcePacket {
            info,
            payload: packet.payload.clone(),
        };
        for edge in state.edges.keys() {
            let Some(edge) = edges.get_mut(edge) else {
                continue;
            };
            let (Some(output), Some(conn)) = (&mut edge.audio_output, conns.get_mut(edge.conn))
            else {
                continue;
            };
            if let Some(attachment) = output.attachment {
                output
                    .slot
                    .forward(conn.rtc(), attachment, &state.source, &shared, now);
            }
        }
        for publication in &state.publications {
            let Some(publication) = publications.get_mut(publication) else {
                continue;
            };
            let (Some(output), Some(conn)) = (
                &mut publication.audio_output,
                conns.get_mut(publication.conn),
            ) else {
                continue;
            };
            if let Some(attachment) = output.attachment {
                output
                    .slot
                    .forward(conn.rtc(), attachment, &state.source, &shared, now);
            }
        }
        Ok(())
    }

    pub(crate) fn close_audio(&mut self, conns: &mut Connections, key: u64, now: Instant) {
        let Some(state) = self.audios.get_mut(&key) else {
            return;
        };
        if state.closed {
            return;
        }
        state.closed = true;
        state.source.close();
        let edges: Vec<u64> = state.edges.keys().copied().collect();
        let publications: Vec<u64> = state.publications.iter().copied().collect();
        for edge in edges {
            self.close_edge(conns, edge, now);
        }
        for publication in publications {
            self.close_publication(conns, publication, now);
        }
        self.audios.remove(&key);
    }
}

/// The PCM encoder and RTP clock of a local audio source.
struct Encoding {
    encoder: Encoder,
    volume: f64,
    applied: f64,
    sequence: u64,
    timestamp: u64,
}

/// One Opus source on an [`Engine`]. Cheap to clone.
#[derive(Clone)]
pub struct AudioSource {
    pub(crate) engine: Engine,
    pub(crate) key: u64,
    encoding: Option<Arc<Mutex<Encoding>>>,
}

impl std::fmt::Debug for AudioSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AudioSource")
            .field("key", &self.key)
            .field("relayed", &self.encoding.is_none())
            .finish()
    }
}

impl PartialEq for AudioSource {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key && self.engine.same(&other.engine)
    }
}

impl Engine {
    /// Local PCM encoded once for every binding.
    pub async fn new_audio_source(&self, capacity: usize, bitrate: u32) -> Result<AudioSource> {
        let encoder = Encoder::new(bitrate)?;
        let configured = encoder.bitrate();
        let key = self
            .call(move |app, _, _| app.new_audio(capacity, configured))
            .await??;
        Ok(AudioSource {
            engine: self.clone(),
            key,
            encoding: Some(Arc::new(Mutex::new(Encoding {
                encoder,
                volume: 1.0,
                applied: 1.0,
                sequence: 0,
                timestamp: 0,
            }))),
        })
    }

    /// Received Opus RTP, forwarded unchanged.
    pub async fn new_relayed_audio_source(&self, capacity: usize) -> Result<AudioSource> {
        let key = self
            .call(move |app, _, _| app.new_audio(capacity, 0))
            .await??;
        Ok(AudioSource {
            engine: self.clone(),
            key,
            encoding: None,
        })
    }

    pub(crate) fn relayed_audio(&self, key: u64) -> AudioSource {
        AudioSource {
            engine: self.clone(),
            key,
            encoding: None,
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

/// Ramps over one stereo frame to avoid clicks and clips once before Opus
/// encoding.
pub(crate) fn scale_pcm(output: &mut [u8], input: &[u8], from: f64, to: f64) {
    for sample in 0..FRAME_SAMPLES {
        let gain = from + (to - from) * (sample + 1) as f64 / FRAME_SAMPLES as f64;
        for channel in 0..CHANNELS {
            let offset = (sample * CHANNELS + channel) * 2;
            let value = f64::from(i16::from_le_bytes([input[offset], input[offset + 1]])) * gain;
            let clipped = value.clamp(-32768.0, 32767.0) as i16;
            output[offset..offset + 2].copy_from_slice(&clipped.to_le_bytes());
        }
    }
}

impl AudioSource {
    /// Encoded payload bytes written by this source.
    pub async fn encoded_bytes(&self) -> Result<u64> {
        let key = self.key;
        self.engine
            .call(move |app, _, _| app.audios.get(&key).map_or(0, |state| state.bytes))
            .await
    }

    pub fn is_relayed(&self) -> bool {
        self.encoding.is_none()
    }

    /// Encodes and sends one PCM frame.
    pub async fn write_pcm(&self, pcm: &[u8], duration: Duration) -> Result<()> {
        self.write_pcm_with_dropped(pcm, duration, 0).await
    }

    /// `dropped` earlier frames advance the RTP clock, as Pion's sample writer
    /// does.
    pub async fn write_pcm_with_dropped(
        &self,
        pcm: &[u8],
        duration: Duration,
        dropped: u16,
    ) -> Result<()> {
        let Some(encoding) = &self.encoding else {
            return Err(Error::Invalid("native PCM sample is invalid"));
        };
        if duration.is_zero() || pcm.len() != FRAME_BYTES {
            return Err(Error::Invalid("native PCM sample is invalid"));
        }
        let packet = {
            let mut encoding = lock(encoding);
            let mut adjusted = [0u8; FRAME_BYTES];
            let mut input = pcm;
            if encoding.volume != 1.0 || encoding.applied != 1.0 {
                // One source gain precedes the shared encoder, so Peer, SFU and
                // local preview receive identical sound.
                scale_pcm(&mut adjusted, pcm, encoding.applied, encoding.volume);
                input = &adjusted;
            }
            encoding.applied = encoding.volume;
            let payload: Arc<[u8]> = Arc::from(encoding.encoder.encode(input)?);
            let samples = (duration.as_nanos() * u128::from(SAMPLE_RATE) / 1_000_000_000) as u64;
            encoding.timestamp += samples * u64::from(dropped);
            let packet = AudioPacket {
                ext_seq: encoding.sequence,
                ext_timestamp: encoding.timestamp,
                marker: false,
                payload,
                arrival: Instant::now(),
            };
            encoding.sequence += 1;
            encoding.timestamp += samples;
            packet
        };
        let key = self.key;
        self.engine
            .call(move |app, conns, now| app.write_audio(conns, key, &packet, now))
            .await?
    }

    /// 0..=2, applied to the next frame with a ramp.
    pub async fn set_volume(&self, volume: f64) -> Result<()> {
        let Some(encoding) = &self.encoding else {
            return Err(Error::Invalid("native source volume is invalid"));
        };
        if !volume.is_finite() || !(0.0..=2.0).contains(&volume) {
            return Err(Error::Invalid("native source volume is invalid"));
        }
        self.ensure_open().await?;
        lock(encoding).volume = volume;
        Ok(())
    }

    pub async fn set_bitrate(&self, bitrate: u32) -> Result<()> {
        let Some(encoding) = &self.encoding else {
            return Err(Error::Invalid("native audio source is unavailable"));
        };
        self.ensure_open().await?;
        let configured = {
            let mut encoding = lock(encoding);
            encoding.encoder.set_bitrate(bitrate)?;
            encoding.encoder.bitrate()
        };
        let key = self.key;
        self.engine
            .call(move |app, _, _| {
                if let Some(state) = app.audios.get_mut(&key) {
                    state.bitrate = configured;
                }
            })
            .await
    }

    /// The encoder's configured rate; zero for relayed audio.
    pub fn configured_bitrate(&self) -> u32 {
        self.encoding
            .as_ref()
            .map_or(0, |encoding| lock(encoding).encoder.bitrate())
    }

    async fn ensure_open(&self) -> Result<()> {
        let key = self.key;
        let open = self
            .engine
            .call(move |app, _, _| app.audios.get(&key).is_some_and(|state| !state.closed))
            .await?;
        if open {
            Ok(())
        } else {
            Err(Error::Invalid("native audio source is closed"))
        }
    }

    /// Retires every edge carrying this source.
    pub async fn close(&self) {
        let key = self.key;
        let _ = self
            .engine
            .call(move |app, conns, now| app.close_audio(conns, key, now))
            .await;
    }
}

#[cfg(test)]
mod tests;
