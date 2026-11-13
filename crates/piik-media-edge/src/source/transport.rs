//! Forwarding of one source media to its attached edges and publications,
//! including tracker polling and moving a consumer between renditions.

use super::*;

impl App {
    /// Forwards `packets` of one media to every consumer attached to it.
    pub(crate) fn forward_media(
        &mut self,
        conns: &mut Connections,
        key: u64,
        media: MediaId,
        packets: &[SourcePacket],
        changes: SourceChanges,
        now: Instant,
    ) {
        let App {
            sources,
            edges,
            publications,
            ..
        } = self;
        let Some(state) = sources.get_mut(&key) else {
            return;
        };
        let mut requests = Vec::new();
        {
            let Some(source) = state.source_of(media) else {
                return;
            };
            for edge in edges.values_mut() {
                if edge.source != key || edge.media != media || edge.closed {
                    continue;
                }
                let Some(conn) = conns.get_mut(edge.conn) else {
                    continue;
                };
                edge.forward(
                    conn.rtc(),
                    source,
                    packets,
                    changes.any(),
                    now,
                    &mut requests,
                );
            }
            for publication in publications.values_mut() {
                if publication.source != key || publication.media != media || publication.closed {
                    continue;
                }
                let Some(conn) = conns.get_mut(publication.conn) else {
                    continue;
                };
                publication.forward(
                    conn.rtc(),
                    source,
                    packets,
                    changes.any(),
                    now,
                    &mut requests,
                );
            }
        }
        for request in requests {
            state.keyframe(media, request, now);
        }
    }

    /// Re-reads one media's tracker windows and reconciles its consumers.
    pub(crate) fn poll_source(&mut self, conns: &mut Connections, key: u64, now: Instant) {
        let Some(state) = self.sources.get_mut(&key) else {
            return;
        };
        if state.closed {
            return;
        }
        let mut changed = Vec::new();
        let original = state.media.source_mut().poll(now);
        if original.any() {
            changed.push((ORIGINAL, original));
        }
        for (id, media) in &mut state.group_media {
            let changes = media.source_mut().poll(now);
            if changes.any() {
                changed.push((*id, changes));
            }
        }
        for (media, changes) in changed {
            self.forward_media(conns, key, media, &[], changes, now);
        }
    }

    /// Moves a consumer to `next` on an independent frame.
    pub(crate) fn replace_consumer(
        &mut self,
        conns: &mut Connections,
        key: u64,
        consumer: Consumer,
        next: MediaId,
        now: Instant,
    ) {
        let App {
            sources,
            edges,
            publications,
            ..
        } = self;
        let Some(state) = sources.get_mut(&key) else {
            return;
        };
        let requests = match consumer {
            Consumer::Edge(edge_key) => {
                let Some(edge) = edges.get_mut(&edge_key) else {
                    return;
                };
                let (Some(current), Some(target)) =
                    (state.source_of(edge.media), state.source_of(next))
                else {
                    return;
                };
                let Some(conn) = conns.get_mut(edge.conn) else {
                    return;
                };
                let requests = edge.replace_source(conn.rtc(), current, target);
                edge.media = next;
                requests
            }
            Consumer::Publication(publication_key) => {
                let Some(publication) = publications.get_mut(&publication_key) else {
                    return;
                };
                let (Some(current), Some(target)) =
                    (state.source_of(publication.media), state.source_of(next))
                else {
                    return;
                };
                let Some(conn) = conns.get_mut(publication.conn) else {
                    return;
                };
                let requests = publication.replace_source(conn.rtc(), current, target);
                publication.media = next;
                requests
            }
        };
        for request in requests {
            state.keyframe(next, request, now);
        }
    }

    pub(super) fn install_group(
        &mut self,
        conns: &mut Connections,
        key: u64,
        group: Option<usize>,
        original: bool,
        now: Instant,
    ) {
        let Some(state) = self.sources.get(&key) else {
            return;
        };
        let current = |consumer: &Consumer| self.consumer_media(*consumer).unwrap_or(ORIGINAL);
        let moves = state.groups.install(group, original, current);
        for (consumer, next) in moves {
            self.replace_consumer(conns, key, consumer, next, now);
        }
    }

    fn consumer_media(&self, consumer: Consumer) -> Option<MediaId> {
        match consumer {
            Consumer::Edge(key) => self.edges.get(&key).map(|edge| edge.media),
            Consumer::Publication(key) => self.publications.get(&key).map(|p| p.media),
        }
    }
}
