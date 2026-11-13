//! Per-output RTP and VP8 identity rewriting, including the sequence-wrap
//! fixes for packets straddling a 16-bit cycle. All counters are extended; the
//! transport passes extended sequence numbers to str0m (`SeqNo`), which owns
//! the SRTP ROC.
//!
//! Offsets use wrapping `u64` arithmetic: a layer switch can map a smaller
//! incoming sequence onto a larger outgoing one.

use std::collections::VecDeque;

use crate::packet::{Unwrapper, Vp8Descriptor, Vp8Rewrite};

/// Older packets beyond half the sequence space cannot be placed reliably.
const MAX_REORDER: u64 = 1 << 15;
/// Bounded history for out-of-order lookups.
const HISTORY: usize = 512;
const SEQ_CYCLE: u64 = 1 << 16;
const TS_CYCLE: u64 = 1 << 32;

/// One translated packet position on this output's timeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Translated {
    pub ext_seq: u64,
    pub ext_timestamp: u64,
}

#[derive(Clone, Debug, Default)]
pub struct RtpMunger {
    started: bool,
    first_in: u64,
    highest_in: u64,
    base_offset: u64,
    seq_offset: u64,
    ts_offset: u64,
    last_out_seq: u64,
    last_out_ts: u64,
    last_marker: bool,
    /// (first incoming sequence, offset) whenever the offset changes in order.
    ranges: VecDeque<(u64, u64)>,
    dropped: VecDeque<u64>,
}

impl RtpMunger {
    pub fn is_started(&self) -> bool {
        self.started
    }

    pub fn last_sequence(&self) -> u64 {
        self.last_out_seq
    }

    pub fn last_timestamp(&self) -> u64 {
        self.last_out_ts
    }

    /// Starts this output's timeline. Its first sequence number keeps the
    /// incoming low 16 bits with ROC 0, so it is always below 65536; the first
    /// timestamp keeps the incoming low 32 bits.
    pub fn start(&mut self, ext_seq: u64, ext_timestamp: u64) {
        let out_seq = ext_seq % SEQ_CYCLE;
        let out_ts = ext_timestamp % TS_CYCLE;
        self.rebase(ext_seq, ext_timestamp, out_seq, out_ts);
        // The first packet is about to be translated as the new highest.
        self.highest_in = ext_seq.wrapping_sub(1);
        self.last_out_seq = out_seq.wrapping_sub(1);
    }

    /// Continues the output across an input discontinuity (layer switch,
    /// resync or source restart): the packet becomes `last + 1` and its
    /// timestamp `out_timestamp`, which the caller keeps after the last one.
    pub fn switch(&mut self, ext_seq: u64, ext_timestamp: u64, out_timestamp: u64) {
        if !self.started {
            self.start(ext_seq, ext_timestamp);
            return;
        }
        let out_seq = self.last_out_seq.wrapping_add(1);
        self.rebase(ext_seq, ext_timestamp, out_seq, out_timestamp);
        self.highest_in = ext_seq.wrapping_sub(1);
    }

    fn rebase(&mut self, ext_seq: u64, ext_timestamp: u64, out_seq: u64, out_ts: u64) {
        self.started = true;
        self.first_in = ext_seq;
        self.seq_offset = ext_seq.wrapping_sub(out_seq);
        self.base_offset = self.seq_offset;
        self.ts_offset = ext_timestamp.wrapping_sub(out_ts);
        self.ranges.clear();
        self.dropped.clear();
        self.last_marker = false;
    }

    /// Translates a forwarded packet. `None` drops duplicates, packets before
    /// this timeline's start, dropped positions and packets too old to place.
    pub fn translate(
        &mut self,
        ext_seq: u64,
        ext_timestamp: u64,
        marker: bool,
    ) -> Option<Translated> {
        if !self.started {
            return None;
        }
        let ext_timestamp_out = ext_timestamp.wrapping_sub(self.ts_offset);
        if ext_seq > self.highest_in || ext_seq == self.highest_in.wrapping_add(1) {
            self.highest_in = ext_seq;
            self.last_out_seq = ext_seq.wrapping_sub(self.seq_offset);
            self.last_out_ts = ext_timestamp_out;
            self.last_marker = marker;
            return Some(Translated {
                ext_seq: self.last_out_seq,
                ext_timestamp: ext_timestamp_out,
            });
        }
        if ext_seq == self.highest_in
            || ext_seq < self.first_in
            || self.highest_in - ext_seq >= MAX_REORDER
            || self.dropped.contains(&ext_seq)
        {
            return None;
        }
        let offset = self
            .ranges
            .iter()
            .rev()
            .find(|(start, _)| *start <= ext_seq)
            .map_or(self.base_offset, |(_, offset)| *offset);
        Some(Translated {
            ext_seq: ext_seq.wrapping_sub(offset),
            ext_timestamp: ext_timestamp_out,
        })
    }

    /// The forwarder dropped an in-order packet (padding or a filtered
    /// temporal layer); later packets close the gap it would leave.
    pub fn drop_packet(&mut self, ext_seq: u64) {
        if !self.started || ext_seq <= self.highest_in && ext_seq != self.highest_in.wrapping_add(1)
        {
            return;
        }
        self.highest_in = ext_seq;
        self.shift(1);
        push_bounded(&mut self.dropped, ext_seq);
    }

    fn shift(&mut self, by: u64) {
        self.seq_offset = self.seq_offset.wrapping_add(by);
        if self.ranges.len() == HISTORY
            && let Some((_, offset)) = self.ranges.pop_front()
        {
            self.base_offset = offset;
        }
        self.ranges
            .push_back((self.highest_in.wrapping_add(1), self.seq_offset));
    }

