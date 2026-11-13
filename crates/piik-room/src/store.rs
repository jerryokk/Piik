//! Room authority, code allocation and participant membership.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use piik_ordered::Map;
use piik_protocol::{CodeEntryPolicy, MAX_VIEWERS_PER_ROOM_LIMIT, Role, valid_viewer_grant};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::database::{Database, StoredRoomAuthority};
use crate::error::{Error, ErrorCode, invalid};
use crate::password::{DerivedMaterial, PasswordDeriver, SharedDeriver};

/// The number of four-digit room codes without a leading zero.
pub const CAPACITY: usize = 9_000;

const ROOM_CODE_FIRST: usize = 1_000;

/// Returns `size` random bytes. It runs under the caller's lock and on
/// derivation threads, so it must be thread-safe.
pub type RandomSource = Arc<dyn Fn(usize) -> Vec<u8> + Send + Sync>;

/// Returns the current Unix time in milliseconds.
pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AdmittedBy {
    Grant,
    Code,
}

#[derive(Debug, Clone)]
pub(crate) struct Participant {
    client_id: String,
    peer_id: String,
    /// `None` means offline.
    session_id: Option<String>,
    admitted_by: AdmittedBy,
}

/// One room's authority plus its participants.
pub(crate) struct Room {
    room_id: String,
    /// Unique per store and never reused, so a [`RoomRef`] taken before
    /// unlocked work cannot match a replacement that reuses the code.
    incarnation: u64,
    host_token_digest: Vec<u8>,
    viewer_grant_digest: Vec<u8>,
    viewer_password_material: Option<Vec<u8>>,
    viewer_authorization_generation: String,
    code_entry_policy: CodeEntryPolicy,
    host: Option<Participant>,
    /// Process-local: restored rooms receive a fresh reconnect window. Only
    /// the last authenticated departure updates this; failed admission cannot
    /// renew it.
    empty_since_ms: i64,
    /// Insertion order decides viewer presence order, the closed-session list
    /// and the router's participant order.
    viewers: Map<String, Participant>,
}

/// Identifies one room incarnation across unlocked password derivation. It
/// grants no authority by itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomRef {
    room_id: String,
    incarnation: u64,
}

/// Newly issued Host authority and the room's Viewer grant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedRoom {
    pub room_id: String,
    pub host_token: String,
    pub code_entry_policy: CodeEntryPolicy,
    pub viewer_grant: String,
}

/// Uses `token` for a Host and `viewer_grant` for a Viewer. Empty credentials
/// are absent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectParticipantInput {
    pub room_id: String,
    pub role: Role,
    pub token: String,
    pub viewer_grant: String,
    pub client_id: String,
    pub session_id: String,
}

/// Viewer password admission. `password` is consumed by the caller's
/// [`PasswordDeriver::derive_viewer_password`] call, not by the commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectViewerWithPasswordInput {
    pub room_id: String,
    pub password: String,
    pub client_id: String,
    pub session_id: String,
}

