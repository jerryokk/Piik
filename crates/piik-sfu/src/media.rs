//! The embedded SFU's physical media owner and its [`Runtime`] boundary.
//!
//! [`Media`] keeps a small registry (publication and voice handles, their
//! rooms and cached demand) behind one short-lived mutex, so the methods
//! signaling calls under its authority lock never block. Every `Rtc` lives in
//! one shard per room ([`RoomApp`] on `piik-media-transport`'s driver);
//! blocking methods send that shard a job and wait for its reply. Callbacks
//! leave the shard through one dispatcher thread per room and run without
//! any media lock.

use std::collections::{HashMap, VecDeque};
use std::io;
use std::net::SocketAddrV4;
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Instant;

use piik_media_transport::{Connections, HostAddrs, Shard, SharedUdp, spawn};
use tokio::runtime::{Handle, RuntimeFlavor};
use tokio::sync::mpsc::error::TrySendError;

use crate::room::{RoomApp, SubscriptionKey};
use crate::runtime::{
    DeferredWork, IceCandidateInit, MediaError, MediaEvent, PublicationMedia, ResourceFence,
    Runtime, SessionDescription, SubscriptionFence, VoiceRuntime,
};
use crate::voice::{VoiceEntry, VoiceKey};

/// Receives publication and subscription callbacks. It runs on a media
/// dispatcher thread without media locks and may call back into [`Media`],
/// including its blocking methods.
pub type MediaEventSink = Arc<dyn Fn(MediaEvent) + Send + Sync>;

/// Shard work: runs in the room's shard task with its connections.
pub(crate) type Job = Box<dyn FnOnce(&mut RoomApp, Instant, &mut Connections) + Send>;
/// A callback leaving the shard; runs on the room's dispatcher thread.
pub(crate) type Dispatch = Box<dyn FnOnce() + Send>;
/// The reply channel of one blocking shard request.
pub(crate) type Reply<T> = std_mpsc::Sender<Result<T, MediaError>>;

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub(crate) fn unavailable() -> MediaError {
    MediaError::failed("SFU publication media is unavailable")
}

fn subscription_unavailable() -> MediaError {
    MediaError::failed("SFU subscription media is unavailable")
}

/// Waits for a shard reply. On a multi-thread runtime worker the wait moves
/// the worker's other tasks elsewhere first; shards never wait on callers.
/// Callers on a current-thread runtime must not share it with the shards.
pub(crate) fn wait<T>(
    rx: &std_mpsc::Receiver<Result<T, MediaError>>,
    deadline: Option<Instant>,
) -> Result<T, MediaError> {
    let receive = || {
        let received = match deadline {
            None => rx.recv().map_err(|_| unavailable()),
            Some(deadline) => rx
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .map_err(|error| match error {
                    std_mpsc::RecvTimeoutError::Timeout => {
                        MediaError::failed("SFU media operation passed its deadline")
                    }
                    std_mpsc::RecvTimeoutError::Disconnected => unavailable(),
                }),
        };
        received.and_then(|result| result)
    };
    match Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(receive)
        }
        _ => receive(),
    }
}

/// One room's shard. Jobs queue without bound (their callers are bounded by
/// room authority); the shard's bounded command channel only carries wakes.
pub(crate) struct Room {
    jobs: Arc<Mutex<VecDeque<Job>>>,
    wake: tokio::sync::mpsc::Sender<()>,
}

impl Room {
    fn spawn(shared: &Arc<Shared>, room_id: &str) -> Arc<Self> {
        let jobs = Arc::new(Mutex::new(VecDeque::new()));
        let (events, dispatch) = std_mpsc::channel::<Dispatch>();
        // Ends once the shard (the only sender owner) has stopped.
        let _ = std::thread::Builder::new()
            .name(format!("piik-sfu-room-{room_id}"))
            .spawn(move || {
                for callback in dispatch {
                    callback();
                }
            });
        let inbox = Arc::new(OnceLock::new());
        let app = RoomApp::new(
            Arc::downgrade(shared),
            shared.udp.clone(),
            jobs.clone(),
            events,
            inbox.clone(),
        );
        let _entered = shared.runtime.enter();
        let (handle, _task) = spawn(Shard::new(app));
        let _ = inbox.set(handle.inbox.clone());
        Arc::new(Self {
            jobs,
            wake: handle.commands,
        })
    }

    /// Queues a job; false once the shard has stopped.
    pub(crate) fn send(&self, job: Job) -> bool {
        lock(&self.jobs).push_back(job);
        !matches!(self.wake.try_send(()), Err(TrySendError::Closed(())))
    }

