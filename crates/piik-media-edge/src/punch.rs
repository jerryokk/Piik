//! One optional NAT punch preparation per Engine. It owns no media, persists
//! no address and expires after five seconds even if SDP never follows.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use bytecodec::{DecodeExt, EncodeExt};
use piik_media_transport::PunchedSocket;
use serde::{Deserialize, Serialize};
use stun_codec::rfc5389::attributes::{Fingerprint, MessageIntegrity, XorMappedAddress};
use stun_codec::rfc5389::{Attribute, methods::BINDING};
use stun_codec::{Message, MessageClass, MessageDecoder, MessageEncoder, TransactionId};
use tokio::net::UdpSocket as TokioUdpSocket;
use tokio_util::sync::CancellationToken;

use crate::engine::Engine;
use crate::error::{Error, Result};
use crate::gather::{IceServerUrls, stun_urls};
use crate::source::VideoSource;

const PREPARATION_LIFETIME: Duration = Duration::from_secs(5);
const SURVEY_TIMEOUT: Duration = Duration::from_millis(1500);
const ARRAY_BUDGET: Duration = Duration::from_millis(1100);
const ARRAY_SOCKETS: usize = 8;
const ARRAY_REQUESTS: usize = 64;
const ARRAY_ROUNDS: usize = 8;
const ARRAY_ROUND: Duration = Duration::from_millis(100);
const STUN_RTO: Duration = Duration::from_millis(500);

/// A transient mapping observation, not a NAT classification. Only an
/// authenticated peer in the current route may supply it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PunchPrediction {
    pub address: String,
    pub port: i64,
    pub step: i64,
}

fn public_unicast(address: Ipv4Addr) -> bool {
    !(address.is_private()
        || address.is_loopback()
        || address.is_link_local()
        || address.is_unspecified()
        || address.is_broadcast()
        || address.is_multicast()
        || address.octets()[0] >= 240
        || address.octets()[0] == 0)
}

impl PunchPrediction {
    /// A public IPv4 unicast address, port 1024..=65535, step 0..=16,
    /// predicted four allocations ahead.
    pub fn target(&self) -> Result<SocketAddr> {
        self.target_with(false)
    }

    /// `private` admits non-public addresses (loopback test fixtures).
    fn target_with(&self, private: bool) -> Result<SocketAddr> {
        let invalid = || Error::Invalid("unsupported peer mapping observation");
        let address: Ipv4Addr = self.address.parse().map_err(|_| invalid())?;
        if !(private || public_unicast(address))
            || !(0..=16).contains(&self.step)
            || !(1024..=65535).contains(&self.port)
        {
            return Err(invalid());
        }
        let port = u16::try_from(self.port + 4 * self.step).map_err(|_| invalid())?;
        Ok(SocketAddr::V4(SocketAddrV4::new(address, port)))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    New,
    Survey,
    Ready,
    Running,
    Complete,
    Closed,
}

struct Inner {
    connection_id: String,
    /// The source key and engine identity the preparation belongs to.
    source: Option<u64>,
    servers: Vec<IceServerUrls>,
    secret: String,
    cancel: CancellationToken,
    state: Mutex<PreparationState>,
    /// Held by the one running step (`prepare`/`run`).
    work: tokio::sync::Mutex<()>,
}

struct PreparationState {
    stage: Stage,
    socket: Option<UdpSocket>,
    remote: Option<SocketAddr>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

/// The Engine's one preparation slot.
#[derive(Default)]
pub(crate) struct PunchSlot(Mutex<Option<PunchPreparation>>);

/// One NAT punch preparation. Cheap to clone.
#[derive(Clone)]
pub struct PunchPreparation {
    engine: Engine,
    inner: Arc<Inner>,
}

impl std::fmt::Debug for PunchPreparation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PunchPreparation")
            .field("stage", &lock(&self.inner.state).stage)
            .finish_non_exhaustive()
    }
}

impl PunchPreparation {
    fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    fn transition(&self, from: Stage, to: Stage) -> bool {
        let mut state = lock(&self.inner.state);
        if self.inner.cancel.is_cancelled() || state.stage != from {
            return false;
        }
        state.stage = to;
        true
    }

