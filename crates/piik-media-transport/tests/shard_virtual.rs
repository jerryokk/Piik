//! The sans-IO shard on virtual time: two ICE-lite SFU connections share one
//! address, carry data channels through `DataChannel` handles, and project
//! their lifecycle onto Pion's states. No wall time passes.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use piik_input_route::InputChannel;
use piik_media_transport::sdp::{VideoCodec, codec_config};
use piik_media_transport::str0m::change::{SdpAnswer, SdpPendingOffer};
use piik_media_transport::str0m::media::{Direction, MediaKind, Mid};
use piik_media_transport::str0m::net::{Protocol, Receive};
use piik_media_transport::str0m::{Candidate, Event, Input, Output, Rtc};
use piik_media_transport::{
    ConnEvent, ConnId, Connection, Connections, Datagram, PcState, Shard, ShardApp,
};

const LATENCY: Duration = Duration::from_millis(5);

fn addr(s: &str) -> SocketAddr {
    s.parse().unwrap()
}

#[derive(Default)]
struct Echo {
    states: BTreeMap<ConnId, Vec<PcState>>,
    channels: BTreeMap<ConnId, Arc<piik_media_transport::DataChannel>>,
    received: Vec<(ConnId, String)>,
    closed_channels: Vec<ConnId>,
    rtp: Vec<(ConnId, u16)>,
    rtp_received_at: Vec<Instant>,
}

enum Command {
    Close(ConnId),
    CloseChannel(ConnId),
}

impl ShardApp for Echo {
    type Command = Command;

    fn on_command(&mut self, _now: Instant, command: Command, conns: &mut Connections) {
        match command {
            Command::Close(id) => conns.close(id),
            Command::CloseChannel(id) => {
                if let Some(channel) = self.channels.get(&id) {
                    channel.close();
                }
            }
        }
    }

    fn on_event(
        &mut self,
        _now: Instant,
        conn: ConnId,
        event: ConnEvent,
        _conns: &mut Connections,
    ) {
        match event {
            ConnEvent::State(state) => self.states.entry(conn).or_default().push(state),
            ConnEvent::ChannelOpen(channel) => {
                self.channels.insert(conn, channel);
            }
            ConnEvent::ChannelData(channel, data) => {
                let text = String::from_utf8(data.data).unwrap();
                // Reply through the cross-thread handle, as input routing does.
                channel.send_text(&format!("echo:{text}"));
                self.received.push((conn, text));
            }
            ConnEvent::ChannelClose(channel) => {
                assert!(channel.is_closed());
                self.closed_channels.push(conn);
            }
            ConnEvent::Rtc(event) => {
                if let Event::RtpPacket(packet) = *event {
                    self.rtp.push((conn, packet.header.sequence_number));
                    self.rtp_received_at.push(packet.timestamp);
                }
            }
        }
    }
}

struct Client {
    rtc: Rtc,
    addr: SocketAddr,
    timeout: Instant,
    texts: Vec<String>,
    open: bool,
    online: bool,
}

struct World {
    now: Instant,
    shard: Shard<Echo>,
    sfu: SocketAddr,
    clients: Vec<Client>,
    routes: BTreeMap<SocketAddr, ConnId>,
    /// (deliver at, source, destination, contents)
    wire: Vec<(Instant, SocketAddr, SocketAddr, Vec<u8>)>,
}

impl World {
    fn new() -> Self {
        let now = Instant::now();
        Self {
            now,
            shard: Shard::new(Echo::default()),
            sfu: addr("192.0.2.1:7000"),
            clients: Vec::new(),
            routes: BTreeMap::new(),
            wire: Vec::new(),
        }
    }

    /// A browser-like client offers a data channel; the SFU answers it.
    fn connect_client(&mut self, client_addr: &str) -> (usize, ConnId) {
        self.connect_client_with(client_addr, false).0
    }

