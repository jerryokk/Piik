//! Platform-neutral gamepad reports, device ownership traits and the ViGEm
//! XUSB wire layout.

use serde::{Deserialize, Serialize};

use crate::Error;
use crate::event::InputEvent;
use crate::manager::{Inner, Key};

/// Bits above the 17 W3C standard buttons reject a report.
pub(crate) const GAMEPAD_BUTTON_MASK: u32 = (1 << 17) - 1;

/// Button bits follow W3C standard indices. Sticks use signed 16-bit values
/// with Y increasing downward; triggers use 0..=255. Native formats stay local.
/// Every field is required, as the Browser always sends a full state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct GamepadReport {
    pub buttons: u32,
    pub left_trigger: u8,
    pub right_trigger: u8,
    pub left_x: i16,
    pub left_y: i16,
    pub right_x: i16,
    pub right_y: i16,
}

pub(crate) fn gamepad_unavailable(detail: &str) -> Error {
    Error::input(format!("virtual gamepad output unavailable: {detail}"))
}

/// One virtual controller, owned by exactly one grant.
pub(crate) trait GamepadDevice: Send {
    fn update(&mut self, report: GamepadReport) -> Result<(), Error>;
    fn close(&mut self) -> Result<(), Error>;
}

/// A device factory. `open` may enumerate an OS device; it runs without the
/// input lock and must never replay a report from before enumeration.
pub(crate) trait GamepadBackend: Send + Sync {
    fn open(&self) -> Result<Box<dyn GamepadDevice>, Error>;
    fn close(&self) -> Result<(), Error>;
}

impl Inner {
    /// One creation may enumerate an OS device at a time. The driver assigns
    /// the free XInput index itself, so two simultaneous plugs can both see the
    /// same free slot and one target silently disappears. A report that arrives
    /// while another creation runs is dropped; full reports repeat at 60 Hz.
    /// It never holds the input lock and never replays a report from before
    /// enumeration.
    fn assign_gamepad(&mut self, key: &Key, index: u8) {
        if self.gamepad_opening.is_some() {
            return;
        }
        let Some(backend) = self.gamepads.clone() else {
            self.fail_gamepad(key, index);
            return;
        };
        let c = &self.connections[key];
        let (serial, grant, intent) = (
            c.serial,
            c.grant.clone(),
            c.pad_intent.get(&index).copied().unwrap_or_default(),
        );
        let shared = self.this.clone();
        let key = key.clone();
        self.gamepad_opening = Some(index);
        std::thread::spawn(move || {
            let opened = backend.open();
            let Some(shared) = shared.upgrade() else {
                if let Ok(mut pad) = opened {
                    let _ = pad.close();
                }
                return;
            };
            let stale = {
                let mut inner = shared.lock();
                let valid = !inner.closed
                    && !inner.session_closed(key.0)
                    && inner.available(key.0)
                    && inner.co_play(key.0)
                    && inner.connections.get(&key).is_some_and(|c| {
                        c.serial == serial
                            && !c.grant.is_empty()
                            && c.grant == grant
                            && c.pad_intent.get(&index).copied().unwrap_or_default() == intent
                            && !c.pad_failed.contains(&index)
                    });
                match (valid, opened) {
                    (true, Ok(pad)) => {
                        inner
                            .connections
                            .get_mut(&key)
                            .expect("valid connection")
                            .pads
                            .insert(index, pad);
                        inner.notify(key.0);
                        None
                    }
                    (true, Err(_)) => {
                        inner.fail_gamepad(&key, index);
                        None
                    }
                    (false, Ok(pad)) => Some(pad),
                    (false, Err(_)) => None,
                }
            };
            // A stale device is retired outside the input lock.
            if let Some(mut pad) = stale {
                let _ = pad.close();
            }
            shared.lock().gamepad_opening = None;
        });
    }

    pub fn fail_gamepad(&mut self, key: &Key, index: u8) {
        let Some(c) = self.connections.get_mut(key) else {
            return;
        };
        if let Some(mut pad) = c.pads.remove(&index) {
            let _ = pad.close();
        }
        c.pad_failed.insert(index);
        // The capability stays advertised: a refused Viewer can retry once the
        // host frees a slot, and that path only exists while it is offered.
        self.notify(key.0);
        // Losing a controller frees a slot someone else was waiting for. This
        // also answers the refusal, so it must not be followed by a second,
        // reason-less reply that would erase the explanation.
        self.announce_gamepad_room();
    }

