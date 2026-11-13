//! Per-endpoint authenticated input encryption.
//!
//! Each authenticated room endpoint gets a fresh key through room signaling,
//! never through a media relay. Random nonces survive native-session
//! replacement; the authenticated direction and sequence reject reflected or
//! delayed packets.

use aws_lc_rs::aead::{AES_256_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

use crate::text::MAX_INPUT_BYTES;

const SEQUENCE_LEN: usize = 8;
pub(crate) const HEADER_LEN: usize = SEQUENCE_LEN + NONCE_LEN;
pub(crate) const TAG_LEN: usize = 16;

/// Viewer to Host.
pub(crate) const FROM_VIEWER: u8 = 0;
/// Host to Viewer.
pub(crate) const FROM_HOST: u8 = 1;

pub(crate) struct InputCipher {
    key: LessSafeKey,
    pub sent: u64,
    pub received: u64,
}

impl InputCipher {
    /// `key` is 32 bytes of hex, as validated by the room signaling contract.
    pub fn new(key: &str) -> Option<Self> {
        let bytes = hex::decode(key).ok().filter(|bytes| bytes.len() == 32)?;
        let key = UnboundKey::new(&AES_256_GCM, &bytes).ok()?;
        Some(Self {
            key: LessSafeKey::new(key),
            sent: 0,
            received: 0,
        })
    }

    /// Opens one Viewer packet. Its sequence must exceed every accepted one.
    pub fn open(&mut self, data: &str, epoch: &str) -> Option<Vec<u8>> {
        let mut bytes = STANDARD.decode(data).ok()?;
        if bytes.len() < HEADER_LEN + TAG_LEN
            || bytes.len() > MAX_INPUT_BYTES + HEADER_LEN + TAG_LEN
        {
            return None;
        }
        let sequence = u64::from_be_bytes(bytes[..SEQUENCE_LEN].try_into().ok()?);
        if sequence <= self.received {
            return None;
        }
        let nonce = Nonce::try_assume_unique_for_key(&bytes[SEQUENCE_LEN..HEADER_LEN]).ok()?;
        let aad = aad(FROM_VIEWER, &bytes[..SEQUENCE_LEN], epoch);
        let plain = self
            .key
            .open_in_place(nonce, Aad::from(aad), &mut bytes[HEADER_LEN..])
            .ok()?
            .to_vec();
        self.received = sequence;
        Some(plain)
    }

    /// Seals one Host reply.
    pub fn seal(&mut self, data: &[u8], epoch: &str) -> String {
        self.sent += 1;
        self.seal_with(FROM_HOST, self.sent, data, epoch)
    }

    /// Seals with an explicit direction and sequence; Viewer-side test packets
    /// use the same construction as the Browser.
    pub fn seal_with(&self, direction: u8, sequence: u64, data: &[u8], epoch: &str) -> String {
        let mut header = [0u8; HEADER_LEN];
        header[..SEQUENCE_LEN].copy_from_slice(&sequence.to_be_bytes());
        aws_lc_rs::rand::fill(&mut header[SEQUENCE_LEN..]).expect("system random source");
        let nonce =
            Nonce::try_assume_unique_for_key(&header[SEQUENCE_LEN..]).expect("nonce length");
        let mut body = data.to_vec();
        self.key
            .seal_in_place_append_tag(
                nonce,
                Aad::from(aad(direction, &header[..SEQUENCE_LEN], epoch)),
                &mut body,
            )
            .expect("AES-GCM seals bounded input");
        let mut packet = header.to_vec();
        packet.extend_from_slice(&body);
        STANDARD.encode(packet)
    }

    /// Opens a packet with an explicit direction, without sequence state.
    #[cfg(test)]
    pub fn open_with(&self, direction: u8, data: &str, epoch: &str) -> Option<Vec<u8>> {
        let mut bytes = STANDARD.decode(data).ok()?;
        if bytes.len() < HEADER_LEN + TAG_LEN {
            return None;
        }
        let nonce = Nonce::try_assume_unique_for_key(&bytes[SEQUENCE_LEN..HEADER_LEN]).ok()?;
        let aad = aad(direction, &bytes[..SEQUENCE_LEN], epoch);
        let body = &mut bytes[HEADER_LEN..];
        Some(
            self.key
                .open_in_place(nonce, Aad::from(aad), body)
                .ok()?
                .to_vec(),
        )
    }
}

fn aad(direction: u8, sequence: &[u8], epoch: &str) -> Vec<u8> {
    let mut aad = Vec::with_capacity(1 + sequence.len() + epoch.len());
    aad.push(direction);
    aad.extend_from_slice(sequence);
    aad.extend_from_slice(epoch.as_bytes());
    aad
}

/// Fills `bytes` from the system random source.
pub(crate) fn random(bytes: &mut [u8]) {
    aws_lc_rs::rand::fill(bytes).expect("system random source");
}

/// A random lowercase-hex identifier of `len` bytes.
pub(crate) fn random_hex(len: usize) -> String {
    let mut bytes = vec![0u8; len];
    random(&mut bytes);
    hex::encode(bytes)
}
