//! RFC 7741 VP8 packetization.

use crate::Payloader;

const PICTURE_ID_MAX: u16 = 0x7fff;

/// VP8 payloader. With picture identity enabled, every access unit carries a
/// 7- or 15-bit PictureID that continues across calls.
#[derive(Debug, Default, Clone)]
pub struct Vp8Payloader {
    enable_picture_id: bool,
    picture_id: u16,
}

impl Vp8Payloader {
    pub fn new(enable_picture_id: bool) -> Self {
        Self {
            enable_picture_id,
            picture_id: 0,
        }
    }
}

impl Payloader for Vp8Payloader {
    fn payload(&mut self, mtu: usize, data: &[u8]) -> Vec<Vec<u8>> {
        let header_size = match (self.enable_picture_id, self.picture_id) {
            (false, _) => 1,
            (true, 0..=127) => 3,
            (true, _) => 4,
        };
        let mut payloads = Vec::new();
        let Some(max_fragment) = mtu.checked_sub(header_size).filter(|size| *size > 0) else {
            return payloads;
        };
        if data.is_empty() {
            return payloads;
        }
        for (index, fragment) in data.chunks(max_fragment).enumerate() {
            let mut header = [0u8; 4];
            if index == 0 {
                header[0] = 0x10; // S: start of the frame's first partition.
            }
            if self.enable_picture_id {
                header[0] |= 0x80; // X: extended control bits follow.
                header[1] = 0x80; // I: PictureID follows.
                if header_size == 3 {
                    header[2] = (self.picture_id & 0x7f) as u8;
                } else {
                    header[2] = 0x80 | ((self.picture_id >> 8) & 0x7f) as u8;
                    header[3] = (self.picture_id & 0xff) as u8;
                }
            }
            let mut out = Vec::with_capacity(header_size + fragment.len());
            out.extend_from_slice(&header[..header_size]);
            out.extend_from_slice(fragment);
            payloads.push(out);
        }
        self.picture_id = (self.picture_id + 1) & PICTURE_ID_MAX;
        payloads
    }
}