    /// As [`World::connect_client`], optionally offering one sendonly VP8
    /// video section; returns its mid.
    fn connect_client_with(
        &mut self,
        client_addr: &str,
        video: bool,
    ) -> ((usize, ConnId), Option<Mid>) {
        let client_addr = addr(client_addr);
        let mut rtc = Rtc::builder()
            .clear_codecs()
            .enable_vp8(true)
            .set_rtp_mode(video)
            .build(self.now);
        rtc.add_local_candidate(Candidate::host(client_addr, "udp").unwrap());
        let mut api = rtc.sdp_api();
        api.add_channel("piik".into());
        let mid =
            video.then(|| api.add_media(MediaKind::Video, Direction::SendOnly, None, None, None));
        let (offer, pending): (_, SdpPendingOffer) = api.apply().unwrap();

        let config = codec_config(&[VideoCodec::Vp8], false).set_ice_lite(true);
        let mut conn = Connection::new(config, self.now);
        conn.add_local_candidate(Candidate::host(self.sfu, "udp").unwrap());
        let answer = conn.accept_offer(&offer.to_sdp_string()).unwrap();
        rtc.sdp_api()
            .accept_answer(pending, SdpAnswer::from_sdp_string(&answer).unwrap())
            .unwrap();
        let id = self.shard.conns().insert(conn);
        self.routes.insert(client_addr, id);
        self.clients.push(Client {
            rtc,
            addr: client_addr,
            timeout: self.now,
            texts: Vec::new(),
            open: false,
            online: true,
        });
        ((self.clients.len() - 1, id), mid)
    }

    fn send(&mut self, source: SocketAddr, destination: SocketAddr, contents: Vec<u8>) {
        self.wire
            .push((self.now + LATENCY, source, destination, contents));
    }

    fn drain_shard(&mut self) {
        let sfu = self.sfu;
        for out in self.shard.poll(self.now) {
            assert_eq!(out.transmit.source, sfu);
            self.send(
                sfu,
                out.transmit.destination,
                out.transmit.contents.to_vec(),
            );
        }
    }

    fn drain_client(&mut self, index: usize) {
        loop {
            let client = &mut self.clients[index];
            // An offline client's DTLS times out; that is its own business.
            let Ok(output) = client.rtc.poll_output() else {
                client.timeout = self.now + Duration::from_secs(3600);
                return;
            };
            match output {
                Output::Timeout(at) => {
                    client.timeout = at.max(self.now + Duration::from_millis(1));
                    return;
                }
                Output::Transmit(t) => {
                    let (source, contents) = (t.source, t.contents.to_vec());
                    if client.online {
                        self.send(source, t.destination, contents);
                    }
                }
                Output::Event(Event::ChannelOpen(..)) => client.open = true,
                Output::Event(Event::ChannelData(data)) => {
                    client.texts.push(String::from_utf8(data.data).unwrap());
                }
                Output::Event(_) => {}
            }
        }
    }

    fn run_for(&mut self, duration: Duration) {
        let end = self.now + duration;
        self.drain_shard();
        for index in 0..self.clients.len() {
            self.drain_client(index);
        }
        loop {
            let wire = self.wire.iter().map(|w| w.0).min();
            let clients = self.clients.iter().map(|c| c.timeout).min();
            let next = [wire, clients, self.shard.next_timeout()]
                .into_iter()
                .flatten()
                .min()
                .unwrap_or(end);
            if next > end {
                self.now = end;
                return;
            }
            self.now = self.now.max(next);
            let now = self.now;
            let (due, later): (Vec<_>, Vec<_>) = std::mem::take(&mut self.wire)
                .into_iter()
                .partition(|w| w.0 <= now);
            self.wire = later;
            for (_, source, destination, contents) in due {
                if destination == self.sfu {
                    // The SharedUdp role: route by remote address.
                    if let Some(conn) = self.routes.get(&source) {
                        let datagram = Datagram {
                            received_at: now,
                            source,
                            destination,
                            contents,
                        };
                        self.shard.handle_datagram(now, *conn, &datagram);
                    }
                } else if let Some(index) = self.clients.iter().position(|c| c.addr == destination)
                    && self.clients[index].online
                {
                    let receive =
                        Receive::new(Protocol::Udp, source, destination, &contents).unwrap();
                    let _ = self.clients[index]
                        .rtc
                        .handle_input(Input::Receive(now, receive));
                    self.drain_client(index);
                }
            }
            for index in 0..self.clients.len() {
                if self.clients[index].timeout <= now {
                    let _ = self.clients[index].rtc.handle_input(Input::Timeout(now));
                    self.drain_client(index);
                }
            }
            self.shard.handle_timeout(now);
            self.drain_shard();
        }
    }

    fn client_send(&mut self, index: usize, text: &str) {
        let rtc = &mut self.clients[index].rtc;
        let id = rtc
            .sctp_channel_ids()
            .into_iter()
            .next()
            .expect("client channel");
        assert!(
            rtc.channel(id)
                .unwrap()
                .write(false, text.as_bytes())
                .unwrap()
        );
    }
}

