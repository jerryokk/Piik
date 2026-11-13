//! Encoded frame and RTP ingress: writing frames into the original or a
//! derived media, layer failure and received-RTP correlation.

use super::*;

impl App {
    pub(crate) fn write_video(
        &mut self,
        conns: &mut Connections,
        key: u64,
        layer: usize,
        frame: Frame,
        now: Instant,
    ) -> Result<()> {
        let state = self.source(key)?;
        if !state.output_bitrates.is_empty()
            && state
                .output_bitrates
                .get(layer)
                .is_none_or(|bitrate| *bitrate == 0)
        {
            return Err(Error::Invalid("native output layer is unavailable"));
        }
        if frame.data.is_empty()
            || frame.duration.is_zero()
            || !state.has_input
            || frame.pts > state.input_pts
        {
            return Err(Error::Invalid(
                "encoded output has no matching source input",
            ));
        }
        if state.format_count != 2 {
            return self.write_media(conns, key, ORIGINAL, layer, &frame, now);
        }
        if layer == 1 {
            self.write_media(conns, key, ORIGINAL, 1, &frame, now)?;
            let state = self.source(key)?;
            let targets: Vec<MediaId> = state
                .groups
                .groups()
                .filter(|group| group.slot != 0 && group.active && frame.pts >= group.start)
                .map(|group| group.media)
                .collect();
            for media in targets {
                self.write_media(conns, key, media, 1, &frame, now)?;
            }
            if frame.recovery {
                self.install_group(conns, key, None, true, now);
            }
            return Ok(());
        }
        let Some(group) = state.groups.group(layer).cloned() else {
            if layer == 0 {
                return self.write_media(conns, key, ORIGINAL, 0, &frame, now);
            }
            return Ok(());
        };
        if !group.active || frame.pts < group.start {
            return Ok(());
        }
        self.write_media(conns, key, group.media, 0, &frame, now)?;
        if frame.recovery {
            self.install_group(conns, key, Some(group.slot), false, now);
        }
        Ok(())
    }

    /// Packetizes one access unit into `media` and forwards it.
    fn write_media(
        &mut self,
        conns: &mut Connections,
        key: u64,
        media: MediaId,
        layer: usize,
        frame: &Frame,
        now: Instant,
    ) -> Result<()> {
        let state = self.source(key)?;
        let relay = state.relay.is_some();
        let original = relay.then(|| state.media.source().clone());
        let target = if media == ORIGINAL {
            &mut state.media
        } else {
            state.group_media.get_mut(&media).ok_or(Error::Closed)?
        };
        let written = target.write_frame(layer, frame, original.as_ref())?;
        if frame.recovery {
            state.keyframe_received(media, layer);
        }
        self.forward_media(conns, key, media, &written.packets, written.changes, now);
        Ok(())
    }

    /// Retires the failed output's consumers and
    /// every publication declaring it. Healthy siblings stay.
    pub(crate) fn mark_layer_unavailable(
        &mut self,
        conns: &mut Connections,
        key: u64,
        layer: usize,
        now: Instant,
    ) -> Result<()> {
        let state = self.source(key)?;
        if layer >= state.output_bitrates.len() {
            return Err(Error::Invalid("native output layer is unavailable"));
        }
        state.output_bitrates[layer] = 0;
        if layer == 0 {
            state
                .media
                .source_mut()
                .set_lowest_layer_rate_controlled(false);
        }
        let state = &self.sources[&key];
        let group = state.groups.group(layer).cloned();
        let format_count = state.format_count;
        let mut publications = Vec::new();
        for publication_key in &state.publications {
            let Some(publication) = self.publications.get(publication_key) else {
                continue;
            };
            let member = state
                .groups
                .membership(&Consumer::Publication(*publication_key));
            let affected = layer == 1
                || group.is_none() && layer < format_count
                || group.as_ref().is_some_and(|group| {
                    publication.media == group.media || member == Some(group.slot)
                });
            if affected {
                publications.push(*publication_key);
            }
        }
        let mut affected = Vec::new();
        for edge_key in state.edges.keys() {
            let Some(edge) = self.edges.get(edge_key) else {
                continue;
            };
            let Some(source) = state.source_of(edge.media) else {
                continue;
            };
            let output = edge.output_state(source);
            let (_, observed) = edge.target_bitrate();
            let mut selected = layer as i32;
            if let Some(group) = &group {
                if state.groups.membership(&Consumer::Edge(*edge_key)) == Some(group.slot) {
                    affected.push(*edge_key);
                    continue;
                }
                if edge.media != group.media {
                    continue;
                }
                selected = 0;
            }
            if output.target == selected
                || output.current == selected
                || observed && output.paused && output.prepare == selected
                || output.target < 0 && output.current < 0 && edge.max_spatial_layer() == selected
            {
                affected.push(*edge_key);
            }
        }
        let state = self.source(key)?;
        if let Some(group) = &group {
            if let Some(entry) = state.groups.group_mut(group.slot) {
                entry.active = false;
            }
            if let Some(media) = state.group_media.get_mut(&group.media) {
                media.source_mut().set_lowest_layer_rate_controlled(false);
            }
        }
        for edge_key in affected {
            self.close_edge(conns, edge_key, now);
        }
        for publication_key in publications {
            self.close_publication(conns, publication_key, now);
        }
        Ok(())
    }

