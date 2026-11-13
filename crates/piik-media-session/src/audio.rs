//! A reusable Opus send slot. One negotiated sender keeps its SSRC, sequence
//! and RTP clock across input sources; detaching fences every write of the
//! previous input.

use std::time::Instant;

use piik_media_forwarding::{
    CodecKind, Forwarded, RtpMunger, Source, VideoLayer, check_audio_receive_limit,
};
use str0m::Rtc;
use str0m::media::{Mid, Pt};

use crate::write::Stream;
use crate::{EgressStats, Error, SourcePacket};

#[derive(Clone, Copy, Debug)]
pub struct AudioSlotOptions {
    pub mid: Mid,
    pub pt: Pt,
    /// Highest Opus rate any attached source may carry; receivers must admit it.
    pub source_ceiling: u32,
}

/// Write authority for one attached input. A token from an earlier
/// attachment is stale and writes nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioAttachment(u64);

#[derive(Debug)]
pub struct AudioSlot {
    stream: Stream,
    ceiling: u32,
    munger: RtpMunger,
    generation: u64,
    attached: Option<String>,
    /// The next packet starts a new input segment on this output's timeline.
    switch: bool,
    last_arrival: Option<Instant>,
    stats: EgressStats,
    closed: bool,
}

impl AudioSlot {
    pub fn new(options: AudioSlotOptions) -> Self {
        Self {
            stream: Stream {
                mid: options.mid,
                rid: None,
                pt: options.pt,
                nackable: false,
            },
            ceiling: options.source_ceiling,
            munger: RtpMunger::default(),
            generation: 0,
            attached: None,
            switch: true,
            last_arrival: None,
            stats: EgressStats::default(),
            closed: false,
        }
    }

    pub fn mid(&self) -> Mid {
        self.stream.mid
    }

    pub fn is_closed(&self) -> bool {
        self.closed
    }

    pub fn egress(&self) -> EgressStats {
        self.stats
    }

    /// The attached source's id, if any.
    pub fn attached(&self) -> Option<&str> {
        self.attached.as_deref()
    }

    /// Admits a receiver's answer: unchanged forwarding must not exceed the
    /// receiver's Opus limit.
    pub fn check_receiver(&self, answer: &str) -> Result<(), Error> {
        Ok(check_audio_receive_limit(answer, self.ceiling)?)
    }

    /// Attaches `source`, replacing any previous input. Re-attaching the same
    /// source keeps its token.
    pub fn replace_source(&mut self, source: &Source) -> Result<AudioAttachment, Error> {
        if self.closed || source.is_closed() {
            return Err(Error::Closed);
        }
        if source.codec().kind != CodecKind::Opus {
            return Err(Error::Incompatible);
        }
        if self.attached.as_deref() == Some(source.id()) {
            return Ok(AudioAttachment(self.generation));
        }
        self.generation += 1;
        self.attached = Some(source.id().to_owned());
        self.switch = true;
        Ok(AudioAttachment(self.generation))
    }

    /// Fences the current input; the negotiated slot stays usable.
    pub fn detach_source(&mut self) {
        if self.attached.take().is_some() {
            self.generation += 1;
        }
    }

    /// The attached source retired: same as detaching it.
    pub fn source_closed(&mut self, attachment: AudioAttachment) {
        if attachment == AudioAttachment(self.generation) {
            self.detach_source();
        }
    }

    /// Forwards one packet of the attached input. Loss and DTX gaps within an
    /// input stay in the output timeline; a new input continues at the next
    /// sequence and after the elapsed arrival time.
    pub fn forward(
        &mut self,
        rtc: &mut Rtc,
        attachment: AudioAttachment,
        source: &Source,
        packet: &SourcePacket,
        now: Instant,
    ) -> bool {
        if self.closed || self.attached.is_none() || attachment != AudioAttachment(self.generation)
        {
            return false;
        }
        let info = &packet.info;
        if self.switch {
            self.switch = false;
            let ticks = self.last_arrival.map_or(1, |last| {
                let elapsed = info.arrival.saturating_duration_since(last);
                ((elapsed.as_secs_f64() * f64::from(CodecKind::Opus.clock_rate())).round() as u64)
                    .max(1)
            });
            let out_timestamp = self.munger.last_timestamp().wrapping_add(ticks);
            self.munger
                .switch(info.ext_seq, info.ext_timestamp, out_timestamp);
        }
        let Some(translated) = self
            .munger
            .translate(info.ext_seq, info.ext_timestamp, info.marker)
        else {
            return false;
        };
        if translated.ext_seq == self.munger.last_sequence() {
            self.last_arrival = Some(info.arrival);
        }
        let forwarded = Forwarded {
            ext_seq: translated.ext_seq,
            ext_timestamp: translated.ext_timestamp,
            marker: info.marker,
            vp8: None,
            layer: VideoLayer::new(0, 0),
            switched: false,
        };
        self.stream
            .write(rtc, source, packet, &forwarded, now, &mut self.stats)
    }

    pub fn close(&mut self) {
        self.detach_source();
        self.closed = true;
    }
}