    /// Reserves `count` padding positions after the last packet, at its
    /// timestamp. Without `force`, padding only follows a complete frame.
    pub fn padding(&mut self, count: u64, force: bool) -> Vec<Translated> {
        if !self.started || count == 0 || !force && !self.last_marker {
            return Vec::new();
        }
        let packets = (1..=count)
            .map(|index| Translated {
                ext_seq: self.last_out_seq.wrapping_add(index),
                ext_timestamp: self.last_out_ts,
            })
            .collect();
        self.last_out_seq = self.last_out_seq.wrapping_add(count);
        // Subsequent media moves up by the reserved positions.
        self.shift(count.wrapping_neg());
        packets
    }
}

fn push_bounded<T>(queue: &mut VecDeque<T>, value: T) {
    if queue.len() == HISTORY {
        queue.pop_front();
    }
    queue.push_back(value);
}

/// VP8 PictureID/TL0PICIDX/KEYIDX continuity across switches and temporal
/// filtering. Out-of-order pictures reuse the offset of their own picture.
#[derive(Clone, Debug)]
pub struct Vp8Munger {
    started: bool,
    picture_ids: Unwrapper,
    highest_in: u64,
    picture_offset: u64,
    last_out_picture: u64,
    tl0_offset: u8,
    last_out_tl0: u8,
    key_offset: u8,
    last_out_key: u8,
    dropped: VecDeque<u64>,
    recent: VecDeque<(u64, u64)>,
}

fn picture_unwrapper(descriptor: &Vp8Descriptor) -> Unwrapper {
    Unwrapper::bits(if descriptor.picture_id_15bit { 15 } else { 7 })
}

impl Default for Vp8Munger {
    fn default() -> Self {
        Self {
            started: false,
            picture_ids: Unwrapper::bits(15),
            highest_in: 0,
            picture_offset: 0,
            last_out_picture: 0,
            tl0_offset: 0,
            last_out_tl0: 0,
            key_offset: 0,
            last_out_key: 0,
            dropped: VecDeque::new(),
            recent: VecDeque::new(),
        }
    }
}

impl Vp8Munger {
    /// Starts with the incoming identities unchanged.
    pub fn start(&mut self, descriptor: &Vp8Descriptor) {
        *self = Self::default();
        self.started = true;
        self.picture_ids = picture_unwrapper(descriptor);
        if let Some(id) = descriptor.picture_id {
            let extended = self.picture_ids.extend(u64::from(id));
            self.picture_offset = extended - u64::from(id);
            self.highest_in = extended.wrapping_sub(1);
            self.last_out_picture = u64::from(id).wrapping_sub(1);
        }
        self.last_out_tl0 = descriptor.tl0_pic_idx.unwrap_or(0).wrapping_sub(1);
        self.last_out_key = descriptor.key_idx.unwrap_or(0).wrapping_sub(1);
    }

    /// Continues each identity at `last + 1` from the new input's first frame.
    pub fn switch(&mut self, descriptor: &Vp8Descriptor) {
        if !self.started {
            self.start(descriptor);
            return;
        }
        self.picture_ids = picture_unwrapper(descriptor);
        self.dropped.clear();
        self.recent.clear();
        if let Some(id) = descriptor.picture_id {
            let extended = self.picture_ids.extend(u64::from(id));
            self.picture_offset = extended.wrapping_sub(self.last_out_picture.wrapping_add(1));
            self.highest_in = extended.wrapping_sub(1);
        }
        if let Some(tl0) = descriptor.tl0_pic_idx {
            self.tl0_offset = tl0.wrapping_sub(self.last_out_tl0.wrapping_add(1));
        }
        if let Some(key) = descriptor.key_idx {
            self.key_offset = key.wrapping_sub(self.last_out_key.wrapping_add(1));
        }
    }

    /// Rewrites one forwarded packet, or records a dropped picture so later
    /// PictureIDs stay contiguous. `None` means drop.
    pub fn rewrite(&mut self, descriptor: &Vp8Descriptor, drop: bool) -> Option<Vp8Rewrite> {
        if !self.started {
            return None;
        }
        let mut rewrite = Vp8Rewrite::default();
        let mut in_order = true;
        if let Some(id) = descriptor.picture_id {
            let extended = self.picture_ids.extend(u64::from(id));
            let offset = if extended > self.highest_in {
                self.highest_in = extended;
                if drop {
                    self.picture_offset = self.picture_offset.wrapping_add(1);
                    push_bounded(&mut self.dropped, extended);
                    return None;
                }
                push_bounded(&mut self.recent, (extended, self.picture_offset));
                self.picture_offset
            } else {
                in_order = extended == self.highest_in;
                if drop || self.dropped.contains(&extended) {
                    return None;
                }
                self.recent
                    .iter()
                    .rev()
                    .find(|(start, _)| *start <= extended)?
                    .1
            };
            let out = extended.wrapping_sub(offset);
            if in_order {
                self.last_out_picture = out;
            }
            let mask = if descriptor.picture_id_15bit {
                0x7fff
            } else {
                0x7f
            };
            rewrite.picture_id = Some((out & mask) as u16);
        } else if drop {
            return None;
        }
        if let Some(tl0) = descriptor.tl0_pic_idx {
            let out = tl0.wrapping_sub(self.tl0_offset);
            if in_order {
                self.last_out_tl0 = out;
            }
            rewrite.tl0_pic_idx = Some(out);
        }
        if let Some(key) = descriptor.key_idx {
            let out = key.wrapping_sub(self.key_offset) & 0x1f;
            if in_order {
                self.last_out_key = out;
            }
            rewrite.key_idx = Some(out);
        }
        Some(rewrite)
    }
}

#[cfg(test)]
mod tests;
