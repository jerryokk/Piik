//! Shared output planning without groups and the derived output groups:
//! demands, plans, formats and failed output layers.

use super::*;

/// One connected consumer's demand on a source without output groups.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SharedDemand {
    pub required: usize,
    /// A route edge or publication; a local preview is not.
    pub remote: bool,
    pub budget: i64,
}

/// Frame planning without groups: the active count follows the largest
/// requirement; one shared rendition follows the largest remote demand.
/// Local preview neither holds it at the ceiling nor lowers a healthy child.
pub(crate) fn shared_plan(
    ceilings: &[u32],
    format_count: usize,
    demands: &[SharedDemand],
    codec_budget: impl Fn(i64) -> i64,
) -> OutputPlan {
    let mut plan = OutputPlan {
        active: vec![false; ceilings.len()],
        recovery_layers: Vec::new(),
        bitrates: ceilings.to_vec(),
    };
    let count = demands
        .iter()
        .map(|demand| demand.required)
        .max()
        .unwrap_or(0);
    let remote: Vec<i64> = demands
        .iter()
        .filter(|demand| demand.remote && demand.required > 0)
        .map(|demand| demand.budget)
        .collect();
    let active = count.min(plan.active.len());
    plan.active[..active].fill(true);
    if format_count == 1 && plan.bitrates.len() == 1 && plan.bitrates[0] > 0 && !remote.is_empty() {
        let shared = remote.into_iter().max().unwrap_or(0);
        let budget = codec_budget(shared)
            .min(i64::from(plan.bitrates[0]))
            .max(1000);
        plan.bitrates[0] = budget as u32;
    }
    plan
}

/// Creates group media for [`OutputGroups::plan`].
struct GroupFactory<'a> {
    original: &'a EncodedSource,
    group_media: &'a mut HashMap<MediaId, EncodedSource>,
    next_media: &'a mut u64,
    output_bitrates: &'a [u32],
    relay: Option<usize>,
    anchor: ClockAnchor,
    input: Option<(Duration, SystemTime)>,
}

impl GroupMedia for GroupFactory<'_> {
    type Error = Error;

    fn new_group(&mut self, slot: usize) -> Result<MediaId> {
        let mut media = new_group_media(
            self.original,
            self.output_bitrates[slot],
            self.relay,
            self.anchor,
        )?;
        if let Some((pts, at)) = self.input {
            media.begin_frame(pts, at)?;
        }
        *self.next_media += 1;
        let id = MediaId(*self.next_media);
        self.group_media.insert(id, media);
        Ok(id)
    }

    fn codec_budget(&self, media: MediaId, rtp_budget: i64) -> i64 {
        let media = if media == ORIGINAL {
            Some(self.original)
        } else {
            self.group_media.get(&media)
        };
        media.map_or(rtp_budget.max(0), |media| media.codec_budget(0, rtp_budget))
    }
}

/// A new group's media: the original's metadata with its own lower ceiling.
pub(super) fn new_group_media(
    original: &EncodedSource,
    bitrate: u32,
    relay: Option<usize>,
    anchor: ClockAnchor,
) -> Result<EncodedSource> {
    let source = original.source();
    let mut formats = source.formats().to_vec();
    formats[0].bitrate = bitrate;
    let options = piik_media_forwarding::SourceOptions {
        id: source.id().to_owned(),
        stream_id: source.stream_id().to_owned(),
        codec: source.codec().clone(),
        formats,
        max_packets: source.max_packets(),
    };
    let clock = relay.map_or(InputClock::Local(anchor), |layer| InputClock::Source {
        layer,
    });
    let mut media = EncodedSource::new(options, clock)?;
    media.source_mut().set_lowest_layer_rate_controlled(true);
    Ok(media)
}

