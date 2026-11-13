//! Projects room voice onto the route graph's physical facets.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::Arc;

use piik_protocol::{
    MediaRouteUpstream, RoomVoiceErrorCode, RoomVoiceState, ServerMessage, VoiceConnection,
    VoiceMember, VoicePublication,
};
use piik_route::UpstreamKind;

use crate::router::sfu::DrainKind;
use crate::runtime::{Completion, opaque_id, spawn_blocking};
use crate::server::Core;
use crate::voice::{RoomVoice, VoiceEdge, VoiceNegotiation};
use piik_sfu::runtime::{
    DeferredWork, SubscriptionFence, VoiceConnectionPlan, VoiceEventSink, VoiceRuntime,
};

impl Core {
    /// The voice runtime of the embedded SFU, when it has one.
    pub(crate) fn voice_runtime(&self) -> Option<Arc<dyn piik_sfu::runtime::Runtime>> {
        let media = self.router.sfu.as_ref()?.media.clone();
        media.voice().is_some().then_some(media)
    }

    pub(crate) fn sync_room_voice(&mut self, room_id: &str) {
        if !self.voices.contains_key(room_id) {
            return;
        }
        self.voices.get_mut(room_id).expect("voice").revision += 1;
        self.voice_room(room_id);
        let mut voice = self.voices.remove(room_id).expect("voice");
        let live: Vec<String> = voice
            .members
            .keys()
            .filter(|peer| self.voice_session_in(&voice, room_id, peer).is_none())
            .cloned()
            .collect();
        for peer in live {
            voice
                .members
                .get_mut(&peer)
                .expect("member")
                .publication_id
                .clear();
        }
        let mut publications: Vec<VoicePublication> = voice
            .members
            .iter()
            .filter(|(peer, member)| {
                !member.publication_id.is_empty() && self.voice_allowed_in(&voice, room_id, peer)
            })
            .map(|(peer, member)| VoicePublication {
                id: member.publication_id.clone(),
                peer_id: peer.clone(),
            })
            .collect();
        publications.sort_by(|a, b| a.id.cmp(&b.id));
        let previous = std::mem::take(&mut voice.connections);
        let (edges, retired) = self.room_voice_edges(room_id, &voice, previous);
        for mut edge in retired {
            self.close_voice_edge(room_id, &mut edge);
        }
        voice.connections = edges;
        let unavailable = self.project_voice_flows(room_id, &mut voice, &publications);
        let logical_ids: Vec<String> = voice.connections.keys().cloned().collect();
        for logical in logical_ids {
            if voice.connections[&logical].transport == "sfu" {
                self.configure_voice_junction(room_id, &mut voice, &logical, &publications);
            }
        }
        let mut members = Vec::with_capacity(voice.members.len());
        for (peer, member) in &voice.members {
            let session = self.voice_session_in(&voice, room_id, peer);
            let upstream = voice
                .connections
                .values()
                .find(|edge| {
                    edge.child == *peer
                        && !edge.pending
                        && edge.connected(&edge.child)
                        && edge.connected(&edge.parent)
                })
                .map_or(MediaRouteUpstream::None, |edge| {
                    if edge.transport == "peer" {
                        MediaRouteUpstream::peer(&edge.parent)
                    } else {
                        MediaRouteUpstream::Sfu
                    }
                });
            members.push(VoiceMember {
                peer_id: peer.clone(),
                connected: session.is_some(),
                joined: member.joined,
                upstream,
                publication_id: (!member.publication_id.is_empty())
                    .then(|| member.publication_id.clone()),
                deafened: None,
            });
        }
        let host_peer_id = (!voice.host_peer_id.is_empty()).then(|| voice.host_peer_id.clone());
        let enabled = voice.enabled();
        let recipients: Vec<(String, u64)> = voice
            .members
            .keys()
            .filter_map(|peer| {
                self.voice_session_in(&voice, room_id, peer)
                    .map(|key| (peer.clone(), key))
            })
            .collect();
        let mut messages = Vec::new();
        for (peer, key) in recipients {
            let mut recipient_members = members.clone();
            for member in &mut recipient_members {
                if member.connected {
                    member.deafened = voice.members.get(&member.peer_id).and_then(|m| m.deafened);
                }
            }
            let mut connections: Vec<VoiceConnection> = voice
                .connections
                .values()
                .filter(|edge| peer == edge.parent || peer == edge.child)
                .map(|edge| {
                    let other = if peer == edge.parent {
                        &edge.child
                    } else {
                        &edge.parent
                    };
                    VoiceConnection {
                        connection_id: edge.id.clone(),
                        parent_peer_id: (!edge.parent.is_empty()).then(|| edge.parent.clone()),
                        child_peer_id: edge.child.clone(),
                        transport: edge.transport.to_owned(),
                        pending: edge.pending,
                        revision: edge.revision,
                        send: edge.send.get(&peer).cloned().unwrap_or_default(),
                        receive: edge.send.get(other).cloned().unwrap_or_default(),
                    }
                })
                .collect();
            connections.sort_by(|a, b| a.connection_id.cmp(&b.connection_id));
            let state = RoomVoiceState {
                bitrate: Some(voice.bitrate),
                revision: voice.revision,
                enabled,
                host_peer_id: host_peer_id.clone(),
                members: recipient_members,
                publications: publications.clone(),
                connections,
                unavailable: unavailable.get(&peer).cloned().unwrap_or_default(),
            };
            messages.push((key, ServerMessage::RoomVoiceState(Box::new(state))));
        }
        self.voices.insert(room_id.to_owned(), voice);
        for (key, message) in messages {
            self.send(key, &message);
        }
    }