    fn close(&self) {
        self.inner.cancel.cancel();
        let mut state = lock(&self.inner.state);
        state.stage = Stage::Closed;
        state.socket = None;
        state.remote = None;
    }

    /// Releases a step; a retired preparation frees its slot now.
    fn finish(&self, work: tokio::sync::MutexGuard<'_, ()>) {
        drop(work);
        if self.inner.cancel.is_cancelled() {
            self.engine.free_punch(self);
        }
    }

    /// Observes three destinations in order before any peer check. A stable
    /// mapping retains this exact socket.
    pub async fn prepare(&self) -> Result<PunchPrediction> {
        let Ok(work) = self.inner.work.try_lock() else {
            return Err(Error::Invalid("connection preparation is busy"));
        };
        let result = self.prepare_step().await;
        self.finish(work);
        result
    }

    async fn prepare_step(&self) -> Result<PunchPrediction> {
        if !self.transition(Stage::New, Stage::Survey) {
            return Err(Error::Invalid("connection preparation was retired"));
        }
        let survey = tokio::time::timeout(SURVEY_TIMEOUT, self.survey());
        let result = tokio::select! {
            () = self.inner.cancel.cancelled() => Err(Error::Invalid("connection preparation was retired")),
            result = survey => result.unwrap_or(Err(Error::Invalid("connection preparation discovery unavailable"))),
        };
        let (prediction, socket) = result?;
        let mut state = lock(&self.inner.state);
        if self.inner.cancel.is_cancelled() || state.stage != Stage::Survey {
            return Err(Error::Invalid("connection preparation was retired"));
        }
        state.stage = Stage::Ready;
        if prediction.step == 0 {
            // A stable observed endpoint is usable only with this socket.
            state.socket = Some(socket.into_std()?);
        }
        Ok(prediction)
    }

    async fn survey(&self) -> Result<(PunchPrediction, TokioUdpSocket)> {
        let socket = TokioUdpSocket::from_std(bind(self.engine.settings().bind)?)?;
        let mut seen = Vec::new();
        let mut observations: Vec<SocketAddr> = Vec::new();
        for server in stun_urls(&self.inner.servers) {
            let address = tokio::net::lookup_host((server.host.as_str(), server.port))
                .await
                .ok()
                .and_then(|mut addresses| addresses.find(SocketAddr::is_ipv4))
                .ok_or(Error::Invalid(
                    "connection preparation discovery unavailable",
                ))?;
            if seen.contains(&address) {
                continue;
            }
            seen.push(address);
            let mapped = query(&socket, address).await.ok_or(Error::Invalid(
                "connection preparation discovery unavailable",
            ))?;
            observations.push(mapped);
            if let [first, second, third] = observations[..] {
                let (SocketAddr::V4(first), SocketAddr::V4(second), SocketAddr::V4(third)) =
                    (first, second, third)
                else {
                    return Err(Error::Invalid("no usable mapping observation"));
                };
                let step = i64::from(third.port()) - i64::from(second.port());
                let prediction = PunchPrediction {
                    address: third.ip().to_string(),
                    port: i64::from(third.port()),
                    step,
                };
                let private = self.engine.settings().allow_private_punch;
                if prediction.target_with(private).is_err()
                    || first.ip() != second.ip()
                    || second.ip() != third.ip()
                    || i64::from(second.port()) - i64::from(first.port()) != step
                {
                    return Err(Error::Invalid("no usable mapping observation"));
                }
                return Ok((prediction, socket));
            }
        }
        Err(Error::Invalid(
            "three distinct discovery destinations are required",
        ))
    }

    /// A bounded authenticated preflight to the peer's predicted tuple.
    /// Returns whether a socket proved it.
    pub async fn run(&self, remote: PunchPrediction) -> bool {
        let Ok(work) = self.inner.work.try_lock() else {
            return false;
        };
        let proved = self.run_step(remote).await;
        self.finish(work);
        proved
    }