    /// Runs `job` in the shard and waits for the reply it sends, at most
    /// until `deadline`.
    pub(crate) fn request<T: Send + 'static>(
        &self,
        deadline: Option<Instant>,
        job: impl FnOnce(&mut RoomApp, Instant, &mut Connections, Reply<T>) + Send + 'static,
    ) -> Result<T, MediaError> {
        let (reply, receive) = std_mpsc::channel();
        if !self.send(Box::new(move |app, now, conns| job(app, now, conns, reply))) {
            return Err(unavailable());
        }
        wait(&receive, deadline)
    }

    /// Runs `job` in the shard and waits for its result.
    pub(crate) fn call<T: Send + 'static>(
        &self,
        job: impl FnOnce(&mut RoomApp, Instant, &mut Connections) -> Result<T, MediaError>
        + Send
        + 'static,
    ) -> Result<T, MediaError> {
        self.request(None, move |app, now, conns, reply| {
            let _ = reply.send(job(app, now, conns));
        })
    }
}

/// Jobs drained by the shard on each wake.
pub(crate) fn drain_jobs(jobs: &Mutex<VecDeque<Job>>) -> VecDeque<Job> {
    std::mem::take(&mut *lock(jobs))
}

pub(crate) struct PublicationEntry {
    pub id: u64,
    pub connection_id: String,
    pub demand: i64,
    pub room: Arc<Room>,
}

pub(crate) struct Registry {
    pub closed: bool,
    next_id: u64,
    rooms: HashMap<String, Arc<Room>>,
    pub publications: HashMap<ResourceFence, PublicationEntry>,
    pub voices: HashMap<VoiceKey, VoiceEntry>,
    pub events: Option<MediaEventSink>,
}

impl Registry {
    pub(crate) fn next_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    pub(crate) fn room(&mut self, shared: &Arc<Shared>, room_id: &str) -> Arc<Room> {
        self.rooms
            .entry(room_id.to_owned())
            .or_insert_with(|| Room::spawn(shared, room_id))
            .clone()
    }

    /// Stops routing new work to an empty room; in-flight work keeps its
    /// shard alive until it finishes.
    pub(crate) fn release_room(&mut self, room_id: &str) {
        let used = self.publications.keys().any(|f| f.room_id == room_id)
            || self.voices.keys().any(|k| k.room == room_id);
        if !used {
            self.rooms.remove(room_id);
        }
    }
}

pub(crate) struct Shared {
    pub registry: Mutex<Registry>,
    pub udp: Arc<SharedUdp>,
    runtime: Handle,
}

/// The embedded SFU: publications keyed by [`ResourceFence`], their
/// subscribers, and room voice junctions, all on one ICE-lite UDP port.
///
/// It owns physical resources already authorized by the room's
/// [`crate::admission::Admission`]; it does not admit rooms, issue
/// credentials, retry, or choose routes. Construct it inside a multi-thread
/// tokio runtime; its shards run there.
#[derive(Clone)]
pub struct Media {
    shared: Arc<Shared>,
}

impl Media {
    /// Uses an application-owned port. The port's host snapshot (bind
    /// address, NAT 1:1 public IP, optional loopback) supplies each new
    /// connection's host candidates.
    ///
    /// # Panics
    /// Outside a tokio runtime.
    pub fn new(udp: Arc<SharedUdp>) -> Self {
        Self {
            shared: Arc::new(Shared {
                registry: Mutex::new(Registry {
                    closed: false,
                    next_id: 0,
                    rooms: HashMap::new(),
                    publications: HashMap::new(),
                    voices: HashMap::new(),
                    events: None,
                }),
                udp,
                runtime: Handle::current(),
            }),
        }
    }

    /// Binds the SFU port and its host candidates, as
    /// [`SharedUdp::bind`] does.
    pub fn bind(addr: SocketAddrV4, hosts: HostAddrs) -> io::Result<Self> {
        Ok(Self::new(Arc::new(SharedUdp::bind(addr, hosts)?)))
    }

    pub fn udp(&self) -> &Arc<SharedUdp> {
        &self.shared.udp
    }

    /// Installs the publication/subscription callback, normally
    /// `piik_signal::Server::handle_sfu_media_event`. Earlier events are
    /// dropped.
    pub fn set_event_sink(&self, sink: MediaEventSink) {
        lock(&self.shared.registry).events = Some(sink);
    }

    /// Closes every publication, subscription and voice connection and waits
    /// for their shards. Blocking; later calls are rejected.
    pub fn close(&self) -> Result<(), MediaError> {
        let rooms: Vec<Arc<Room>> = {
            let mut registry = lock(&self.shared.registry);
            if registry.closed {
                return Ok(());
            }
            registry.closed = true;
            registry.publications.clear();
            registry.voices.clear();
            registry.rooms.drain().map(|(_, room)| room).collect()
        };
        let mut result = Ok(());
        for room in rooms {
            if let Err(error) = room.call(|app, now, conns| {
                app.shutdown(now, conns);
                Ok(())
            }) && result.is_ok()
            {
                result = Err(error);
            }
        }
        result
    }

