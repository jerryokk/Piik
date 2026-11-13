//! Port of LiveKit `server-sdk-go/v2/pkg/samplebuilder` (Apache-2.0,
//! Copyright 2023 LiveKit, Inc., derived from github.com/jech/samplebuilder):
//! a circular RTP reorder buffer. Drop notifications are counted instead of
//! invoking a callback; the caller drains them with [`SampleBuilder::take_drops`].
//!
//! H.264 uses one-packet partitions (every packet is head and tail) so the
//! caller owns AU boundaries; VP8 uses the descriptor S bit and RTP marker.

use super::{Codec, RtpPacket, vp8_payload};

#[derive(Debug, Clone)]
struct Slot {
    start: bool,
    end: bool,
    packet: RtpPacket,
}

#[derive(Debug)]
pub(super) struct SampleBuilder {
    // head == tail is empty, so there is always one free slot.
    packets: Vec<Option<Slot>>,
    head: u16,
    tail: u16,
    max_late: u16,
    codec: Codec,
    // The seqno of the last popped or dropped packet, if any.
    last_seqno: Option<u16>,
    drops: usize,
}

impl SampleBuilder {
    /// `max_late` is the delay, in sequence numbers, before an incomplete
    /// frame is dropped. The buffer holds twice as many packets.
    pub(super) fn new(max_late: u16, codec: Codec) -> Self {
        let max_late = max_late.clamp(2, 0x7fff);
        Self {
            packets: vec![None; usize::from(2 * max_late + 1)],
            head: 0,
            tail: 0,
            max_late,
            codec,
            last_seqno: None,
            drops: 0,
        }
    }

    pub(super) fn take_drops(&mut self) -> usize {
        std::mem::take(&mut self.drops)
    }

    fn size(&self) -> u16 {
        self.packets.len() as u16
    }

    fn length(&self) -> u16 {
        if self.tail <= self.head {
            self.head - self.tail
        } else {
            self.head + self.size() - self.tail
        }
    }

    fn cap(&self) -> u16 {
        self.size() - 1
    }

    fn inc(&self, n: u16) -> u16 {
        if n < self.size() - 1 { n + 1 } else { 0 }
    }

    fn dec(&self, n: u16) -> u16 {
        if n > 0 { n - 1 } else { self.size() - 1 }
    }

    fn slot(&self, index: u16) -> Option<&Slot> {
        self.packets[usize::from(index)].as_ref()
    }

    fn slot_mut(&mut self, index: u16) -> Option<&mut Slot> {
        self.packets[usize::from(index)].as_mut()
    }

    fn occupied(&self, index: u16) -> &Slot {
        self.slot(index)
            .expect("sample builder invariant: occupied slot")
    }

    fn is_start(&self, packet: &RtpPacket) -> bool {
        packet.payload.is_empty()
            || match self.codec {
                Codec::H264 => true,
                Codec::Vp8 => packet.payload[0] & 0x10 != 0,
            }
    }

    fn is_end(&self, packet: &RtpPacket) -> bool {
        packet.payload.is_empty()
            || match self.codec {
                Codec::H264 => true,
                Codec::Vp8 => packet.marker,
            }
    }

    /// Releases the packet at tail and returns it.
    fn release(&mut self) -> Option<RtpPacket> {
        if self.head == self.tail {
            return None;
        }
        let slot = self.packets[usize::from(self.tail)].take()?;
        self.last_seqno = Some(slot.packet.sequence_number);
        self.tail = self.inc(self.tail);
        while self.tail != self.head && self.slot(self.tail).is_none() {
            self.tail = self.inc(self.tail);
        }
        if self.tail == self.head {
            self.head = 0;
            self.tail = 0;
        }
        Some(slot.packet)
    }

    fn release_all(&mut self) {
        while self.tail != self.head {
            self.release();
        }
    }

    /// Drops the oldest frame, even if incomplete.
    fn drop_frame(&mut self) -> bool {
        if self.tail == self.head {
            return false;
        }
        self.drops += 1;
        let timestamp = self.occupied(self.tail).packet.timestamp;
        self.release();
        while self.tail != self.head {
            let slot = self.occupied(self.tail);
            if slot.start || slot.packet.timestamp != timestamp {
                break;
            }
            self.release();
        }
        true
    }

