//! Relay volunteering: a Viewer offers to forward media to other Viewers.

use serde_json::Value;

use crate::codec::{Int, Obj, Result, ensure, in_range, object};
use crate::constants::MAX_VIEWERS_PER_ROOM_LIMIT;

/// The body shared by client `set-relay-volunteer` and server
/// `relay-volunteer`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RelayVolunteer {
    pub enabled: bool,
    pub capacity: Int,
}

impl RelayVolunteer {
    pub(crate) fn decode(value: &Value) -> Result<Self> {
        let f = object(value, &["type", "enabled", "capacity"])?;
        f.require(&["type", "enabled", "capacity"])?;
        let volunteer = Self {
            enabled: f.boolean("enabled")?,
            capacity: f.int("capacity")?,
        };
        ensure(
            in_range(volunteer.capacity, 0, MAX_VIEWERS_PER_ROOM_LIMIT as Int),
            "invalid relay capacity",
        )?;
        Ok(volunteer)
    }

    pub(crate) fn encode(&self, message_type: &str) -> Value {
        Obj::message(message_type)
            .put("enabled", &self.enabled)
            .put("capacity", &self.capacity)
            .build()
    }
}