/// Admission and any connected session it superseded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectedParticipant {
    pub room_id: String,
    pub role: Role,
    pub peer_id: String,
    pub host_online: bool,
    pub replaced_session_id: Option<String>,
    pub code_entry_policy: CodeEntryPolicy,
    pub viewer_password_enabled: bool,
    pub viewer_authorization_generation: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisconnectedParticipant {
    pub room_id: String,
    pub role: Role,
    pub peer_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectedPeer {
    pub peer_id: String,
    pub session_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClosedRoom {
    pub room_id: String,
    pub session_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplacedRoom {
    pub created: CreatedRoom,
    pub closed: ClosedRoom,
}

/// Revoked admission; `session_id` is `None` for an offline Viewer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevokedViewer {
    pub peer_id: String,
    pub session_id: Option<String>,
}

/// Rotates or revokes the room's Viewer invitation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewerGrantAction {
    Rotate,
    Revoke,
}

/// A grant change and the affected Viewers; `viewer_grant` is `None` when the
/// invitation was revoked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewerGrantUpdate {
    pub viewer_grant: Option<String>,
    pub viewer_authorization_generation: String,
    pub previous_viewer_authorization_generation: String,
    pub revoked_viewers: Vec<RevokedViewer>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodeEntryUpdate {
    pub code_entry_policy: CodeEntryPolicy,
    pub viewer_password_enabled: bool,
}

/// Capacity, optional persistence and injectable entropy/time.
#[derive(Default)]
pub struct Options {
    pub max_rooms: usize,
    pub max_viewers_per_room: usize,
    pub database: Option<Database>,
    pub random: Option<RandomSource>,
    pub now: Option<Clock>,
}

/// Owns room authority. See the crate documentation for the locking contract.
pub struct Store {
    /// Insertion order determines the order returned by
    /// [`Store::abandon_all_rooms`].
    rooms: Map<String, Room>,
    pub(crate) free_room_codes: Vec<String>,
    random: RandomSource,
    now: Clock,
    pub(crate) database: Option<Database>,
    initialized: bool,
    max_rooms: usize,
    max_viewers_per_room: usize,
    next_incarnation: u64,
    /// Bounds derivation work across every room in this store.
    deriver: SharedDeriver,
}

mod admission;
mod credentials;
mod participants;
mod rooms;

impl Store {
    /// Validates limits and prepares a store; [`Store::initialize`] loads
    /// persisted authority.
    pub fn new(options: Options) -> Result<Self, Error> {
        if options.max_rooms == 0 || options.max_rooms > CAPACITY {
            return Err(invalid(format!(
                "Room limit must be an integer between 1 and {CAPACITY}"
            )));
        }
        if options.max_viewers_per_room == 0
            || options.max_viewers_per_room > MAX_VIEWERS_PER_ROOM_LIMIT
        {
            return Err(invalid(format!(
                "Room viewer limit must be an integer between 1 and {MAX_VIEWERS_PER_ROOM_LIMIT}"
            )));
        }
        let random = options.random.unwrap_or_else(|| Arc::new(system_random));
        let now = options.now.unwrap_or_else(|| Arc::new(unix_millis));
        Ok(Self {
            rooms: Map::new(),
            free_room_codes: (0..CAPACITY)
                .map(|index| (ROOM_CODE_FIRST + index).to_string())
                .collect(),
            initialized: options.database.is_none(),
            database: options.database,
            deriver: Arc::new(PasswordDeriver::new(random.clone())),
            random,
            now,
            max_rooms: options.max_rooms,
            max_viewers_per_room: options.max_viewers_per_room,
            next_incarnation: 0,
        })
    }

    /// The shared password deriver. Clone the handle under the lock, then
    /// derive after releasing it.
    pub fn password_deriver(&self) -> Arc<PasswordDeriver> {
        self.deriver.clone()
    }

    pub fn max_viewers_per_room(&self) -> usize {
        self.max_viewers_per_room
    }

    /// The number of rooms.
    pub fn size(&self) -> usize {
        self.rooms.len()
    }

    /// Closes the database.
    pub fn close(&mut self) -> Result<(), Error> {
        match &mut self.database {
            Some(database) => database.close(),
            None => Ok(()),
        }
    }

    /// Recovers the stable authority. It is a no-op without a database.
    pub fn initialize(&mut self) -> Result<(), Error> {
        if self.initialized {
            return Ok(());
        }
        if self.database.is_none() {
            self.initialized = true;
            return Ok(());
        }
        if let Err(error) = self.restore() {
            self.rooms.clear();
            if let Some(database) = &mut self.database {
                let _ = database.close();
            }
            return Err(error);
        }
        self.initialized = true;
        Ok(())
    }

    fn restore(&mut self) -> Result<(), Error> {
        let Some(database) = &mut self.database else {
            return Ok(());
        };
        let stored_rooms = database.initialize()?;
        if stored_rooms.len() > self.max_rooms {
            return Err(invalid("Room database exceeds the configured room limit"));
        }
        let room_ids: HashSet<String> = stored_rooms
            .iter()
            .map(|stored| stored.room_id.clone())
            .collect();
        if room_ids.len() != stored_rooms.len() {
            return Err(invalid("Room database contains duplicate room IDs"));
        }
        for stored in stored_rooms {
            let incarnation = self.take_incarnation();
            self.rooms.set(
                stored.room_id.clone(),
                Room {
                    room_id: stored.room_id,
                    incarnation,
                    host_token_digest: stored.host_token_digest,
                    viewer_grant_digest: stored.viewer_grant_digest,
                    viewer_password_material: stored.viewer_password_material,
                    viewer_authorization_generation: stored.viewer_authorization_generation,
                    code_entry_policy: stored.code_entry_policy,
                    host: None,
                    empty_since_ms: (self.now)(),
                    viewers: Map::new(),
                },
            );
        }
        self.free_room_codes.retain(|code| !room_ids.contains(code));
        Ok(())
    }

    /// Commits a password change and reports whether a password is now set.
    fn host_managed(&self, room_id: &str, host_token: &str) -> Result<&Room, Error> {
        let room = self
            .rooms
            .get(&room_id.to_owned())
            .ok_or(Error::Room(ErrorCode::InvalidToken))?;
        if !verify_digest(host_token, &room.host_token_digest) {
            return Err(ErrorCode::InvalidToken.into());
        }
        Ok(room)
    }

    pub(super) fn take_room_code(
        &mut self,
        preferred_room_id: Option<&str>,
    ) -> Result<String, Error> {
        if self.free_room_codes.is_empty() {
            return Err(ErrorCode::RoomLimit.into());
        }
        if let Some(preferred) = preferred_room_id.filter(|preferred| !preferred.is_empty())
            && let Some(index) = self
                .free_room_codes
                .iter()
                .position(|code| code == preferred)
        {
            return Ok(self.free_room_codes.swap_remove(index));
        }
        let index = uniform_index(self.free_room_codes.len(), &*self.random)?;
        // swap_remove moves the last code into the taken slot.
        Ok(self.free_room_codes.swap_remove(index))
    }

    pub(super) fn release_room_code(&mut self, room_id: String) {
        self.free_room_codes.push(room_id);
    }

    pub(super) fn take_incarnation(&mut self) -> u64 {
        let incarnation = self.next_incarnation;
        self.next_incarnation += 1;
        incarnation
    }

    pub(super) fn new_room(
        &mut self,
        room_id: &str,
        policy: CodeEntryPolicy,
        material: Option<Vec<u8>>,
    ) -> Result<(Room, CreatedRoom), Error> {
        let host_token_bytes = (self.random)(32);
        if host_token_bytes.len() != 32 {
            return Err(invalid("Host token random source must return 32 bytes"));
        }
        let host_token = URL_SAFE_NO_PAD.encode(host_token_bytes);
        let viewer_grant = create_viewer_grant(&self.random)?;
        let generation = new_authorization_generation(&self.random)?;
        let room = Room {
            room_id: room_id.to_owned(),
            incarnation: self.take_incarnation(),
            host_token_digest: digest(&host_token),
            viewer_grant_digest: digest(&viewer_grant),
            viewer_password_material: material,
            viewer_authorization_generation: generation,
            code_entry_policy: policy,
            host: None,
            empty_since_ms: (self.now)(),
            viewers: Map::new(),
        };
        Ok((
            room,
            CreatedRoom {
                room_id: room_id.to_owned(),
                host_token,
                code_entry_policy: policy,
                viewer_grant,
            },
        ))
    }

    fn ensure_initialized(&self) -> Result<(), Error> {
        if !self.initialized {
            return Err(invalid("RoomStore stable authority is not initialized"));
        }
        Ok(())
    }
}

/// Separates material from a gate outcome, returning other derivation
/// failures immediately.
pub(crate) fn split_derived(
    derived: DerivedMaterial,
) -> Result<(Option<Vec<u8>>, Option<Error>), Error> {
    match derived {
        Ok(material) => Ok((material, None)),
        Err(error) if error.is_gate_outcome() => Ok((None, Some(error))),
        Err(error) => Err(error),
    }
}

/// The post-derivation checks for Host changes: identity together with a
/// cancelled derivation, then busy.
pub(crate) fn check_identity_and_gate(
    owned: &Room,
    current: &RoomRef,
    gate: Option<Error>,
) -> Result<(), Error> {
    if owned.incarnation != current.incarnation || matches!(gate, Some(Error::DerivationCancelled))
    {
        return Err(ErrorCode::RoomAccessDenied.into());
    }
    if gate.is_some() {
        return Err(ErrorCode::RoomBusy.into());
    }
    Ok(())
}

pub(crate) fn connect_host(
    room: &mut Room,
    input: &ConnectParticipantInput,
    random: &RandomSource,
) -> Result<ConnectedParticipant, Error> {
    if let Some(current) = &room.host
        && current.session_id.is_some()
        && current.client_id != input.client_id
    {
        return Err(ErrorCode::HostAlreadyConnected.into());
    }
    let returning = room
        .host
        .as_ref()
        .is_some_and(|current| current.client_id == input.client_id);
    if !returning {
        let peer_id = new_peer_id(room, random)?;
        room.host = Some(Participant {
            client_id: input.client_id.clone(),
            peer_id,
            session_id: None,
            admitted_by: AdmittedBy::Code,
        });
    }
    let host = room
        .host
        .as_mut()
        .ok_or_else(|| invalid("Host is missing"))?;
    let replaced_session_id = host.session_id.replace(input.session_id.clone());
    let peer_id = host.peer_id.clone();
    Ok(connected_participant(
        room,
        peer_id,
        Role::Host,
        replaced_session_id,
        &input.session_id,
    ))
}

pub(crate) fn connect_viewer(
    room: &mut Room,
    client_id: &str,
    session_id: &str,
    admitted_by: AdmittedBy,
    max_viewers_per_room: usize,
    random: &RandomSource,
) -> Result<ConnectedParticipant, Error> {
    let key = client_id.to_owned();
    if !room.viewers.has(&key) {
        if room.viewers.len() >= max_viewers_per_room {
            return Err(ErrorCode::RoomFull.into());
        }
        let peer_id = new_peer_id(room, random)?;
        room.viewers.set(
            key.clone(),
            Participant {
                client_id: key.clone(),
                peer_id,
                session_id: None,
                admitted_by,
            },
        );
    }
    // An existing key keeps its insertion position.
    let viewer = room
        .viewers
        .get_mut(&key)
        .ok_or_else(|| invalid("Viewer is missing"))?;
    let replaced_session_id = viewer.session_id.replace(session_id.to_owned());
    viewer.admitted_by = admitted_by;
    let peer_id = viewer.peer_id.clone();
    Ok(connected_participant(
        room,
        peer_id,
        Role::Viewer,
        replaced_session_id,
        session_id,
    ))
}

fn connected_participant(
    room: &Room,
    peer_id: String,
    role: Role,
    replaced_session_id: Option<String>,
    session_id: &str,
) -> ConnectedParticipant {
    ConnectedParticipant {
        room_id: room.room_id.clone(),
        role,
        peer_id,
        host_online: room
            .host
            .as_ref()
            .is_some_and(|host| host.session_id.is_some()),
        replaced_session_id: replaced_session_id.filter(|replaced| replaced != session_id),
        code_entry_policy: room.code_entry_policy,
        viewer_password_enabled: room.viewer_password_material.is_some(),
        viewer_authorization_generation: room.viewer_authorization_generation.clone(),
    }
}

fn new_peer_id(room: &Room, random: &RandomSource) -> Result<String, Error> {
    unique_id(random, 16, |candidate| {
        room.host
            .as_ref()
            .is_some_and(|host| host.peer_id == candidate)
            || room
                .viewers
                .iter()
                .any(|(_, viewer)| viewer.peer_id == candidate)
    })
}

fn unique_id(
    random: &RandomSource,
    size: usize,
    exists: impl Fn(&str) -> bool,
) -> Result<String, Error> {
    for _ in 0..8 {
        let value = random(size);
        if value.len() != size {
            return Err(invalid(format!(
                "Identifier random source must return {size} bytes"
            )));
        }
        let candidate = URL_SAFE_NO_PAD.encode(value);
        if !exists(&candidate) {
            return Ok(candidate);
        }
    }
    Err(invalid("Unable to allocate a unique identifier"))
}

/// Draws a uniform index below `count` from 8 random bytes, rejecting the
/// incomplete final bucket of the 2^64 possible values.
/// `(u64::MAX % count + 1) % count` computes 2^64 % count without overflow.
pub(crate) fn uniform_index(
    count: usize,
    random: &dyn Fn(usize) -> Vec<u8>,
) -> Result<usize, Error> {
    let span = count as u64;
    let remainder = (u64::MAX % span + 1) % span;
    for _ in 0..16 {
        let value = random(8);
        let Ok(bytes) = <[u8; 8]>::try_from(value.as_slice()) else {
            return Err(invalid("Room code random source must return 8 bytes"));
        };
        let value = u64::from_be_bytes(bytes);
        if remainder != 0 && value > u64::MAX - remainder {
            continue;
        }
        return Ok((value % span) as usize);
    }
    Err(invalid("Unable to select a room code uniformly"))
}

fn viewer_grant_is_valid(room: &Room, grant: &str) -> bool {
    !grant.is_empty()
        && valid_viewer_grant(grant)
        && verify_digest(grant, &room.viewer_grant_digest)
}

pub(crate) fn create_viewer_grant(random: &RandomSource) -> Result<String, Error> {
    let secret = random(16);
    if secret.len() != 16 {
        return Err(invalid("Viewer grant random source must return 16 bytes"));
    }
    Ok(URL_SAFE_NO_PAD.encode(secret))
}

pub(crate) fn random_digest(random: &RandomSource) -> Result<Vec<u8>, Error> {
    let value = random(32);
    if value.len() != 32 {
        return Err(invalid("Viewer grant random source must return 32 bytes"));
    }
    Ok(value)
}

pub(crate) fn new_authorization_generation(random: &RandomSource) -> Result<String, Error> {
    let value = random(16);
    if value.len() != 16 {
        return Err(invalid(
            "Authorization generation random source must return 16 bytes",
        ));
    }
    Ok(URL_SAFE_NO_PAD.encode(value))
}

pub(crate) fn room_ref(room: &Room) -> RoomRef {
    RoomRef {
        room_id: room.room_id.clone(),
        incarnation: room.incarnation,
    }
}

fn stored_room_authority(room: &Room) -> StoredRoomAuthority {
    StoredRoomAuthority {
        room_id: room.room_id.clone(),
        host_token_digest: room.host_token_digest.clone(),
        viewer_grant_digest: room.viewer_grant_digest.clone(),
        viewer_authorization_generation: room.viewer_authorization_generation.clone(),
        code_entry_policy: room.code_entry_policy,
        viewer_password_material: room.viewer_password_material.clone(),
    }
}

pub(crate) fn room_occupied(room: &Room) -> bool {
    room.host
        .as_ref()
        .is_some_and(|host| host.session_id.is_some())
        || room
            .viewers
            .iter()
            .any(|(_, viewer)| viewer.session_id.is_some())
}

pub(crate) fn find_viewer_by_peer_id_mut<'a>(
    room: &'a mut Room,
    peer_id: &str,
) -> Option<&'a mut Participant> {
    let client_id = room
        .viewers
        .iter()
        .find(|(_, viewer)| viewer.peer_id == peer_id)
        .map(|(client_id, _)| client_id.clone())?;
    room.viewers.get_mut(&client_id)
}

fn connected_session_ids(room: &Room) -> Vec<String> {
    room.host
        .iter()
        .chain(room.viewers.iter().map(|(_, viewer)| viewer))
        .filter_map(|participant| participant.session_id.clone())
        .collect()
}

pub(crate) fn digest(value: &str) -> Vec<u8> {
    Sha256::digest(value.as_bytes()).to_vec()
}

pub(crate) fn verify_digest(value: &str, expected_digest: &[u8]) -> bool {
    bool::from(digest(value).ct_eq(expected_digest))
}

fn system_random(size: usize) -> Vec<u8> {
    use rand::TryRng;
    let mut value = vec![0; size];
    // Like crypto/rand, an operating-system entropy failure is fatal.
    rand::rngs::SysRng
        .try_fill_bytes(&mut value)
        .expect("operating-system random source failed");
    value
}

fn unix_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as i64)
}