    /// A refused controller is not a dead end: the slot count is a live fact,
    /// so a Viewer may retry once room appears. Clearing the refusal lets the
    /// next full report for that index try again instead of returning forever.
    pub fn retry_gamepad(&mut self, key: &Key) {
        let Some(c) = self.connections.get_mut(key) else {
            return;
        };
        if c.pad_failed.is_empty() {
            return;
        }
        c.pad_failed.clear();
        // Clearing a refusal is not a control decision: the grant, the source
        // and the mode are untouched, so report the state this connection is
        // actually in rather than implying it is merely ready to ask.
        let state = self.state(key);
        self.reply(key, state, "");
    }

    /// Returns every controller this connection owns to neutral without
    /// retiring it. A Viewer that cannot drive anything right now keeps its
    /// player slots, so resuming needs no new permission.
    pub fn neutralize(&mut self, key: &Key) {
        let failed: Vec<u8> = {
            let Some(c) = self.connections.get_mut(key) else {
                return;
            };
            let mut failed = Vec::new();
            for (index, pad) in c.pads.iter_mut() {
                if pad.update(GamepadReport::default()).is_err() {
                    failed.push(*index);
                }
            }
            failed
        };
        for index in failed {
            self.fail_gamepad(key, index);
        }
    }

    /// Retires every controller this connection owns, reporting whether any
    /// slot came free. Callers that do not already broadcast state owe the
    /// share an `announce_gamepad_room`, or a refused Viewer keeps seeing a
    /// full budget it could now use.
    pub fn close_pads(&mut self, key: &Key) -> bool {
        let Some(c) = self.connections.get_mut(key) else {
            return false;
        };
        if c.pads.is_empty() {
            return false;
        }
        for (_, mut pad) in std::mem::take(&mut c.pads) {
            let _ = pad.close();
        }
        true
    }

    /// Tells every share's Viewers that the controller budget changed, so a
    /// refused Viewer learns there is room instead of having to guess.
    pub fn announce_gamepad_room(&mut self) {
        let keys: Vec<Key> = self
            .connections
            .iter()
            .filter(|(key, c)| self.co_play(key.0) && !c.epoch.is_empty())
            .map(|(key, _)| key.clone())
            .collect();
        for key in keys {
            // This update is about the slot count, not about the refusal having
            // ended. Dropping the reason here would leave the Viewer with a
            // controller that silently does nothing.
            let reason = if self.connections[&key].pad_failed.is_empty() {
                ""
            } else {
                "gamepad"
            };
            let state = self.state(&key);
            self.reply(&key, state, reason);
        }
    }

