//! Real 127.0.0.1 UDP fixture: one `Media` on a `SharedUdp` and str0m client
//! peers, each driven by its own thread on its own socket.

#![allow(dead_code)]

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use piik_media_transport::str0m::change::{SdpAnswer, SdpOffer, SdpPendingOffer};
use piik_media_transport::str0m::net::{Protocol, Receive};
use piik_media_transport::str0m::{Candidate, Event, Input, Output, Rtc, RtcConfig};
use piik_media_transport::{HostAddrs, SharedUdp};
use piik_sfu::Media;
use piik_sfu::runtime::{
    PeerConnectionState, ResourceFence, SdpType, SessionDescription, SubscriptionFence,
};

pub const WAIT: Duration = Duration::from_secs(10);

/// A multi-thread runtime entered on the test thread, as signaling's
/// blocking pool would be, and the SFU on a loopback port.
pub struct Fixture {
    pub media: Media,
    _guard: tokio::runtime::EnterGuard<'static>,
}

/// One runtime for every test of a binary, as one server process has.
fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .build()
            .unwrap()
    })
}

pub fn fixture() -> Fixture {
    let guard = runtime().enter();
    let udp = SharedUdp::bind(
        SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0),
        HostAddrs::from_addrs([Ipv4Addr::LOCALHOST]),
    )
    .unwrap();
    Fixture {
        media: Media::new(Arc::new(udp)),
        _guard: guard,
    }
}

pub fn fence() -> ResourceFence {
    ResourceFence {
        room_id: "1234".into(),
        share_generation: "share_12345678".into(),
        publication_generation: "publication_12345678".into(),
    }
}

pub fn subscription(viewer: &str, connection: &str) -> SubscriptionFence {
    SubscriptionFence {
        resource: fence(),
        viewer_peer_id: viewer.into(),
        connection_id: connection.into(),
    }
}

pub fn until(condition: impl FnMut() -> bool, what: &str) {
    until_with(condition, || what.to_owned());
}

/// Like [`until`], describing the final state on timeout.
pub fn until_with(mut condition: impl FnMut() -> bool, what: impl Fn() -> String) {
    let deadline = Instant::now() + WAIT;
    while !condition() {
        assert!(Instant::now() < deadline, "timed out: {}", what());
        std::thread::sleep(Duration::from_millis(5));
    }
}

pub struct State {
    pub rtc: Rtc,
    pub events: Vec<Event>,
}

