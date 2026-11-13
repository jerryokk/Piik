//! Held key/button ownership and release debt.

use crate::event::{Bounds, InputEvent};
use crate::manager::{Inner, Key, SessionId};
use crate::{Error, join};

const MAX_KEY_CODE_BYTES: usize = 32;
const MAX_HELD_KEYS: usize = 256;
const MAX_BUTTON: i64 = 4;

impl Inner {
    /// Whole-source cleanup may use the platform's tracked physical state.
    /// The logical release debt stays until reset actually succeeds,
    /// including after retirement.
    pub fn reset_held(&mut self, session: SessionId) -> Result<(), Error> {
        for key in self.session_keys(session) {
            let c = self.connections.get_mut(&key).expect("listed connection");
            self.pending_keys.extend(std::mem::take(&mut c.keys));
            self.pending_buttons.extend(std::mem::take(&mut c.buttons));
        }
        self.sink.reset()?;
        self.pending_keys.clear();
        self.pending_buttons.clear();
        Ok(())
    }

    /// Failed releases outlive their departing controller. Only releases are
    /// retried; pointer movement, presses and committed text never replay.
    pub fn release_key(&mut self, code: &str) -> Result<(), Error> {
        let result = self.apply_input(Bounds::default(), &InputEvent::key(code, false));
        if result.is_err() {
            self.pending_keys.insert(code.to_owned());
        } else {
            self.pending_keys.remove(code);
        }
        result
    }

    pub fn release_button(&mut self, button: i64) -> Result<(), Error> {
        let result = self.apply_input(Bounds::default(), &InputEvent::button(button, false));
        if result.is_err() {
            self.pending_buttons.insert(button);
        } else {
            self.pending_buttons.remove(&button);
        }
        result
    }

    pub fn retry_releases(&mut self) -> Result<(), Error> {
        let mut result = None;
        for code in self.pending_keys.clone() {
            if let Err(error) = self.release_key(&code) {
                result = join(result, error.context("key release retry"));
            }
        }
        for button in self.pending_buttons.clone() {
            if let Err(error) = self.release_button(button) {
                result = join(result, error.context("button release retry"));
            }
        }
        result.map_or(Ok(()), Err)
    }

    pub fn note_input_result(&mut self, key: &Key, failed: bool) {
        let Some(c) = self.connections.get_mut(key) else {
            return;
        };
        if c.input_failed != failed {
            c.input_failed = failed;
            self.send_state(key);
        }
    }

    pub fn apply_held(
        &mut self,
        key: &Key,
        bounds: Bounds,
        event: &InputEvent,
    ) -> Result<(), Error> {
        let release_error = self.retry_releases().err();
        let releasing = matches!(event.kind.as_str(), "key" | "button") && !event.down;
        if !releasing && (!self.pending_keys.is_empty() || !self.pending_buttons.is_empty()) {
            let cause = release_error
                .map(|error| error.to_string())
                .unwrap_or_default();
            return Err(Error::input(format!(
                "held input cleanup is unavailable ({} key, {} button releases pending): {cause}",
                self.pending_keys.len(),
                self.pending_buttons.len()
            )));
        }
        match event.kind.as_str() {
            "key" => {
                let c = &self.connections[key];
                let code = event.code.as_str();
                if code.is_empty()
                    || code.len() > MAX_KEY_CODE_BYTES
                    || (event.down && c.keys.len() >= MAX_HELD_KEYS && !c.keys.contains(code))
                {
                    return Ok(());
                }
                if event.down {
                    // A repeat from this holder is input; another holder
                    // shares the press.
                    if c.keys.contains(code) || !self.key_held(code) {
                        self.apply_input(bounds, event)?;
                    }
                    let c = self.connections.get_mut(key).expect("known connection");
                    c.keys.insert(code.to_owned());
                } else {
                    let c = self.connections.get_mut(key).expect("known connection");
                    if !c.keys.remove(code) {
                        return Ok(());
                    }
                    if !self.key_held(code) {
                        return self.release_key(code);
                    }
                }
            }
            "button" => {
                let c = &self.connections[key];
                let button = event.button;
                if !(0..=MAX_BUTTON).contains(&button) || c.buttons.contains(&button) == event.down
                {
                    return Ok(());
                }
                if event.down {
                    if !self.button_held(button) {
                        self.apply_input(bounds, event)?;
                    }
                    let c = self.connections.get_mut(key).expect("known connection");
                    c.buttons.insert(button);
                } else {
                    let c = self.connections.get_mut(key).expect("known connection");
                    c.buttons.remove(&button);
                    if !self.button_held(button) {
                        return self.release_button(button);
                    }
                }
            }
            _ => return self.apply_input(bounds, event),
        }
        Ok(())
    }
}