impl App {
    pub(crate) fn collect_demands(
        &self,
        key: u64,
        edges: &[u64],
        publications: &[u64],
    ) -> Vec<OutputDemand<Consumer>> {
        let Some(state) = self.sources.get(&key) else {
            return Vec::new();
        };
        let ceiling = state.ceiling();
        let mut demands = Vec::with_capacity(edges.len() + publications.len());
        for edge_key in edges {
            let Some(edge) = self.edges.get(edge_key) else {
                continue;
            };
            let Some(source) = state.source_of(edge.media) else {
                continue;
            };
            let output = edge.output_state(source);
            let (_, observed) = edge.target_bitrate();
            let demand = DemandState {
                connected: edge.state.is(PcState::Connected),
                local: edge.local,
                observed,
                video_budget: output.video_budget,
                target: output.target,
                current: output.current,
                prepare: output.prepare,
                paused: output.paused,
                required_active_count: edge.required_active_count(source),
            };
            demands.push(groups::edge_demand(
                Consumer::Edge(*edge_key),
                edge.media,
                &demand,
                ceiling,
            ));
        }
        for publication_key in publications {
            let Some(publication) = self.publications.get(publication_key) else {
                continue;
            };
            let Some(source) = state.source_of(publication.media) else {
                continue;
            };
            demands.push(groups::publication_demand(
                Consumer::Publication(*publication_key),
                publication.media,
                publication.required_active_count(),
                publication.lowest_layer_budget(source),
                ceiling,
            ));
        }
        demands
    }

    /// Plans the output groups and merges their recovery.
    pub(crate) fn plan_groups(
        &mut self,
        key: u64,
        demands: &[OutputDemand<Consumer>],
    ) -> Result<OutputPlan> {
        let anchor = self.anchor;
        let state = self.source(key)?;
        let relay = state.relay.is_some().then_some(state.format_count - 1);
        let input = state
            .has_input
            .then(|| state.input_at.map(|at| (state.input_pts, at)))
            .flatten();
        let SourceState {
            groups,
            media,
            group_media,
            next_media,
            output_bitrates,
            format_count,
            input_pts,
            ..
        } = state;
        let output_bitrates: &[u32] = output_bitrates;
        let mut factory = GroupFactory {
            original: media,
            group_media,
            next_media,
            output_bitrates,
            relay,
            anchor,
            input,
        };
        let (mut plan, result) = groups.plan(
            demands,
            output_bitrates,
            *format_count,
            *input_pts,
            &mut factory,
        );
        groups.take_recovery_layers(&mut plan);
        result.map_err(|error| match error {
            groups::PlanError::Shape => {
                Error::Invalid("shared output groups need an original and derived slot")
            }
            groups::PlanError::Media(error) => error,
        })?;
        Ok(plan)
    }

    pub(super) fn unavailable_demand(&self, key: u64, edge_key: u64) -> bool {
        let (Some(state), Some(edge)) = (self.sources.get(&key), self.edges.get(&edge_key)) else {
            return false;
        };
        let Some(source) = state.source_of(edge.media) else {
            return false;
        };
        let output = edge.output_state(source);
        let mut selected = output.target;
        if !output.paused {
            selected = selected.max(output.current);
        }
        let group_slot = state
            .groups
            .group_for_media(edge.media)
            .map(|group| group.slot as i32);
        let mut physical = selected;
        if selected == 0
            && let Some(slot) = group_slot
        {
            physical = slot;
        }
        if selected >= 0 && !state.failed_output(physical) {
            return false;
        }
        let mut fallback = edge.required_active_count(source) as i32 - 1;
        if fallback == 0
            && let Some(slot) = group_slot
        {
            fallback = slot;
        }
        state.failed_output(fallback)
    }