    /// The facets of the committed graph and the current attempt; returns the
    /// next connections and the previous ones to retire.
    fn room_voice_edges(
        &mut self,
        room_id: &str,
        voice: &RoomVoice,
        mut previous: BTreeMap<String, VoiceEdge>,
    ) -> (BTreeMap<String, VoiceEdge>, Vec<VoiceEdge>) {
        let mut next = BTreeMap::new();
        let mut retired = Vec::new();
        let Some(snapshot) = self.snapshot(room_id).filter(|_| self.room_media(room_id)) else {
            retired.extend(previous.into_values());
            return (next, retired);
        };
        let host_peer_id = self.host_peer_of(room_id);
        let mut add = |core: &mut Self,
                       logical: &str,
                       parent: &str,
                       child: &str,
                       transport: &'static str,
                       pending: bool,
                       revision: i64| {
            if !core.voice_allowed_in(voice, room_id, child)
                || (!parent.is_empty() && !core.voice_allowed_in(voice, room_id, parent))
            {
                return;
            }
            let session_of = |peer: &str| {
                core.voice_session_in(voice, room_id, peer)
                    .and_then(|key| core.session(key))
                    .map(|s| s.session_id.clone())
                    .unwrap_or_default()
            };
            let child_session = session_of(child);
            let parent_session = if parent.is_empty() {
                String::new()
            } else {
                session_of(parent)
            };
            let mut edge = previous.remove(logical);
            if let Some(existing) = edge.take() {
                if existing.parent_session != parent_session
                    || existing.child_session != child_session
                {
                    retired.push(existing);
                } else {
                    edge = Some(existing);
                }
            }
            let mut edge = edge.unwrap_or_else(|| VoiceEdge {
                id: opaque_id(),
                logical_id: logical.to_owned(),
                parent: parent.to_owned(),
                child: child.to_owned(),
                transport,
                parent_session: parent_session.clone(),
                child_session: child_session.clone(),
                pending,
                revision,
                send: HashMap::new(),
                connected: HashMap::new(),
                sfu_plan: None,
                sfu_work: None,
            });
            edge.pending = pending;
            edge.revision = revision;
            edge.send = HashMap::from([
                (parent.to_owned(), Vec::new()),
                (child.to_owned(), Vec::new()),
            ]);
            next.insert(logical.to_owned(), edge);
        };
        if let Some(publication) = snapshot
            .host_publication
            .as_ref()
            .filter(|p| p.physical_active)
        {
            add(
                self,
                &publication.generation,
                "",
                &host_peer_id,
                "sfu",
                false,
                snapshot.revision,
            );
        }
        for (child, edge) in snapshot.upstream_by_viewer.iter() {
            if !edge.physical_active {
                continue;
            }
            let (parent, transport) = if edge.kind == UpstreamKind::Sfu {
                ("", "sfu")
            } else {
                (edge.parent_peer_id.as_str(), "peer")
            };
            add(
                self,
                &edge.connection_id,
                parent,
                child,
                transport,
                false,
                snapshot.revision,
            );
        }
        if let Some(op) = snapshot.operation.as_ref()
            && let Some(current) = op.current.as_ref()
            && op.attach_connection_id.is_empty()
        {
            let (parent, transport) = if current.tuple.kind == UpstreamKind::Sfu {
                ("", "sfu")
            } else {
                (current.tuple.parent_peer_id.as_str(), "peer")
            };
            if current.tuple.kind == UpstreamKind::Sfu
                && let Some(prepared) = self
                    .room_rt(room_id)
                    .and_then(|rm| rm.pending_media.clone())
                && prepared.connection_id == current.connection_id
                && !prepared.publication_connection_id.is_empty()
            {
                add(
                    self,
                    &prepared.publication_generation,
                    "",
                    &host_peer_id,
                    "sfu",
                    true,
                    current.revision,
                );
            }
            add(
                self,
                &current.connection_id,
                parent,
                &op.child_peer_id,
                transport,
                true,
                current.revision,
            );
        }
        retired.extend(previous.into_values());
        (next, retired)
    }