    async fn run_step(&self, remote: PunchPrediction) -> bool {
        let Ok(target) = remote.target_with(self.engine.settings().allow_private_punch) else {
            return false;
        };
        if !self.transition(Stage::Ready, Stage::Running) {
            return false;
        }
        let retained = lock(&self.inner.state).socket.take();
        let bind_address = self.engine.settings().bind;
        let winner = tokio::select! {
            () = self.inner.cancel.cancelled() => None,
            winner = punch_socket_array(bind_address, target, &self.inner.secret, retained) => winner.ok(),
        };
        let mut state = lock(&self.inner.state);
        let Some(winner) = winner else {
            return false;
        };
        if self.inner.cancel.is_cancelled() || state.stage != Stage::Running {
            return false;
        }
        state.socket = Some(winner);
        state.remote = Some(target);
        state.stage = Stage::Complete;
        true
    }
}

fn bind(address: SocketAddrV4) -> Result<UdpSocket> {
    let socket = UdpSocket::bind(address)?;
    socket.set_nonblocking(true)?;
    Ok(socket)
}

/// One unauthenticated Binding transaction with RFC 5389 retransmission.
async fn query(socket: &TokioUdpSocket, server: SocketAddr) -> Option<SocketAddr> {
    let id: [u8; 12] = rand::random();
    let request = Message::<Attribute>::new(MessageClass::Request, BINDING, TransactionId::new(id));
    let bytes = MessageEncoder::new().encode_into_bytes(request).ok()?;
    let mut buffer = [0u8; 1500];
    let mut rto = STUN_RTO;
    loop {
        socket.send_to(&bytes, server).await.ok()?;
        let deadline = tokio::time::Instant::now() + rto;
        loop {
            let received = tokio::time::timeout_at(deadline, socket.recv_from(&mut buffer)).await;
            let Ok(Ok((size, sender))) = received else {
                break;
            };
            if sender != server {
                continue;
            }
            let Ok(Ok(message)) =
                MessageDecoder::<Attribute>::new().decode_from_bytes(&buffer[..size])
            else {
                continue;
            };
            if message.transaction_id().as_bytes() != &id
                || message.class() != MessageClass::SuccessResponse
            {
                continue;
            }
            return message
                .get_attribute::<XorMappedAddress>()
                .map(XorMappedAddress::address);
        }
        rto *= 2;
    }
}

/// An authenticated Binding message with short-term integrity keyed by the
/// generation secret's hex text, plus a fingerprint.
fn authenticated(
    class: MessageClass,
    id: TransactionId,
    password: &str,
    mapped: Option<SocketAddr>,
) -> Option<Vec<u8>> {
    let mut message = Message::<Attribute>::new(class, BINDING, id);
    if let Some(mapped) = mapped {
        message.add_attribute(Attribute::XorMappedAddress(XorMappedAddress::new(mapped)));
    }
    let integrity = MessageIntegrity::new_short_term_credential(&message, password).ok()?;
    message.add_attribute(Attribute::MessageIntegrity(integrity));
    let fingerprint = Fingerprint::new(&message).ok()?;
    message.add_attribute(Attribute::Fingerprint(fingerprint));
    MessageEncoder::new().encode_into_bytes(message).ok()
}

fn verified(contents: &[u8], password: &str) -> Option<Message<Attribute>> {
    let message = MessageDecoder::<Attribute>::new()
        .decode_from_bytes(contents)
        .ok()?
        .ok()?;
    message.get_attribute::<Fingerprint>()?;
    message
        .get_attribute::<MessageIntegrity>()?
        .check_short_term_credential(password)
        .ok()?;
    Some(message)
}