    pub(crate) fn begin_frame(
        &mut self,
        conns: &mut Connections,
        key: u64,
        pts: Duration,
        at: SystemTime,
        now: Instant,
    ) -> Result<OutputPlan> {
        let state = self.source(key)?;
        let edges: Vec<u64> = state.edges.keys().copied().collect();
        let publications: Vec<u64> = state.publications.iter().copied().collect();
        state.begin_group_frame(pts, at)?;
        let mut unavailable = Vec::new();
        for edge_key in &edges {
            let connected = self
                .edges
                .get(edge_key)
                .is_some_and(|edge| edge.state.is(PcState::Connected));
            if !connected {
                continue;
            }
            if !self.connect_edge(conns, *edge_key) || self.unavailable_demand(key, *edge_key) {
                unavailable.push(*edge_key);
                continue;
            }
            if let Some(edge) = self.edges.get_mut(edge_key) {
                edge.begin_frame();
            }
        }
        for edge_key in unavailable {
            self.close_edge(conns, edge_key, now);
        }
        let edges: Vec<u64> = self.source(key)?.edges.keys().copied().collect();
        let state = self.source(key)?;
        if state.format_count == 2 && state.output_bitrates.len() >= 2 {
            let demands = self.collect_demands(key, &edges, &publications);
            return self.plan_groups(key, &demands);
        }
        let state = &self.sources[&key];
        let mut demands = Vec::new();
        for edge_key in &edges {
            let Some(edge) = self.edges.get(edge_key) else {
                continue;
            };
            if !edge.state.is(PcState::Connected) {
                continue;
            }
            let Some(source) = state.source_of(edge.media) else {
                continue;
            };
            demands.push(SharedDemand {
                required: edge.required_active_count(source),
                remote: !edge.local,
                budget: edge.output_state(source).video_budget,
            });
        }
        for publication_key in &publications {
            let Some(publication) = self.publications.get(publication_key) else {
                continue;
            };
            let Some(source) = state.source_of(publication.media) else {
                continue;
            };
            demands.push(SharedDemand {
                required: publication.required_active_count(),
                remote: true,
                budget: publication.lowest_layer_budget(source),
            });
        }
        Ok(shared_plan(
            &state.output_bitrates,
            state.format_count,
            &demands,
            |budget| state.media.codec_budget(0, budget),
        ))
    }

    pub(crate) fn configure_outputs(&mut self, key: u64, ceilings: &[u32]) -> Result<()> {
        let state = self.source(key)?;
        let formats = state.format_count;
        if ceilings.len() < formats
            || ceilings.len() > formats + state.group_capacity()
            || formats != 2 && ceilings.len() != formats
        {
            return Err(Error::Invalid(
                "native output count does not match the source",
            ));
        }
        if ceilings.iter().any(|bitrate| *bitrate < 1000) {
            return Err(Error::Invalid("native output bitrate is invalid"));
        }
        state.output_bitrates = ceilings.to_vec();
        let mut layers = state.media.source().formats().to_vec();
        for (layer, format) in layers.iter_mut().enumerate() {
            format.bitrate = ceilings[layer];
        }
        state.media.source_mut().update_formats(layers.clone())?;
        state.groups.configure(ceilings);
        for (slot, group) in state
            .groups
            .groups()
            .map(|g| (g.slot, g.media))
            .collect::<Vec<_>>()
        {
            if slot == 0 {
                continue;
            }
            if let Some(media) = state.group_media.get_mut(&group) {
                let mut formats = layers.clone();
                formats[0].bitrate = ceilings[slot];
                formats[0].width = media.source().formats()[0].width;
                formats[0].height = media.source().formats()[0].height;
                media.source_mut().update_formats(formats)?;
            }
        }
        // Healthy outputs resume their trackers after a restored generation.
        Ok(())
    }

    pub(crate) fn set_format(
        &mut self,
        key: u64,
        layer: usize,
        width: u32,
        height: u32,
    ) -> Result<()> {
        let state = self.source(key)?;
        if layer >= state.format_count.max(state.output_bitrates.len()) || width == 0 || height == 0
        {
            return Err(Error::Invalid("native output format is invalid"));
        }
        if layer >= state.format_count {
            let media = state.groups.group(layer).map(|group| group.media);
            if let Some(media) = media.and_then(|media| state.group_media.get_mut(&media)) {
                media.source_mut().update_dimensions(0, width, height)?;
            }
            return Ok(());
        }
        state
            .media
            .source_mut()
            .update_dimensions(layer, width, height)?;
        if layer == 1 {
            for media in state.group_media.values_mut() {
                media.source_mut().update_dimensions(1, width, height)?;
            }
        }
        Ok(())
    }

    pub(crate) fn begin_generation(&mut self, key: u64) {
        let Ok(state) = self.source(key) else {
            return;
        };
        state.media.begin_generation();
        state.has_input = false;
        state.groups.begin_generation();
        for media in state.group_media.values_mut() {
            media.begin_generation();
        }
    }

    pub(crate) fn set_lowest_layer_rate_controlled(&mut self, key: u64, controlled: bool) {
        let Ok(state) = self.source(key) else {
            return;
        };
        state
            .media
            .source_mut()
            .set_lowest_layer_rate_controlled(controlled);
        for media in state.group_media.values_mut() {
            media
                .source_mut()
                .set_lowest_layer_rate_controlled(controlled);
        }
    }
}