/// One str0m peer on its own socket.
pub struct Peer {
    state: Arc<Mutex<State>>,
    pub addr: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Peer {
    pub fn new(config: RtcConfig) -> Self {
        let socket = socket2::Socket::new(
            socket2::Domain::IPV4,
            socket2::Type::DGRAM,
            Some(socket2::Protocol::UDP),
        )
        .unwrap();
        // The media socket options, so a descheduled test peer loses nothing.
        piik_media_transport::configure_udp(&socket).unwrap();
        socket
            .bind(&SocketAddr::from((Ipv4Addr::LOCALHOST, 0)).into())
            .unwrap();
        let socket: UdpSocket = socket.into();
        socket.set_nonblocking(true).unwrap();
        let addr = socket.local_addr().unwrap();
        let mut rtc = config.build(Instant::now());
        rtc.add_local_candidate(Candidate::host(addr, "udp").unwrap());
        let state = Arc::new(Mutex::new(State {
            rtc,
            events: Vec::new(),
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (state, stop) = (state.clone(), stop.clone());
            std::thread::spawn(move || drive(&state, &stop, &socket, addr))
        };
        Self {
            state,
            addr,
            stop,
            thread: Some(thread),
        }
    }

    pub fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }

    pub fn with<R>(&self, f: impl FnOnce(&mut Rtc) -> R) -> R {
        f(&mut self.lock().rtc)
    }

    pub fn count(&self, f: impl Fn(&Event) -> bool) -> usize {
        self.lock().events.iter().filter(|event| f(event)).count()
    }

    pub fn connected(&self) -> bool {
        self.with(|rtc| rtc.is_connected())
    }

    /// Answers a remote offer, rewriting the answer text.
    pub fn answer(
        &self,
        offer: &SessionDescription,
        map: impl FnOnce(String) -> String,
    ) -> SessionDescription {
        assert_eq!(offer.sdp_type, SdpType::Offer);
        let answer = self.with(|rtc| {
            rtc.sdp_api()
                .accept_offer(SdpOffer::from_sdp_string(&offer.sdp).unwrap())
                .unwrap()
                .to_sdp_string()
        });
        SessionDescription {
            sdp_type: SdpType::Answer,
            sdp: map(answer),
        }
    }

    pub fn accept(&self, pending: SdpPendingOffer, answer: &SessionDescription) {
        assert_eq!(answer.sdp_type, SdpType::Answer);
        self.with(|rtc| {
            rtc.sdp_api()
                .accept_answer(pending, SdpAnswer::from_sdp_string(&answer.sdp).unwrap())
                .unwrap();
        });
    }

    pub fn texts(&self) -> Vec<String> {
        self.lock()
            .events
            .iter()
            .filter_map(|event| match event {
                Event::ChannelData(data) => Some(String::from_utf8_lossy(&data.data).into_owned()),
                _ => None,
            })
            .collect()
    }

    /// Writes text on the first open channel; false until one is open.
    pub fn send_text(&self, text: &str) -> bool {
        let mut state = self.lock();
        let Some(id) = state.events.iter().find_map(|event| match event {
            Event::ChannelOpen(id, _) => Some(*id),
            _ => None,
        }) else {
            return false;
        };
        state
            .rtc
            .channel(id)
            .is_some_and(|mut channel| channel.write(false, text.as_bytes()).unwrap_or(false))
    }

    pub fn close(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        self.close();
    }
}

fn drive(state: &Mutex<State>, stop: &AtomicBool, socket: &UdpSocket, local: SocketAddr) {
    let mut buffer = vec![0u8; 2048];
    while !stop.load(Ordering::Relaxed) {
        {
            let mut state = state.lock().unwrap();
            loop {
                match state.rtc.poll_output() {
                    Ok(Output::Timeout(at)) => {
                        if at <= Instant::now() {
                            let _ = state.rtc.handle_input(Input::Timeout(Instant::now()));
                            continue;
                        }
                        break;
                    }
                    Ok(Output::Transmit(transmit)) => {
                        let _ = socket.send_to(&transmit.contents, transmit.destination);
                    }
                    Ok(Output::Event(event)) => state.events.push(event),
                    Err(_) => return,
                }
            }
        }
        let mut received = false;
        while let Ok((count, source)) = socket.recv_from(&mut buffer) {
            received = true;
            if let Ok(receive) = Receive::new(Protocol::Udp, source, local, &buffer[..count]) {
                let mut state = state.lock().unwrap();
                let _ = state
                    .rtc
                    .handle_input(Input::Receive(Instant::now(), receive));
            }
        }
        if !received {
            std::thread::sleep(Duration::from_millis(1));
            let mut state = state.lock().unwrap();
            let _ = state.rtc.handle_input(Input::Timeout(Instant::now()));
        }
    }
}

/// Media events delivered to the test.
#[derive(Clone, Default)]
pub struct Events(pub Arc<Mutex<Vec<piik_sfu::runtime::MediaEvent>>>);

impl Events {
    pub fn install(&self, media: &Media) {
        let events = self.0.clone();
        media.set_event_sink(Arc::new(move |event| events.lock().unwrap().push(event)));
    }

    pub fn take(&self) -> Vec<piik_sfu::runtime::MediaEvent> {
        std::mem::take(&mut *self.0.lock().unwrap())
    }

    pub fn active_counts(&self) -> Vec<i64> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .filter_map(|event| event.active_count)
            .collect()
    }

    pub fn states(&self, connection: &str) -> Vec<PeerConnectionState> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .filter(|event| event.connection_id == connection && event.active_count.is_none())
            .map(|event| event.state)
            .filter(|state| *state != PeerConnectionState::Unknown)
            .collect()
    }
}