    /// Every publication takes the unique path in the committed room tree. A
    /// relay outside voice is not a permissible decoded-audio hop.
    fn project_voice_flows(
        &self,
        room_id: &str,
        voice: &mut RoomVoice,
        publications: &[VoicePublication],
    ) -> HashMap<String, Vec<String>> {
        let mut adjacent: HashMap<String, Vec<(String, String, String)>> = HashMap::new();
        for edge in voice.connections.values().filter(|edge| !edge.pending) {
            adjacent.entry(edge.parent.clone()).or_default().push((
                edge.parent.clone(),
                edge.child.clone(),
                edge.logical_id.clone(),
            ));
            adjacent.entry(edge.child.clone()).or_default().push((
                edge.child.clone(),
                edge.parent.clone(),
                edge.logical_id.clone(),
            ));
        }
        let mut unavailable: HashMap<String, Vec<String>> = HashMap::new();
        let allowed: Vec<String> = voice
            .members
            .keys()
            .filter(|peer| self.voice_allowed_in(voice, room_id, peer))
            .cloned()
            .collect();
        for publication in publications {
            let mut paths: HashMap<String, Vec<(String, String)>> =
                HashMap::from([(publication.peer_id.clone(), Vec::new())]);
            let mut queue = VecDeque::from([publication.peer_id.clone()]);
            while let Some(from) = queue.pop_front() {
                for (step_from, to, logical) in adjacent.get(&from).cloned().unwrap_or_default() {
                    if paths.contains_key(&to) {
                        continue;
                    }
                    let mut path = paths[&from].clone();
                    path.push((step_from, logical));
                    paths.insert(to.clone(), path);
                    queue.push_back(to);
                }
            }
            let mut used = HashSet::new();
            for peer in &allowed {
                if *peer == publication.peer_id {
                    continue;
                }
                let Some(path) = paths.get(peer) else {
                    unavailable
                        .entry(peer.clone())
                        .or_default()
                        .push(publication.id.clone());
                    continue;
                };
                for (from, logical) in path {
                    if used.insert(logical.clone())
                        && let Some(edge) = voice.connections.get_mut(logical)
                    {
                        edge.send
                            .entry(from.clone())
                            .or_default()
                            .push(publication.id.clone());
                    }
                }
            }
        }
        unavailable
    }

    pub(crate) fn configure_voice_junction(
        &mut self,
        room_id: &str,
        voice: &mut RoomVoice,
        logical: &str,
        publications: &[VoicePublication],
    ) {
        let Some(media) = self.voice_runtime() else {
            return;
        };
        let edge = voice.connections.get_mut(logical).expect("edge");
        let mut plan = VoiceConnectionPlan {
            room_id: room_id.to_owned(),
            peer_id: edge.child.clone(),
            session_id: edge.child_session.clone(),
            connection_id: edge.id.clone(),
            publish: Vec::new(),
            subscribe: Vec::new(),
        };
        let child_sends = edge.send.get(&edge.child).cloned().unwrap_or_default();
        let sfu_sends = edge.send.get("").cloned().unwrap_or_default();
        for publication in publications {
            let item = piik_sfu::runtime::VoicePublication {
                id: publication.id.clone(),
                peer_id: publication.peer_id.clone(),
            };
            if child_sends.contains(&publication.id) {
                plan.publish.push(item.clone());
            }
            if sfu_sends.contains(&publication.id) {
                plan.subscribe.push(item);
            }
        }
        if edge
            .sfu_plan
            .as_ref()
            .is_some_and(|(_, current)| *current == plan)
        {
            return;
        }
        let plan_id = self.router.next_id();
        let edge = voice.connections.get_mut(logical).expect("edge");
        edge.sfu_plan = Some((plan_id, plan.clone()));
        let locker = self.locker.clone();
        let emit: VoiceEventSink = Arc::new(move |event| {
            locker.with(|core| core.on_voice_media_event(event));
        });
        let runtime: &dyn VoiceRuntime = media.voice().expect("voice runtime");
        match runtime.configure_voice(plan, emit) {
            Err(_) => self.fail_voice_plan_edge(room_id, edge),
            Ok(work) => {
                if let Some(operation) = edge.sfu_work.as_mut() {
                    operation.run = work;
                    return;
                }
                if let Some(work) = work {
                    let done = Completion::new();
                    edge.sfu_work = Some(VoiceNegotiation {
                        run: Some(work),
                        done: done.clone(),
                    });
                    self.router.inflight += 1;
                    let locker = self.locker.clone();
                    let (room, voice_id, logical, edge_id) = (
                        room_id.to_owned(),
                        voice.id,
                        logical.to_owned(),
                        edge.id.clone(),
                    );
                    spawn_blocking(move || {
                        negotiate_voice(locker, room, voice_id, logical, edge_id, done)
                    });
                }
            }
        }
    }