    pub(crate) fn shared(&self) -> &Arc<Shared> {
        &self.shared
    }

    fn publication(
        &self,
        fence: &ResourceFence,
        connection_id: &str,
    ) -> Result<(u64, Arc<Room>), MediaError> {
        let registry = lock(&self.shared.registry);
        registry
            .publications
            .get(fence)
            .filter(|entry| {
                !registry.closed
                    && (connection_id.is_empty() || entry.connection_id == connection_id)
            })
            .map(|entry| (entry.id, entry.room.clone()))
            .ok_or_else(unavailable)
    }

    fn subscription(
        &self,
        fence: &SubscriptionFence,
        connection_id: &str,
    ) -> Result<(u64, Arc<Room>, SubscriptionKey), MediaError> {
        let (id, room) = self
            .publication(&fence.resource, "")
            .map_err(|_| subscription_unavailable())?;
        Ok((
            id,
            room,
            SubscriptionKey {
                peer_id: fence.viewer_peer_id.clone(),
                connection_id: connection_id.to_owned(),
            },
        ))
    }

    /// Removes the exact publication entry once its physical close is done.
    fn forget_publication(&self, fence: &ResourceFence, id: u64) {
        let mut registry = lock(&self.shared.registry);
        if registry.publications.get(fence).is_some_and(|e| e.id == id) {
            registry.publications.remove(fence);
            registry.release_room(&fence.room_id);
        }
    }
}

/// These are this SFU's admission bounds, not the descriptive wire format:
/// the full Browser/Native bitrate range forwards without transcoding.
pub(crate) fn valid_publication_media(media: &PublicationMedia) -> bool {
    if (media.codec != "h264" && media.codec != "vp8")
        || media.formats.is_empty()
        || media.formats.len() > 3
        || media.rids.len() != media.formats.len()
        || (media.audio && media.audio_bitrate == 0)
    {
        return false;
    }
    media.formats.iter().enumerate().all(|(index, format)| {
        let rid = &media.rids[index];
        (1..=8192).contains(&format.width)
            && (1..=8192).contains(&format.height)
            && (1..=240_000_000).contains(&format.bitrate)
            && rid.len() <= 16
            && (!rid.is_empty() || media.rids.len() == 1)
            && !media.rids[..index].contains(rid)
    })
}

impl Runtime for Media {
    fn has_publication(&self, fence: &ResourceFence, connection_id: &str) -> bool {
        self.publication(fence, connection_id).is_ok()
    }

    /// Called only after the admission reservation; a repeated preparation
    /// never replaces a live publication.
    fn prepare_publication(
        &self,
        fence: &ResourceFence,
        connection_id: &str,
        media: &PublicationMedia,
    ) -> Result<(), MediaError> {
        if !fence.is_valid() || connection_id.is_empty() || !valid_publication_media(media) {
            return Err(MediaError::failed("SFU publication media is invalid"));
        }
        let mut registry = lock(&self.shared.registry);
        if registry.closed || registry.publications.contains_key(fence) {
            return Err(MediaError::failed("SFU publication is unavailable"));
        }
        let id = registry.next_id();
        let room = registry.room(&self.shared, &fence.room_id);
        registry.publications.insert(
            fence.clone(),
            PublicationEntry {
                id,
                connection_id: connection_id.to_owned(),
                demand: 0,
                room: room.clone(),
            },
        );
        let (fence, connection_id, media) =
            (fence.clone(), connection_id.to_owned(), media.clone());
        let fence_key = fence.clone();
        if !room.send(Box::new(move |app, now, conns| {
            app.create_publication(id, fence, connection_id, media, now, conns);
        })) {
            registry.publications.remove(&fence_key);
            return Err(unavailable());
        }
        Ok(())
    }

    fn update_publication(
        &self,
        fence: &ResourceFence,
        connection_id: &str,
        media: &PublicationMedia,
    ) -> Result<(), MediaError> {
        let (id, room) = self.publication(fence, connection_id)?;
        let media = media.clone();
        room.call(move |app, _, conns| app.update_publication(id, media, conns))
    }

    fn publication_demand(
        &self,
        fence: &ResourceFence,
        connection_id: &str,
    ) -> Result<i64, MediaError> {
        let registry = lock(&self.shared.registry);
        registry
            .publications
            .get(fence)
            .filter(|e| {
                !registry.closed && (connection_id.is_empty() || e.connection_id == connection_id)
            })
            .map(|e| e.demand)
            .ok_or_else(unavailable)
    }