/// Eight fresh sockets, or the one retained stable observation socket,
/// within one 1.1-second budget and 64 shared requests.
/// Every socket but the winner closes before ownership transfers.
pub(crate) async fn punch_socket_array(
    bind_address: SocketAddrV4,
    remote: SocketAddr,
    secret: &str,
    retained: Option<UdpSocket>,
) -> Result<UdpSocket> {
    if !remote.is_ipv4() || remote.port() == 0 || secret.len() != 64 {
        return Err(Error::Invalid("invalid peer or generation secret"));
    }
    let mut sockets = Vec::new();
    match retained {
        Some(socket) => sockets.push(TokioUdpSocket::from_std(socket)?),
        None => {
            for _ in 0..ARRAY_SOCKETS {
                sockets.push(TokioUdpSocket::from_std(bind(bind_address)?)?);
            }
        }
    }
    let ids: Vec<TransactionId> = sockets
        .iter()
        .map(|_| TransactionId::new(rand::random()))
        .collect();
    let requests: Vec<Vec<u8>> = ids
        .iter()
        .map(|id| authenticated(MessageClass::Request, *id, secret, None))
        .collect::<Option<_>>()
        .ok_or(Error::Invalid("invalid peer or generation secret"))?;
    let mut left = ARRAY_REQUESTS;
    let mut responses = 0usize;
    let mut proved: Option<usize> = None;
    let deadline = tokio::time::Instant::now() + ARRAY_BUDGET;
    let reserve = sockets.len();
    let mut round = 0;
    let mut next_round = tokio::time::Instant::now();
    let mut buffers = vec![[0u8; 1024]; sockets.len()];
    loop {
        if round < ARRAY_ROUNDS && tokio::time::Instant::now() >= next_round {
            // Keep one check per socket for peer-triggered work.
            for (index, socket) in sockets.iter().enumerate() {
                if left > reserve {
                    left -= 1;
                    let _ = socket.try_send_to(&requests[index], remote);
                }
            }
            round += 1;
            next_round += ARRAY_ROUND;
        }
        let wake = if round < ARRAY_ROUNDS {
            next_round.min(deadline)
        } else {
            deadline
        };
        let received = {
            let futures = sockets.iter().zip(buffers.iter_mut()).enumerate().map(
                |(index, (socket, buffer))| {
                    Box::pin(async move {
                        let result = socket.recv_from(buffer).await;
                        (index, result)
                    })
                },
            );
            let all = futures_select(futures.collect());
            tokio::time::timeout_at(wake, all).await
        };
        if tokio::time::Instant::now() >= deadline {
            break;
        }
        let Ok((index, Ok((size, sender)))) = received else {
            continue;
        };
        if sender != remote {
            continue;
        }
        let Some(message) = verified(&buffers[index][..size], secret) else {
            continue;
        };
        match message.class() {
            MessageClass::Request => {
                responses += 1;
                if responses > ARRAY_REQUESTS {
                    continue;
                }
                if let Some(reply) = authenticated(
                    MessageClass::SuccessResponse,
                    message.transaction_id(),
                    secret,
                    Some(remote),
                ) {
                    let _ = sockets[index].try_send_to(&reply, remote);
                }
                // The peer may start after our fanout: check the tuple back
                // through this same socket and budget.
                if proved.is_none() && left > 0 {
                    left -= 1;
                    let _ = sockets[index].try_send_to(&requests[index], remote);
                }
            }
            MessageClass::SuccessResponse if message.transaction_id() == ids[index] => {
                proved.get_or_insert(index);
            }
            _ => {}
        }
    }
    // The receive budget outlives the fanout so delayed starts can finish.
    let winner = proved.ok_or(Error::Invalid(
        "bounded socket array found no authenticated tuple",
    ))?;
    Ok(sockets.swap_remove(winner).into_std()?)
}

/// The first ready receive of a set of futures.
async fn futures_select<F, T>(mut futures: Vec<std::pin::Pin<Box<F>>>) -> T
where
    F: Future<Output = T>,
{
    std::future::poll_fn(|context| {
        for future in &mut futures {
            if let std::task::Poll::Ready(value) = future.as_mut().poll(context) {
                return std::task::Poll::Ready(value);
            }
        }
        std::task::Poll::Pending
    })
    .await
}

