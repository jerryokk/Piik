//! H.264 profile recognition and RFC 6184 packetization.

use crate::Payloader;

/// Recognizes the equivalent Constrained Baseline coding-tool subsets in
/// RFC 6184 Table 5 and returns the `level_idc` with whether the profile
/// matched. Callers own their permitted level range.
pub fn h264_constrained_baseline_level(value: &str) -> (u8, bool) {
    if value.len() != 6 {
        return (0, false);
    }
    let Ok(profile) = hex::decode(value) else {
        return (0, false);
    };
    let constrained_baseline = (profile[0] == 0x42 && profile[1] & 0x4f == 0x40)
        || (profile[0] == 0x4d && profile[1] & 0x8f == 0x80)
        || (profile[0] == 0x58 && profile[1] & 0xcf == 0xc0);
    (profile[2], constrained_baseline)
}

const NALU_TYPE_MASK: u8 = 0x1f;
const NALU_REF_IDC_MASK: u8 = 0x60;
const AUD_NALU_TYPE: u8 = 9;
const FILLER_NALU_TYPE: u8 = 12;
const SPS_NALU_TYPE: u8 = 7;
const PPS_NALU_TYPE: u8 = 8;
const FUA_NALU_TYPE: u8 = 28;
const FUA_HEADER_SIZE: usize = 2;
const STAPA_HEADER: u8 = 0x78;

/// Annex-B H.264 payloader (packetization-mode 1). SPS and PPS are held and
/// aggregated with the next NAL unit as one STAP-A; larger units use FU-A.
#[derive(Debug, Default, Clone)]
pub struct H264Payloader {
    sps: Option<Vec<u8>>,
    pps: Option<Vec<u8>>,
}

impl H264Payloader {
    pub fn new() -> Self {
        Self::default()
    }

    fn emit(&mut self, nalu: &[u8], mtu: usize, payloads: &mut Vec<Vec<u8>>) {
        let Some(&first) = nalu.first() else {
            return;
        };
        let nalu_type = first & NALU_TYPE_MASK;
        let ref_idc = first & NALU_REF_IDC_MASK;
        match nalu_type {
            AUD_NALU_TYPE | FILLER_NALU_TYPE => return,
            SPS_NALU_TYPE => {
                self.sps = Some(nalu.to_vec());
                return;
            }
            PPS_NALU_TYPE => {
                self.pps = Some(nalu.to_vec());
                return;
            }
            _ => {}
        }
        if let (Some(sps), Some(pps)) = (self.sps.take(), self.pps.take()) {
            // Parameter sets above 64 KiB cannot be aggregated and are dropped,
            // as is an aggregate that does not fit the MTU.
            if let (Ok(sps_len), Ok(pps_len)) = (u16::try_from(sps.len()), u16::try_from(pps.len()))
            {
                let mut stap = Vec::with_capacity(5 + sps.len() + pps.len());
                stap.push(STAPA_HEADER);
                stap.extend_from_slice(&sps_len.to_be_bytes());
                stap.extend_from_slice(&sps);
                stap.extend_from_slice(&pps_len.to_be_bytes());
                stap.extend_from_slice(&pps);
                if stap.len() <= mtu {
                    payloads.push(stap);
                }
            }
        }
        if nalu.len() <= mtu {
            payloads.push(nalu.to_vec());
            return;
        }
        let Some(max_fragment) = mtu.checked_sub(FUA_HEADER_SIZE).filter(|size| *size > 0) else {
            return;
        };
        // The FU header carries the original type; the NAL header octet is not repeated.
        let data = &nalu[1..];
        let count = data.len().div_ceil(max_fragment);
        for (index, fragment) in data.chunks(max_fragment).enumerate() {
            let mut header = nalu_type;
            if index == 0 {
                header |= 0x80;
            } else if index + 1 == count {
                header |= 0x40;
            }
            let mut out = Vec::with_capacity(FUA_HEADER_SIZE + fragment.len());
            out.push(FUA_NALU_TYPE | ref_idc);
            out.push(header);
            out.extend_from_slice(fragment);
            payloads.push(out);
        }
    }
}

/// Returns the next Annex-B start code as (offset, length).
fn next_start_code(data: &[u8], start: usize) -> Option<(usize, usize)> {
    let mut zeros = 0;
    for (index, &byte) in data.get(start..)?.iter().enumerate() {
        if byte == 0 {
            zeros += 1;
            continue;
        }
        if byte == 1 && zeros >= 2 {
            return Some((start + index - zeros, zeros + 1));
        }
        zeros = 0;
    }
    None
}

impl Payloader for H264Payloader {
    fn payload(&mut self, mtu: usize, data: &[u8]) -> Vec<Vec<u8>> {
        let mut payloads = Vec::new();
        if data.is_empty() || mtu == 0 {
            return payloads;
        }
        let Some(mut code) = next_start_code(data, 0) else {
            self.emit(data, mtu, &mut payloads);
            return payloads;
        };
        loop {
            let start = code.0 + code.1;
            match next_start_code(data, start) {
                Some(next) => {
                    self.emit(&data[start..next.0], mtu, &mut payloads);
                    code = next;
                }
                None => {
                    self.emit(&data[start..], mtu, &mut payloads);
                    return payloads;
                }
            }
        }
    }
}

/// Bounds capture and receive to the same coding tools. Automatic encoders may
/// select a lower level for a smaller output.
pub fn supported_profile_level_id(value: &str) -> bool {
    let (level, valid) = h264_constrained_baseline_level(value);
    valid
        && matches!(
            level,
            10 | 11 | 12 | 13 | 20 | 21 | 22 | 30 | 31 | 32 | 40 | 41 | 42 | 50 | 51 | 52
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constrained_baseline_recognizes_equivalent_profiles() {
        for (value, level, valid) in [
            ("42e01f", 0x1f, true),
            ("42C034", 0x34, true),
            ("4d8028", 0x28, true),
            ("58c01e", 0x1e, true),
            ("42001f", 0x1f, false),
            ("640028", 0x28, false),
            ("42e01", 0, false),
            ("zze01f", 0, false),
        ] {
            assert_eq!(
                h264_constrained_baseline_level(value),
                (level, valid),
                "{value}"
            );
        }
    }

    #[test]
    fn aggregates_parameter_sets_and_fragments_large_units() {
        let mut payloader = H264Payloader::new();
        let mut access_unit = vec![0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x68, 3, 0, 0, 0, 1, 0x65];
        access_unit.extend(std::iter::repeat_n(0xaa, 20));
        let payloads = payloader.payload(10, &access_unit);
        assert_eq!(payloads[0], [STAPA_HEADER, 0, 3, 0x67, 1, 2, 0, 2, 0x68, 3]);
        assert_eq!(payloads[1][..2], [FUA_NALU_TYPE | 0x60, 0x85]);
        let last = payloads.last().map(|payload| payload[1]);
        assert_eq!(last, Some(0x45));
        let rebuilt: Vec<u8> = payloads[1..].iter().flat_map(|p| p[2..].to_vec()).collect();
        assert_eq!(rebuilt, vec![0xaa; 20]);
        assert!(payloader.payload(10, &[0, 0, 1, 0x09, 0x10]).is_empty());
    }
}