    pub(crate) fn fail_voice_plan_edge(&mut self, room_id: &str, edge: &mut VoiceEdge) {
        edge.sfu_plan = None;
        let child_session = edge.child_session.clone();
        if let Some(key) = self.voice_session(room_id, &edge.child)
            && self
                .session(key)
                .is_some_and(|s| s.session_id == child_session)
        {
            self.voice_error(key, RoomVoiceErrorCode::ConnectionUnavailable);
        }
    }

    pub(crate) fn close_voice_edge(&mut self, room_id: &str, edge: &mut VoiceEdge) {
        if edge.transport != "sfu" {
            return;
        }
        let Some(media) = self.voice_runtime() else {
            return;
        };
        let runtime = media.voice().expect("voice runtime");
        let mut close_physical: Option<DeferredWork> =
            runtime.close_voice(room_id, &edge.child, &edge.child_session, &edge.id);
        if let Some(operation) = edge.sfu_work.as_ref() {
            let done = operation.done.clone();
            let close_transport = close_physical.take();
            close_physical = Some(Box::new(move || {
                let result = close_transport.map_or(Ok(()), |close| close());
                let _ = done.wait();
                result
            }));
        }
        let Some(close_physical) = close_physical else {
            return;
        };
        let key = SubscriptionFence {
            resource: piik_sfu::runtime::ResourceFence {
                room_id: room_id.to_owned(),
                share_generation: String::new(),
                publication_generation: edge.id.clone(),
            },
            viewer_peer_id: String::new(),
            connection_id: edge.id.clone(),
        };
        self.register_drain_task(DrainKind::Voice, key.clone(), Some(close_physical));
        self.run_sfu_drain(&key);
    }

    /// Whether `voices[room]` and its `logical` connection still are the
    /// worker's exact voice room and physical edge.
    fn voice_edge_current(
        &self,
        room_id: &str,
        voice_id: u64,
        logical: &str,
        edge_id: &str,
    ) -> bool {
        self.voices.get(room_id).is_some_and(|voice| {
            voice.id == voice_id
                && voice
                    .connections
                    .get(logical)
                    .is_some_and(|edge| edge.id == edge_id)
        })
    }
}

/// One worker per physical edge coalesces graph changes into the latest plan
/// without yielding its caller's lock.
fn negotiate_voice(
    locker: crate::runtime::Locker,
    room_id: String,
    voice_id: u64,
    logical: String,
    edge_id: String,
    done: Arc<Completion>,
) {
    loop {
        let next = locker.with(|core| {
            if core.closing || !core.voice_edge_current(&room_id, voice_id, &logical, &edge_id) {
                return None;
            }
            let edge = core
                .voices
                .get_mut(&room_id)?
                .connections
                .get_mut(&logical)?;
            let plan_id = edge.sfu_plan.as_ref()?.0;
            let run = edge.sfu_work.as_mut()?.run.take()?;
            Some((plan_id, run))
        });
        let Some(Some((plan_id, run))) = next else {
            break;
        };
        let result = run();
        let again = locker.with(|core| {
            if !core.voice_edge_current(&room_id, voice_id, &logical, &edge_id) {
                return false;
            }
            let mut voice = core.voices.remove(&room_id).expect("voice");
            let edge = voice.connections.get_mut(&logical).expect("edge");
            let again = edge
                .sfu_plan
                .as_ref()
                .is_none_or(|(current, _)| *current != plan_id)
                && edge.sfu_plan.is_some();
            if !again
                && result.is_err()
                && edge
                    .sfu_plan
                    .as_ref()
                    .is_some_and(|(current, _)| *current == plan_id)
            {
                core.fail_voice_plan_edge(&room_id, edge);
            }
            core.voices.insert(room_id.clone(), voice);
            again
        });
        if again != Some(true) {
            break;
        }
    }
    locker.with(|core| {
        if let Some(edge) = core
            .voices
            .get_mut(&room_id)
            .filter(|voice| voice.id == voice_id)
            .and_then(|voice| voice.connections.get_mut(&logical))
            .filter(|edge| edge.id == edge_id)
        {
            edge.sfu_work = None;
        }
        core.router.inflight -= 1;
    });
    done.complete(Ok(()));
}