    /// Received raw video RTP. Optional lower derivation is fed without
    /// waiting for its decoder.
    pub(crate) fn write_received_rtp(
        &mut self,
        conns: &mut Connections,
        key: u64,
        packet: &crate::receiver::ReceivedRtp,
        now: Instant,
    ) -> Result<()> {
        let state = self.source(key)?;
        let layer = state.format_count - 1;
        let mut recovery = false;
        if state.relay.is_some()
            && let Some((width, height)) =
                crate::relay_input::payload_size(state.codec, &packet.payload)
        {
            recovery = true;
            // Actual input dimensions do not replace the Host's profile.
            state
                .media
                .source_mut()
                .update_dimensions(layer, width, height)?;
            for media in state.group_media.values_mut() {
                media.source_mut().update_dimensions(layer, width, height)?;
            }
        }
        let kind = codec::kind(state.codec);
        let info = InboundPacket::new(
            kind,
            layer as i32,
            packet.ext_seq,
            packet.ext_timestamp,
            &packet.payload,
            packet.arrival,
        )
        .marker(packet.marker)
        .ssrc(layer as u32 + 1)
        .payload_type(codec::video_pt(state.codec))
        .size(packet.size);
        let info = info.reference(state.media.source().reference_time(layer, packet.timestamp));
        let changes = state.media.source_mut().write_rtp(layer, &info)?;
        if info.keyframe {
            state.keyframe_received(ORIGINAL, layer);
        }
        let shared = SourcePacket {
            info,
            payload: packet.payload.clone(),
        };
        let original = state.media.source().clone();
        let mut group_writes = Vec::new();
        for group in state.groups.groups_mut() {
            if group.slot == 0 || !group.active {
                continue;
            }
            let Some(media) = state.group_media.get_mut(&group.media) else {
                continue;
            };
            let group_changes = media.source_mut().write_rtp(layer, &info)?;
            if group.clock_pending {
                media.source_mut().copy_correlation(layer, &original)?;
                group.clock_pending = false;
            }
            group_writes.push((group.media, group_changes));
        }
        self.forward_media(
            conns,
            key,
            ORIGINAL,
            std::slice::from_ref(&shared),
            changes,
            now,
        );
        for (media, group_changes) in group_writes {
            self.forward_media(
                conns,
                key,
                media,
                std::slice::from_ref(&shared),
                group_changes,
                now,
            );
        }
        if recovery {
            self.install_group(conns, key, None, true, now);
        }
        if packet.marker {
            let edges: Vec<u64> = self.source(key)?.edges.keys().copied().collect();
            let unavailable: Vec<u64> = edges
                .into_iter()
                .filter(|edge_key| {
                    self.edges
                        .get(edge_key)
                        .is_some_and(|edge| edge.state.is(PcState::Connected))
                        && self.unavailable_demand(key, *edge_key)
                })
                .collect();
            for edge_key in unavailable {
                self.close_edge(conns, edge_key, now);
            }
        }
        self.relay_push(conns, key, packet, now);
        Ok(())
    }

    /// The received correlation, shared with active groups.
    pub(crate) fn sender_report(&mut self, key: u64, report: SenderReport) -> Result<()> {
        let state = self.source(key)?;
        let layer = state.format_count - 1;
        state
            .media
            .source_mut()
            .sender_report(layer, layer as u32 + 1, report)?;
        let original = state.media.source().clone();
        for group in state.groups.groups() {
            if group.slot == 0 || !group.active {
                continue;
            }
            if let Some(media) = state.group_media.get_mut(&group.media) {
                media.source_mut().copy_correlation(layer, &original)?;
            }
        }
        Ok(())
    }
}
