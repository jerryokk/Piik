//! The local Ctrl+Alt+Fn stop shortcut.

use serde::Serialize;

use crate::Error;
use crate::manager::Inner;
use crate::policy::Mode;

pub const DEFAULT_STOP_KEY: i32 = 10;

/// F12 is reserved by Windows debuggers even when no debugger is running.
pub fn valid_stop_key(key: i32) -> bool {
    (1..=11).contains(&key)
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct StopShortcut {
    pub key: i32,
    pub available: bool,
}

impl Inner {
    /// Called with the grant lock held. Registration begins with an explicit
    /// Host action, so another application's hotkey cannot disable App
    /// startup or capture.
    pub fn ensure_stop_shortcut(&mut self) -> Result<(), Error> {
        if self.stop_shortcut.is_some() {
            return Ok(());
        }
        let key = self.stop_key;
        let Some(register) = self.register_stop.as_mut() else {
            return Ok(());
        };
        self.stop_shortcut = Some(register(key)?);
        Ok(())
    }

    /// A configured chord belongs to enabled input sessions, not the App
    /// process: idle sharing leaves it available to other applications.
    pub fn release_idle_shortcut(&mut self) {
        if self.stop_shortcut.is_none() || self.sessions.values().any(|s| s.mode != Mode::Off) {
            return;
        }
        if let Some(stop) = self.stop_shortcut.take() {
            stop();
        }
    }

    pub fn read_stop_shortcut(&mut self) -> Result<StopShortcut, Error> {
        if self.register_stop.is_none() {
            return Err(Error::ShortcutUnavailable);
        }
        if self.closed {
            return Err(Error::Closed);
        }
        let mut state = StopShortcut {
            key: self.stop_key,
            available: self.stop_shortcut.is_some(),
        };
        if !state.available {
            let key = self.stop_key;
            let register = self.register_stop.as_mut().expect("checked above");
            if let Ok(stop) = register(key) {
                state.available = true;
                stop();
            }
        }
        Ok(state)
    }

    pub fn set_stop_shortcut(&mut self, key: i32) -> Result<StopShortcut, Error> {
        if self.register_stop.is_none() || !valid_stop_key(key) {
            return Err(Error::ShortcutInvalid);
        }
        if self.closed {
            return Err(Error::Closed);
        }
        if key == self.stop_key && self.stop_shortcut.is_some() {
            return Ok(StopShortcut {
                key,
                available: true,
            });
        }
        let register = self.register_stop.as_mut().expect("checked above");
        let stop = register(key)?;
        // Keep the old working chord until registration succeeds; an idle
        // configuration does not reserve the new chord.
        match self.stop_shortcut.take() {
            Some(old) => {
                old();
                self.stop_shortcut = Some(stop);
            }
            None => stop(),
        }
        self.stop_key = key;
        for id in self.sessions.keys() {
            self.notify(*id);
        }
        Ok(StopShortcut {
            key,
            available: true,
        })
    }
}