    fn accept_publisher_offer(
        &self,
        fence: &ResourceFence,
        connection_id: &str,
        offer: SessionDescription,
    ) -> Result<SessionDescription, MediaError> {
        let (id, room) = self.publication(fence, connection_id)?;
        room.call(move |app, _, conns| app.accept_publisher_offer(id, offer, conns))
    }

    fn add_publisher_ice(
        &self,
        fence: &ResourceFence,
        connection_id: &str,
        candidate: IceCandidateInit,
    ) -> Result<(), MediaError> {
        let (id, room) = self.publication(fence, connection_id)?;
        room.call(move |app, _, conns| app.add_publisher_ice(id, &candidate, conns))
    }

    /// Registers the subscriber at once (it bootstraps demand), waits for the
    /// publisher's first video, then offers. `deadline` is the route
    /// operation's, not a second media timeout.
    fn prepare_subscriber(
        &self,
        deadline: Instant,
        fence: &SubscriptionFence,
        connection_id: &str,
    ) -> Result<SessionDescription, MediaError> {
        if fence.viewer_peer_id.is_empty() || connection_id.is_empty() {
            return Err(subscription_unavailable());
        }
        let (id, room, key) = self.subscription(fence, connection_id)?;
        let token = lock(&self.shared.registry).next_id();
        let resource_connection = fence.connection_id.clone();
        let prepared_key = key.clone();
        let result = room.request(Some(deadline), move |app, now, conns, reply| {
            app.prepare_subscriber(
                id,
                prepared_key,
                resource_connection,
                token,
                reply,
                now,
                conns,
            );
        });
        if result.is_err() {
            // The route operation owns this attempt; a late offer is retired.
            room.send(Box::new(move |app, _, conns| {
                app.cancel_subscriber(id, &key, token, conns);
            }));
        }
        result
    }

    fn apply_subscriber_answer(
        &self,
        fence: &SubscriptionFence,
        connection_id: &str,
        answer: SessionDescription,
    ) -> Result<(), MediaError> {
        let (id, room, key) = self.subscription(fence, connection_id)?;
        room.call(move |app, _, conns| app.apply_subscriber_answer(id, &key, &answer, conns))
    }

    fn add_subscriber_ice(
        &self,
        fence: &SubscriptionFence,
        connection_id: &str,
        candidate: IceCandidateInit,
    ) -> Result<(), MediaError> {
        let (id, room, key) = self.subscription(fence, connection_id)?;
        room.call(move |app, _, conns| app.add_subscriber_ice(id, &key, &candidate, conns))
    }

    /// Keeps the committed transport and asks ICE to recover it.
    fn restart_subscriber(
        &self,
        fence: &SubscriptionFence,
        connection_id: &str,
    ) -> Result<SessionDescription, MediaError> {
        let (id, room, key) = self.subscription(fence, connection_id)?;
        room.call(move |app, _, conns| app.restart_subscriber(id, &key, conns))
    }

    fn close_subscription(
        &self,
        fence: &SubscriptionFence,
        connection_id: &str,
    ) -> Result<(), MediaError> {
        let Ok((id, room, _)) = self.subscription(fence, connection_id) else {
            return Ok(());
        };
        let (fence, connection_id) = (fence.clone(), connection_id.to_owned());
        room.call(move |app, _, conns| {
            app.close_subscription(id, &fence, &connection_id, conns);
            Ok(())
        })
    }

    fn close_publication(&self, fence: &ResourceFence) -> Result<(), MediaError> {
        let Ok((id, room)) = self.publication(fence, "") else {
            return Ok(());
        };
        let result = room.call(move |app, _, conns| {
            app.close_publication(id, conns);
            Ok(())
        });
        self.forget_publication(fence, id);
        result
    }

    fn supports_retire_publication(&self) -> bool {
        true
    }

    /// Detaches only the identified video publisher; its logical room
    /// reservation stays admitted and a replacement may be prepared at once.
    fn retire_publication(
        &self,
        fence: &ResourceFence,
        connection_id: &str,
    ) -> Option<DeferredWork> {
        let (id, room) = {
            let mut registry = lock(&self.shared.registry);
            let entry = registry.publications.get(fence)?;
            if entry.connection_id != connection_id {
                return None;
            }
            let found = (entry.id, entry.room.clone());
            registry.publications.remove(fence);
            registry.release_room(&fence.room_id);
            found
        };
        Some(Box::new(move || {
            room.call(move |app, _, conns| {
                app.close_publication(id, conns);
                Ok(())
            })
        }))
    }

    fn voice(&self) -> Option<&dyn VoiceRuntime> {
        Some(self)
    }
}
