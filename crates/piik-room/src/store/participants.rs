//! Connected-peer queries and room lifecycle (abandon and expiry).

use crate::error::Error;
use crate::store::{ClosedRoom, ConnectedPeer, Store, connected_session_ids, room_occupied};

impl Store {
    pub fn get_connected_host(&self, room_id: &str) -> Option<ConnectedPeer> {
        let host = self.rooms.get(&room_id.to_owned())?.host.as_ref()?;
        Some(ConnectedPeer {
            peer_id: host.peer_id.clone(),
            session_id: host.session_id.clone()?,
        })
    }

    /// Returns only a Viewer with a current session.
    pub fn get_connected_viewer(&self, room_id: &str, peer_id: &str) -> Option<ConnectedPeer> {
        let room = self.rooms.get(&room_id.to_owned())?;
        let (_, viewer) = room
            .viewers
            .iter()
            .find(|(_, viewer)| viewer.peer_id == peer_id)?;
        Some(ConnectedPeer {
            peer_id: peer_id.to_owned(),
            session_id: viewer.session_id.clone()?,
        })
    }

    /// Viewer peer IDs in insertion order.
    pub fn get_viewer_peer_ids(&self, room_id: &str) -> Vec<String> {
        self.rooms
            .get(&room_id.to_owned())
            .map(|room| {
                room.viewers
                    .iter()
                    .map(|(_, viewer)| viewer.peer_id.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Connected Viewers in insertion order.
    pub fn get_connected_viewers(&self, room_id: &str) -> Vec<ConnectedPeer> {
        self.rooms
            .get(&room_id.to_owned())
            .map(|room| {
                room.viewers
                    .iter()
                    .filter_map(|(_, viewer)| {
                        Some(ConnectedPeer {
                            peer_id: viewer.peer_id.clone(),
                            session_id: viewer.session_id.clone()?,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Retires room authority; `None` if the room is already absent.
    pub fn abandon_room(&mut self, room_id: &str) -> Result<Option<ClosedRoom>, Error> {
        Ok(self
            .abandon_rooms(&[room_id.to_owned()])?
            .into_iter()
            .next())
    }

    /// Retires every room, in room insertion order.
    pub fn abandon_all_rooms(&mut self) -> Result<Vec<ClosedRoom>, Error> {
        let room_ids = self.rooms.keys();
        self.abandon_rooms(&room_ids)
    }

    /// Retires a batch atomically before releasing codes. One SQLite
    /// transaction avoids thousands of fsyncs under the signaling lock after a
    /// restart.
    pub(crate) fn abandon_rooms(&mut self, room_ids: &[String]) -> Result<Vec<ClosedRoom>, Error> {
        self.ensure_initialized()?;
        let mut authorities = Vec::new();
        let mut closed = Vec::new();
        for room_id in room_ids {
            if let Some(room) = self.rooms.get(room_id) {
                authorities.push((room_id.clone(), room.host_token_digest.clone()));
                closed.push(ClosedRoom {
                    room_id: room_id.clone(),
                    session_ids: connected_session_ids(room),
                });
            }
        }
        if closed.is_empty() {
            return Ok(Vec::new());
        }
        if let Some(database) = &self.database {
            database.delete_rooms(&authorities)?;
        }
        for room in &closed {
            self.rooms.delete(&room.room_id);
            self.release_room_code(room.room_id.clone());
        }
        Ok(closed)
    }

    /// Empty rooms in creation order, including never-joined rooms. A
    /// connected Host or Viewer protects an idle chat room just like a share.
    /// Callers retire candidates synchronously under the same signaling lock.
    pub fn empty_room_ids(&self, before_ms: i64) -> Vec<String> {
        self.rooms
            .iter()
            .filter(|(_, room)| room.empty_since_ms <= before_ms && !room_occupied(room))
            .map(|(room_id, _)| room_id.clone())
            .collect()
    }

    /// Commits storage retirement before returning effect cleanup.
    pub fn expire_empty_rooms(&mut self, before_ms: i64) -> Result<Vec<ClosedRoom>, Error> {
        let room_ids = self.empty_room_ids(before_ms);
        self.abandon_rooms(&room_ids)
    }
}