trait ChannelIds {
    fn sctp_channel_ids(&mut self) -> Vec<piik_media_transport::str0m::channel::ChannelId>;
}

impl ChannelIds for Rtc {
    fn sctp_channel_ids(&mut self) -> Vec<piik_media_transport::str0m::channel::ChannelId> {
        // The offerer's first channel is stream 0.
        self.direct_api()
            .channel_id_by_sctp_stream_id(0)
            .into_iter()
            .collect()
    }
}

#[test]
fn shard_connects_isolates_and_echoes_through_handles() {
    let mut world = World::new();
    let (a, conn_a) = world.connect_client("203.0.113.7:40000");
    let (b, conn_b) = world.connect_client("203.0.113.8:40000");
    world.run_for(Duration::from_secs(3));

    for conn in [conn_a, conn_b] {
        assert_eq!(
            world.shard.app().states[&conn],
            [PcState::Connecting, PcState::Connected],
            "{conn}"
        );
        assert_eq!(
            world.shard.conns().get(conn).unwrap().state(),
            PcState::Connected
        );
    }
    assert!(world.clients[a].open && world.clients[b].open);
    let channel_a = world.shard.app().channels[&conn_a].clone();
    assert_eq!(channel_a.label(), "piik");
    assert!(channel_a.is_open() && !channel_a.is_closed());

    world.client_send(a, "from-a");
    world.client_send(b, "from-b");
    world.run_for(Duration::from_millis(200));
    assert_eq!(
        world.shard.app().received,
        [(conn_a, "from-a".to_owned()), (conn_b, "from-b".to_owned())]
    );
    assert_eq!(world.clients[a].texts, ["echo:from-a"]);
    assert_eq!(world.clients[b].texts, ["echo:from-b"]);
    // SCTP holds the echo until its (delayed) SACK.
    world.run_for(Duration::from_secs(1));
    assert_eq!(channel_a.buffered_amount(), 0);

    // close() stops sending at once and reports closed after SCTP does.
    world
        .shard
        .handle_command(world.now, Command::CloseChannel(conn_a));
    assert!(!channel_a.is_open() && !channel_a.is_closed());
    channel_a.send_text("dropped");
    world.run_for(Duration::from_millis(500));
    assert!(channel_a.is_closed());
    assert_eq!(world.shard.app().closed_channels, [conn_a]);
    assert_eq!(world.clients[a].texts, ["echo:from-a"]);
    assert_eq!(
        world.shard.conns().get(conn_a).unwrap().state(),
        PcState::Connected,
        "closing a channel keeps the connection"
    );

    // Closing a connection is final and removes it from the shard.
    world
        .shard
        .handle_command(world.now, Command::Close(conn_b));
    world.run_for(Duration::from_millis(10));
    assert_eq!(
        world.shard.app().states[&conn_b].last(),
        Some(&PcState::Closed)
    );
    assert!(world.shard.conns().get(conn_b).is_none());
    assert!(world.shard.app().channels[&conn_b].is_closed());
    assert!(world.shard.conns().get(conn_a).is_some());
}

/// A frame's packets arrive together and are handled in one datagram batch.
/// str0m's RTP mode holds one received packet until the next `poll_output`,
/// so every packet must reach the app, not only the last of the batch.
#[test]
fn batched_rtp_datagrams_all_reach_the_app() {
    let mut world = World::new();
    let ((client, conn), mid) = world.connect_client_with("203.0.113.7:40000", true);
    let mid = mid.expect("video mid");
    world.run_for(Duration::from_secs(3));
    assert_eq!(
        world.shard.conns().get(conn).unwrap().state(),
        PcState::Connected
    );

    let rtc = &mut world.clients[client].rtc;
    let pt = rtc
        .codec_config()
        .iter()
        .find(|params| params.spec().codec == piik_media_transport::str0m::format::Codec::Vp8)
        .map(|params| params.pt())
        .expect("vp8");
    let now = world.now;
    let mut api = rtc.direct_api();
    let tx = api.stream_tx_by_mid(mid, None).expect("video stream");
    let sent: Vec<u16> = (100..108).collect();
    for seq in &sent {
        tx.write_rtp(
            piik_media_transport::str0m::rtp::RtpWrite::new(
                pt,
                u64::from(*seq).into(),
                9000,
                now,
                [0x10, 0, 0, *seq as u8],
            )
            .marker(*seq == 107),
        );
    }
    world.drain_client(client);
    world.run_for(Duration::from_millis(50));

    let received: Vec<u16> = world
        .shard
        .app()
        .rtp
        .iter()
        .filter(|(id, _)| *id == conn)
        .map(|(_, seq)| *seq)
        .collect();
    assert_eq!(received, sent);
}