    pub fn receive_gamepad(&mut self, key: &Key, event: &InputEvent) {
        // Gamepads exist only for a shared session with a backend.
        if !self.co_play(key.0) {
            return;
        }
        let index = event.pad;
        // Retrying is one decision about the whole grant, not about one index:
        // a Viewer may have several controllers and the Browser's retry carries
        // no address. Refusals bound to a different stack keep queueing reports
        // otherwise, so clear them all here.
        if event.kind == "gamepad-retry" {
            self.retry_gamepad(key);
            return;
        }
        // A reset is about the Viewer, not one controller, so it never carries
        // an index and must not be filtered by a refusal on index 0.
        if event.kind == "gamepad-reset" {
            self.neutralize(key);
            return;
        }
        let refused = self
            .connections
            .get(key)
            .is_some_and(|c| c.pad_failed.contains(&index));
        if refused {
            // A controller that left ends its own refusal: the Browser index
            // may carry a different device next, and that one may get a slot.
            if event.kind == "gamepad-stop"
                && self
                    .connections
                    .get_mut(key)
                    .is_some_and(|c| c.pad_failed.remove(&index))
            {
                let reason = if self.connections[key].pad_failed.is_empty() {
                    ""
                } else {
                    "gamepad"
                };
                let state = self.state(key);
                self.reply(key, state, reason);
            }
            // Ordinary reports stay ignored until the Viewer asks again, so a
            // refused pad cannot spin up attempts.
            return;
        }
        match event.kind.as_str() {
            "gamepad-stop" => {
                // Stopping a device also invalidates an enumeration in
                // progress. Focus resets keep the device and its player slot.
                let stopped = {
                    let Some(c) = self.connections.get_mut(key) else {
                        return;
                    };
                    *c.pad_intent.entry(index).or_default() += 1;
                    c.pads.remove(&index)
                };
                let Some(mut pad) = stopped else {
                    return;
                };
                let _ = pad.close();
                // A stopped device returns its slot to everyone sharing this
                // source, including Viewers the Host refused earlier.
                self.announce_gamepad_room();
            }
            "gamepad" => {
                let Some(report) = event
                    .gamepad
                    .filter(|r| r.buttons & !GAMEPAD_BUTTON_MASK == 0)
                else {
                    return;
                };
                if !self.refresh_active().1 {
                    return;
                }
                let Some(c) = self.connections.get_mut(key) else {
                    return;
                };
                match c.pads.get_mut(&index) {
                    None => self.assign_gamepad(key, index),
                    Some(pad) => {
                        if pad.update(report).is_err() {
                            self.fail_gamepad(key, index);
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// Native `XUSB_REPORT`, separate from the platform-neutral wire report.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct XusbReport {
    pub buttons: u16,
    pub left_trigger: u8,
    pub right_trigger: u8,
    pub left_x: i16,
    pub left_y: i16,
    pub right_x: i16,
    pub right_y: i16,
}

/// W3C standard button index to ViGEm `XUSB_BUTTON`. Browser trigger buttons
/// (6, 7) are analog fields in `XUSB_REPORT`; Guide uses the SDK's published
/// 0x0400 constant.
pub(crate) const XUSB_BUTTONS: [u16; 17] = [
    0x1000, 0x2000, 0x4000, 0x8000, 0x0100, 0x0200, 0, 0, 0x0020, 0x0010, 0x0040, 0x0080, 0x0001,
    0x0002, 0x0004, 0x0008, 0x0400,
];

pub(crate) fn to_xusb_report(report: GamepadReport) -> XusbReport {
    let buttons = XUSB_BUTTONS
        .iter()
        .enumerate()
        .filter(|(index, _)| report.buttons & (1 << index) != 0)
        .fold(0, |buttons, (_, mask)| buttons | mask);
    // XInput's Y axis increases upward.
    let invert_y = |value: i16| value.checked_neg().unwrap_or(i16::MAX);
    XusbReport {
        buttons,
        left_trigger: report.left_trigger,
        right_trigger: report.right_trigger,
        left_x: report.left_x,
        left_y: invert_y(report.left_y),
        right_x: report.right_x,
        right_y: invert_y(report.right_y),
    }
}

// Public bus protocol: nefarius/ViGEmClient b66d02d57e32cc8595369c53418b843e958649b4
// include/ViGEm/km/BusShared.h. Session ownership and readiness:
// nefarius/ViGEmBus d986e1d93708ec9b11049542fa6027272cce716c, sys/Driver.cpp,
// busenum.cpp and EmulationTargetPDO.cpp.
// CTL_CODE(FILE_DEVICE_BUS_EXTENDER, function, METHOD_BUFFERED, FILE_WRITE_DATA).
pub(crate) const VIGEM_PLUG_IN: u32 = 0x002a_a004;
pub(crate) const VIGEM_UNPLUG: u32 = 0x002a_a008;
pub(crate) const VIGEM_CHECK_VERSION: u32 = 0x002a_a00c;
pub(crate) const VIGEM_WAIT_READY: u32 = 0x002a_a010;
pub(crate) const VIGEM_SUBMIT_XUSB: u32 = 0x002a_a808;

/// `VIGEM_COMMON_VERSION`.
pub(crate) const VIGEM_COMMON_VERSION: u32 = 1;
/// Xbox 360 wired controller identity.
pub(crate) const XBOX360_VENDOR: u16 = 0x045e;
pub(crate) const XBOX360_PRODUCT: u16 = 0x028e;
/// The Windows XInput slots shared with the Host's own controllers.
pub(crate) const MAX_WINDOWS_GAMEPADS: usize = 4;

// The protocol uses Win32 ULONG/enum (32-bit) and USHORT (16-bit) fields.

/// `VIGEM_CHECK_VERSION`, `VIGEM_WAIT_DEVICE_READY` and `VIGEM_UNPLUG_TARGET`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct VigemRequest {
    pub size: u32,
    pub value: u32,
}

/// `VIGEM_PLUGIN_TARGET`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct VigemPlugin {
    pub size: u32,
    pub serial: u32,
    pub target_type: u32,
    pub vendor: u16,
    pub product: u16,
}

/// `XUSB_SUBMIT_REPORT`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct VigemXusbSubmission {
    pub size: u32,
    pub serial: u32,
    pub report: XusbReport,
}