impl Engine {
    /// Begins this Engine's punch preparation. `secret` is the 32-byte
    /// generation secret in hex.
    pub fn begin_punch(
        &self,
        connection_id: &str,
        source: Option<&VideoSource>,
        servers: Vec<IceServerUrls>,
        secret: &str,
    ) -> Result<PunchPreparation> {
        let valid = hex::decode(secret).is_ok_and(|key| key.len() == 32);
        if !valid || connection_id.is_empty() || connection_id.len() > 256 {
            return Err(Error::Invalid("invalid connection preparation"));
        }
        if source.is_some_and(|source| !source.engine.same(self)) {
            return Err(Error::Invalid("media source is unavailable"));
        }
        let mut slot = lock(&self.inner.punch.0);
        if self.is_closed() || slot.is_some() {
            return Err(Error::Invalid("connection preparation is unavailable"));
        }
        let preparation = PunchPreparation {
            engine: self.clone(),
            inner: Arc::new(Inner {
                connection_id: connection_id.to_owned(),
                source: source.map(|source| source.key),
                servers,
                secret: secret.to_ascii_lowercase(),
                cancel: self.inner.cancel.child_token(),
                state: Mutex::new(PreparationState {
                    stage: Stage::New,
                    socket: None,
                    remote: None,
                }),
                work: tokio::sync::Mutex::new(()),
            }),
        };
        *slot = Some(preparation.clone());
        drop(slot);
        let expiring = preparation.clone();
        tokio::spawn(async move {
            tokio::select! {
                () = tokio::time::sleep(PREPARATION_LIFETIME) => {}
                () = expiring.inner.cancel.cancelled() => {}
            }
            expiring.engine.retire_punch(&expiring);
        });
        Ok(preparation)
    }

    /// Cancels `preparation`. The slot frees once no step still owns its
    /// sockets: a busy worker frees it on exit, so a replacement cannot
    /// overtake a retiring worker.
    fn retire_punch(&self, preparation: &PunchPreparation) {
        preparation.close();
        if preparation.inner.work.try_lock().is_err() {
            return;
        }
        self.free_punch(preparation);
    }

    fn free_punch(&self, preparation: &PunchPreparation) {
        let mut slot = lock(&self.inner.punch.0);
        if slot
            .as_ref()
            .is_some_and(|current| current.same(preparation))
        {
            *slot = None;
        }
    }

    /// The current preparation for `connection_id`.
    pub fn punch(&self, connection_id: &str) -> Option<PunchPreparation> {
        lock(&self.inner.punch.0)
            .as_ref()
            .filter(|preparation| preparation.inner.connection_id == connection_id)
            .cloned()
    }

    /// Transfers the proved socket once, after both endpoints reported
    /// success. The caller adopts it or drops it.
    pub fn take_punched_socket(
        &self,
        connection_id: &str,
        source: Option<&VideoSource>,
    ) -> Option<PunchedSocket> {
        let preparation = self.punch(connection_id)?;
        if preparation.inner.source != source.map(|source| source.key) {
            return None;
        }
        // A premature commit must not free the slot while work still owns
        // sockets.
        let work = preparation.inner.work.try_lock().ok()?;
        let taken = {
            let mut state = lock(&preparation.inner.state);
            if state.stage != Stage::Complete || preparation.inner.cancel.is_cancelled() {
                return None;
            }
            state.stage = Stage::Closed;
            state.socket.take().zip(state.remote.take())
        };
        drop(work);
        self.retire_punch(&preparation);
        taken.map(|(socket, remote)| PunchedSocket { socket, remote })
    }

    /// Retires the preparation for `connection_id`; an empty identity retires
    /// any preparation.
    pub fn close_punch(&self, connection_id: &str) {
        let preparation = lock(&self.inner.punch.0)
            .as_ref()
            .filter(|preparation| {
                connection_id.is_empty() || preparation.inner.connection_id == connection_id
            })
            .cloned();
        if let Some(preparation) = preparation {
            self.retire_punch(&preparation);
        }
    }

    pub(crate) fn close_source_punch(&self, source: u64) {
        let preparation = lock(&self.inner.punch.0)
            .as_ref()
            .filter(|preparation| preparation.inner.source == Some(source))
            .cloned();
        if let Some(preparation) = preparation {
            self.retire_punch(&preparation);
        }
    }
}

#[cfg(test)]
mod tests;