#[test]
fn queued_rtp_preserves_socket_receive_time() {
    let mut world = World::new();
    let ((client, conn), mid) = world.connect_client_with("203.0.113.7:40000", true);
    let mid = mid.unwrap();
    world.run_for(Duration::from_secs(3));
    let now = world.now;
    let rtc = &mut world.clients[client].rtc;
    let pt = rtc
        .codec_config()
        .iter()
        .find(|params| params.spec().codec == piik_media_transport::str0m::format::Codec::Vp8)
        .unwrap()
        .pt();
    rtc.direct_api()
        .stream_tx_by_mid(mid, None)
        .unwrap()
        .write_rtp(
            piik_media_transport::str0m::rtp::RtpWrite::new(
                pt,
                100u64.into(),
                9000,
                now,
                [0x10, 0, 0, 1],
            )
            .marker(true),
        );
    world.drain_client(client);
    for _ in 0..10 {
        if world.wire.iter().any(|(_, _, _, bytes)| {
            bytes.len() > 12 && bytes[0] & 0xc0 == 0x80 && bytes[1] & 0x7f == *pt
        }) {
            break;
        }
        world.now = world.clients[client].timeout;
        world.clients[client]
            .rtc
            .handle_input(Input::Timeout(world.now))
            .unwrap();
        world.drain_client(client);
    }
    let (received_at, source, destination, contents) = world
        .wire
        .iter()
        .find(|(_, _, _, bytes)| {
            bytes.len() > 12 && bytes[0] & 0xc0 == 0x80 && bytes[1] & 0x7f == *pt
        })
        .cloned()
        .expect("encrypted RTP datagram");
    let processed_at = received_at + Duration::from_millis(250);
    // Timers may already have advanced while this packet waited in the inbox.
    world.shard.handle_timeout(processed_at);
    world.shard.handle_datagram(
        processed_at,
        conn,
        &Datagram {
            received_at,
            source,
            destination,
            contents,
        },
    );
    assert_eq!(world.shard.app().rtp, [(conn, 100)]);
    assert_eq!(world.shard.app().rtp_received_at, [received_at]);
}

/// Peer loss: str0m's ICE-lite consent expires (15 s), the projection reports
/// `disconnected`, then `failed` 15 s later, matching Pion's ~30 s total.
#[test]
fn lost_peer_disconnects_then_fails() {
    let mut world = World::new();
    let (a, conn) = world.connect_client("203.0.113.7:40000");
    world.run_for(Duration::from_secs(3));
    assert_eq!(
        world.shard.conns().get(conn).unwrap().state(),
        PcState::Connected
    );

    world.clients[a].online = false;
    let lost = world.now;
    // The client's last consent check landed at most a few seconds earlier.
    world.run_for(Duration::from_secs(10));
    assert_eq!(
        world.shard.conns().get(conn).unwrap().state(),
        PcState::Connected
    );
    world.run_for(Duration::from_secs(6));
    assert_eq!(
        world.shard.conns().get(conn).unwrap().state(),
        PcState::Disconnected
    );
    world.run_for(Duration::from_secs(16));
    assert_eq!(
        world.shard.app().states[&conn],
        [
            PcState::Connecting,
            PcState::Connected,
            PcState::Disconnected,
            PcState::Failed
        ]
    );
    assert!(world.now - lost < Duration::from_secs(40));
}

/// A peer that never answers checks fails after Pion's 30 s connect bound.
#[test]
fn unreachable_peer_fails_after_connect_timeout() {
    let mut world = World::new();
    let (a, conn) = world.connect_client("203.0.113.7:40000");
    world.clients[a].online = false;
    world.run_for(Duration::from_secs(29));
    assert_eq!(
        world.shard.conns().get(conn).unwrap().state(),
        PcState::Connecting
    );
    world.run_for(Duration::from_secs(2));
    assert_eq!(
        world.shard.app().states[&conn],
        [PcState::Connecting, PcState::Failed]
    );
    // Shutdown still closes a failed connection.
    let now = world.now;
    world.shard.shutdown(now);
    assert_eq!(
        world.shard.app().states[&conn].last(),
        Some(&PcState::Closed)
    );
    assert!(world.shard.conns().is_empty());
}