    pub(super) fn push(&mut self, packet: RtpPacket) {
        if let Some(last_seqno) = self.last_seqno {
            if last_seqno.wrapping_sub(packet.sequence_number) & 0x8000 == 0 {
                // Late packet.
                if last_seqno.wrapping_sub(packet.sequence_number) > self.max_late {
                    self.last_seqno = None;
                } else {
                    return;
                }
            } else {
                let mut last = packet.sequence_number.wrapping_sub(self.max_late);
                if last.wrapping_sub(last_seqno) & 0x8000 == 0 {
                    if self.head != self.tail {
                        let seqno = self
                            .occupied(self.tail)
                            .packet
                            .sequence_number
                            .wrapping_sub(1);
                        if last.wrapping_sub(seqno) & 0x8000 == 0 {
                            last = seqno;
                        }
                    }
                    self.last_seqno = Some(last);
                }
            }
        }

        if self.head == self.tail {
            self.packets[0] = Some(Slot {
                start: self.is_start(&packet),
                end: self.is_end(&packet),
                packet,
            });
            self.tail = 0;
            self.head = 1;
            return;
        }

        let seqno = packet.sequence_number;
        let timestamp = packet.timestamp;
        let last = self.dec(self.head);
        let last_seqno = self.occupied(last).packet.sequence_number;
        if seqno == last_seqno.wrapping_add(1) {
            // Sequential.
            if self.tail == self.inc(self.head) {
                // Full: the oldest frame must go.
                self.drop_frame();
            }
            let start = if self.tail != self.head {
                let is_start = self.is_start(&packet);
                let previous = self.slot_mut(last).expect("last packet is present");
                let start = previous.end || previous.packet.timestamp != timestamp || is_start;
                if start {
                    previous.end = true;
                }
                start
            } else {
                // The drop may have emptied the buffer.
                self.is_start(&packet)
            };
            let end = self.is_end(&packet);
            self.packets[usize::from(self.head)] = Some(Slot { start, end, packet });
            self.head = self.inc(self.head);
            return;
        }

        if seqno.wrapping_sub(last_seqno) & 0x8000 == 0 {
            // A packet in the future.
            let count = seqno.wrapping_sub(last_seqno).wrapping_sub(1);
            if count >= self.cap() {
                self.release_all();
                self.push(packet);
                return;
            }
            while self.length() + count + 1 >= self.cap() {
                if !self.drop_frame() {
                    return;
                }
            }
            let index = ((u32::from(self.head) + u32::from(count)) % u32::from(self.size())) as u16;
            self.packets[usize::from(index)] = Some(Slot {
                start: self.is_start(&packet),
                end: self.is_end(&packet),
                packet,
            });
            self.head = self.inc(index);
            return;
        }

        // A packet in the past.
        let count = last_seqno.wrapping_sub(seqno).wrapping_add(1);
        if count >= self.cap() {
            return;
        }
        let index = if self.head >= count {
            self.head - count
        } else {
            self.head + self.size() - count
        };
        // Extend if necessary.
        if self.tail < self.head {
            if index < self.tail || index > self.head {
                self.tail = index;
            }
        } else if index < self.tail && index > self.head {
            self.tail = index;
        }
        if self.slot(index).is_some() {
            // Duplicate.
            return;
        }

        let mut start = self.is_start(&packet);
        if index != self.tail {
            let prev = self.dec(index);
            if let Some(previous) = self.slot_mut(prev) {
                if previous.packet.timestamp != timestamp {
                    start = true;
                }
                if !start {
                    start = previous.end;
                } else {
                    previous.end = true;
                }
            }
        }
        let mut end = self.is_end(&packet);
        let next = self.inc(index);
        if let Some(following) = self.slot_mut(next) {
            if following.packet.timestamp != timestamp {
                end = true;
            }
            if !end {
                end = following.start;
            } else {
                following.start = true;
            }
        }
        self.packets[usize::from(index)] = Some(Slot { start, end, packet });
    }

    /// Pops the packets of the oldest complete frame. `force` drops anything
    /// blocking it; after a forced `None` the builder is empty.
    pub(super) fn pop_packets(&mut self, force: bool) -> Option<Vec<RtpPacket>> {
        loop {
            if self.tail == self.head {
                return None;
            }
            if !self.occupied(self.tail).start {
                let diff = self
                    .occupied(self.dec(self.head))
                    .packet
                    .sequence_number
                    .wrapping_sub(self.occupied(self.tail).packet.sequence_number);
                if force || diff > self.max_late {
                    self.drop_frame();
                    continue;
                }
                return None;
            }
            let seqno = self.occupied(self.tail).packet.sequence_number;
            if !force
                && self
                    .last_seqno
                    .is_some_and(|last| last.wrapping_add(1) != seqno)
            {
                // Loss before tail.
                return None;
            }
            let mut last = self.tail;
            let mut gap = false;
            while last != self.head && !self.slot(last).is_some_and(|slot| slot.end) {
                if self.slot(last).is_none() {
                    gap = true;
                    break;
                }
                last = self.inc(last);
            }
            if gap {
                if force {
                    self.drop_frame();
                    continue;
                }
                return None;
            }
            if last == self.head {
                return None;
            }
            let count = if last < self.tail {
                self.size() + last - self.tail + 1
            } else {
                last - self.tail + 1
            };
            let mut packets = Vec::with_capacity(usize::from(count));
            for _ in 0..count {
                packets.extend(self.release());
            }
            return Some(packets);
        }
    }

    /// Pops and depacketizes the oldest complete VP8 frame with its RTP
    /// timestamp. A frame whose descriptor is invalid is consumed and lost.
    pub(super) fn pop_sample(&mut self, force: bool) -> Option<(Vec<u8>, u32)> {
        let packets = self.pop_packets(force)?;
        let timestamp = packets.first()?.timestamp;
        let mut data = Vec::new();
        for packet in &packets {
            data.extend_from_slice(vp8_payload(&packet.payload)?);
        }
        Some((data, timestamp))
    }
}
