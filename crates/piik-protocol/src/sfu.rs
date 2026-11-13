//! Embedded SFU signaling. These messages travel only on the room's
//! authenticated WebSocket.

use serde_json::Value;

use crate::codec::{FromWire, Int, Obj, Result, ToWire, ensure, in_range, object, wire_enum};
use crate::constants::MAX_AUDIO_BITRATE;
use crate::scalars::{utf16_length, valid_opaque_id};
use crate::types::{IceCandidate, SdpType, SessionDescription, valid_revision};

wire_enum! {
    /// `sfuMediaSchema.codec`.
    pub enum SfuCodec {
        H264 = "h264",
        Vp8 = "vp8",
    }
}

/// One real publisher encoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SfuLayer {
    pub rid: String,
    pub width: Int,
    pub height: Int,
    pub bitrate: Int,
}

impl FromWire for SfuLayer {
    fn from_wire(value: &Value) -> Result<Self> {
        let keys = ["rid", "width", "height", "bitrate"];
        let f = object(value, &keys)?;
        let layer = Self {
            rid: f.string("rid")?,
            width: f.int("width")?,
            height: f.int("height")?,
            bitrate: f.int("bitrate")?,
        };
        f.require(&keys)?;
        Ok(layer)
    }
}

impl ToWire for SfuLayer {
    fn to_wire(&self) -> Value {
        Obj::new()
            .put("rid", &self.rid)
            .put("width", &self.width)
            .put("height", &self.height)
            .put("bitrate", &self.bitrate)
            .build()
    }
}

/// `sfuMediaSchema`: the publisher's real encodings, ordered low to high.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SfuMedia {
    pub codec: SfuCodec,
    pub layers: Vec<SfuLayer>,
    pub audio: bool,
    pub audio_bitrate: Int,
}

impl FromWire for SfuMedia {
    fn from_wire(value: &Value) -> Result<Self> {
        let keys = ["codec", "layers", "audio", "audioBitrate"];
        let f = object(value, &keys)?;
        f.require(&keys)?;
        let media = Self {
            codec: f.value("codec")?,
            layers: f.or_default("layers")?,
            audio: f.boolean("audio")?,
            audio_bitrate: f.int("audioBitrate")?,
        };
        ensure(
            (1..=3).contains(&media.layers.len())
                && in_range(media.audio_bitrate, 0, MAX_AUDIO_BITRATE)
                && !(media.audio && media.audio_bitrate == 0),
            "invalid SFU media",
        )?;
        let positive = |value: Int| in_range(value, 1, 0xffff_ffff);
        for (index, layer) in media.layers.iter().enumerate() {
            ensure(
                utf16_length(&layer.rid) <= 16
                    && !(layer.rid.is_empty() && media.layers.len() != 1)
                    && positive(layer.width)
                    && positive(layer.height)
                    && positive(layer.bitrate),
                "invalid SFU layer",
            )?;
            ensure(
                media.layers[..index]
                    .iter()
                    .all(|previous| previous.rid != layer.rid),
                "duplicate SFU RID",
            )?;
        }
        Ok(media)
    }
}

impl ToWire for SfuMedia {
    fn to_wire(&self) -> Value {
        Obj::new()
            .put("codec", &self.codec)
            .put("layers", &self.layers)
            .put("audio", &self.audio)
            .put("audioBitrate", &self.audio_bitrate)
            .build()
    }
}

wire_enum! {
    /// `sfu-signal` kind.
    pub enum SfuSignalKind {
        Subscribe = "subscribe",
        Description = "description",
        Candidate = "candidate",
        Media = "media",
        Layers = "layers",
    }
}

/// `sfu-signal`, sent by both sides. The kind decides which optional member
/// is allowed; decoding enforces that pairing.
#[derive(Debug, Clone, PartialEq)]
pub struct SfuSignalMessage {
    pub revision: Int,
    pub publication_generation: String,
    pub connection_id: String,
    pub kind: SfuSignalKind,
    pub description: Option<SessionDescription>,
    pub candidate: Option<IceCandidate>,
    pub media: Option<SfuMedia>,
    pub active_count: Option<Int>,
}

impl SfuSignalMessage {
    pub(crate) fn decode(value: &Value) -> Result<Self> {
        let f = object(
            value,
            &[
                "type",
                "revision",
                "publicationGeneration",
                "connectionId",
                "kind",
                "description",
                "candidate",
                "media",
                "activeCount",
            ],
        )?;
        f.require(&[
            "type",
            "revision",
            "publicationGeneration",
            "connectionId",
            "kind",
        ])?;
        f.optional(&["description", "candidate", "media", "activeCount"])?;
        let kind = String::from_wire(f.get("kind").unwrap_or(&Value::Null))?;
        let message = Self {
            revision: f.int("revision")?,
            publication_generation: f.string("publicationGeneration")?,
            connection_id: f.string("connectionId")?,
            kind: SfuSignalKind::parse(&kind).unwrap_or(SfuSignalKind::Subscribe),
            description: f.opt("description")?,
            candidate: f.opt("candidate")?,
            media: f.opt("media")?,
            active_count: f.opt("activeCount")?,
        };
        ensure(
            kind == "layers" || message.active_count.is_none(),
            "unexpected SFU layer demand",
        )?;
        ensure(
            valid_revision(message.revision)
                && valid_opaque_id(&message.publication_generation)
                && valid_opaque_id(&message.connection_id),
            "invalid SFU signal fence",
        )?;
        let (has_description, has_candidate, has_media) = (
            message.description.is_some(),
            message.candidate.is_some(),
            message.media.is_some(),
        );
        match kind.as_str() {
            "layers" => ensure(
                message
                    .active_count
                    .is_some_and(|count| in_range(count, 0, 3))
                    && !has_description
                    && !has_candidate
                    && !has_media,
                "invalid SFU layer demand",
            )?,
            "media" => ensure(
                has_media && !has_description && !has_candidate,
                "invalid SFU media signal",
            )?,
            "subscribe" => ensure(
                !has_description && !has_candidate && !has_media,
                "invalid SFU subscription signal",
            )?,
            "description" => ensure(
                message.description.as_ref().is_some_and(|description| {
                    !has_candidate && (!has_media || description.kind == SdpType::Offer)
                }),
                "invalid SFU description signal",
            )?,
            "candidate" => ensure(!has_description && !has_media, "invalid SFU ICE signal")?,
            _ => ensure(false, "unknown SFU signal kind")?,
        }
        Ok(message)
    }

    pub(crate) fn encode(&self) -> Value {
        Obj::message("sfu-signal")
            .put("revision", &self.revision)
            .put("publicationGeneration", &self.publication_generation)
            .put("connectionId", &self.connection_id)
            .put("kind", &self.kind)
            .put_some("description", &self.description)
            .put_some("candidate", &self.candidate)
            .put_some("media", &self.media)
            .put_some("activeCount", &self.active_count)
            .build()
    }
}
