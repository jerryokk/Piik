//! The platform-neutral model of Windows `SendInput` packets and the held
//! key/button state machine of the Windows input backend. The OS call sits
//! behind [`Injector`], so all mapping and tracking runs in tests without
//! injecting real input.

use std::collections::BTreeSet;

use crate::event::{Bounds, InputEvent};
use crate::keyboard::scan_code;
use crate::{Error, join};

// MOUSEINPUT.dwFlags.
pub(crate) const MOUSEEVENTF_MOVE: u32 = 0x0001;
pub(crate) const MOUSEEVENTF_WHEEL: u32 = 0x0800;
/// MOVE | VIRTUALDESK | ABSOLUTE.
pub(crate) const MOUSEEVENTF_ABSOLUTE_MOVE: u32 = 0xc001;
// KEYBDINPUT.dwFlags.
pub(crate) const KEYEVENTF_EXTENDEDKEY: u32 = 0x0001;
pub(crate) const KEYEVENTF_KEYUP: u32 = 0x0002;
pub(crate) const KEYEVENTF_UNICODE: u32 = 0x0004;
pub(crate) const KEYEVENTF_SCANCODE: u32 = 0x0008;

/// One `INPUT` record. `time` and `dwExtraInfo` are always zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RawInput {
    Mouse {
        dx: i32,
        dy: i32,
        data: u32,
        flags: u32,
    },
    Keyboard {
        scan: u16,
        flags: u32,
    },
}

/// A physical key by scan code, never text.
pub(crate) fn key_input(scan: u16, down: bool) -> RawInput {
    let mut flags = KEYEVENTF_SCANCODE;
    if !down {
        flags |= KEYEVENTF_KEYUP;
    }
    if scan & 0xff00 == 0xe000 {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    RawInput::Keyboard {
        scan: scan & 0xff,
        flags,
    }
}

/// Soft keyboards and IMEs supply committed text, not physical scan codes.
/// Sends complete UTF-16 pairs through Windows' standard `VK_PACKET` path.
pub(crate) fn text_inputs(text: &str) -> Vec<RawInput> {
    text.encode_utf16()
        .flat_map(|unit| {
            [KEYEVENTF_UNICODE, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP]
                .map(|flags| RawInput::Keyboard { scan: unit, flags })
        })
        .collect()
}

/// Left, middle, right, X1, X2; the release flag is the press flag shifted.
pub(crate) fn button_input(button: i64, down: bool) -> Result<RawInput, Error> {
    const FLAGS: [u32; 5] = [0x02, 0x20, 0x08, 0x80, 0x80];
    let index = usize::try_from(button)
        .ok()
        .filter(|index| *index < FLAGS.len())
        .ok_or_else(|| Error::input("invalid button"))?;
    let mut flags = FLAGS[index];
    if !down {
        flags <<= 1;
    }
    let data = if index >= 3 { 1 << (index - 3) } else { 0 };
    Ok(RawInput::Mouse {
        dx: 0,
        dy: 0,
        data,
        flags,
    })
}

/// The virtual desktop rectangle from `GetSystemMetrics(SM_*VIRTUALSCREEN)`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct VirtualScreen {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// Maps a normalized point of `target` to the absolute 0..=65535 coordinate
/// space of the whole virtual desktop.
pub(crate) fn absolute_move(
    target: Bounds,
    x: f64,
    y: f64,
    screen: VirtualScreen,
) -> Result<RawInput, Error> {
    if x.is_nan() || y.is_nan() || !(0.0..=1.0).contains(&x) || !(0.0..=1.0).contains(&y) {
        return Err(Error::input("invalid pointer"));
    }
    if screen.width < 2 || screen.height < 2 {
        return Err(Error::input("desktop is unavailable"));
    }
    let px = f64::from(target.left) + x * f64::from(target.width - 1) - f64::from(screen.x);
    let py = f64::from(target.top) + y * f64::from(target.height - 1) - f64::from(screen.y);
    // Absolute coordinates truncate toward zero.
    Ok(RawInput::Mouse {
        dx: (px * 65535.0 / f64::from(screen.width - 1)) as i32,
        dy: (py * 65535.0 / f64::from(screen.height - 1)) as i32,
        data: 0,
        flags: MOUSEEVENTF_ABSOLUTE_MOVE,
    })
}

/// The `SendInput` boundary. A rejection never replays an accepted prefix.
pub(crate) trait Injector: Send {
    fn inject(&mut self, inputs: &[RawInput]) -> Result<(), Error>;
    fn virtual_screen(&self) -> VirtualScreen;
}

/// Tracks the physical keys and buttons this backend pressed, so a release is
/// sent only for input it actually holds.
pub(crate) struct PacketSink<I> {
    pub injector: I,
    pub keys: BTreeSet<u16>,
    pub buttons: BTreeSet<i64>,
}

impl<I: Injector> PacketSink<I> {
    pub fn new(injector: I) -> Self {
        Self {
            injector,
            keys: BTreeSet::new(),
            buttons: BTreeSet::new(),
        }
    }

    fn inject(&mut self, inputs: &[RawInput]) -> Result<(), Error> {
        if inputs.is_empty() {
            return Ok(());
        }
        self.injector.inject(inputs)
    }

    pub fn apply(&mut self, target: Bounds, event: &InputEvent) -> Result<(), Error> {
        match event.kind.as_str() {
            "move" => {
                let input =
                    absolute_move(target, event.x, event.y, self.injector.virtual_screen())?;
                self.inject(&[input])
            }
            "relative" => {
                if !(-32767..=32767).contains(&event.dx) || !(-32767..=32767).contains(&event.dy) {
                    return Err(Error::input("invalid relative pointer"));
                }
                self.inject(&[RawInput::Mouse {
                    dx: event.dx,
                    dy: event.dy,
                    data: 0,
                    flags: MOUSEEVENTF_MOVE,
                }])
            }
            "button" => {
                let input = button_input(event.button, event.down)?;
                if !event.down && !self.buttons.contains(&event.button) {
                    return Ok(());
                }
                self.inject(&[input])?;
                if event.down {
                    self.buttons.insert(event.button);
                } else {
                    self.buttons.remove(&event.button);
                }
                Ok(())
            }
            "wheel" => {
                if !(-1200..=1200).contains(&event.delta) {
                    return Err(Error::input("invalid wheel"));
                }
                // The signed delta travels as its two's-complement DWORD.
                let data = event.delta as i32 as u32;
                self.inject(&[RawInput::Mouse {
                    dx: 0,
                    dy: 0,
                    data,
                    flags: MOUSEEVENTF_WHEEL,
                }])
            }
            "key" => {
                let Some(key) = scan_code(&event.code) else {
                    return Ok(());
                };
                if !event.down && !self.keys.contains(&key) {
                    return Ok(());
                }
                self.inject(&[key_input(key, event.down)])?;
                if event.down {
                    self.keys.insert(key);
                } else {
                    self.keys.remove(&key);
                }
                Ok(())
            }
            "text" => self.inject(&text_inputs(&event.text)),
            _ => Err(Error::input("unsupported input")),
        }
    }

    /// Releases every held key and button; a failed release stays held for
    /// the next reset.
    pub fn reset(&mut self) -> Result<(), Error> {
        let mut result = None;
        for key in self.keys.clone() {
            match self.inject(&[key_input(key, false)]) {
                Ok(()) => {
                    self.keys.remove(&key);
                }
                Err(error) => result = join(result, error),
            }
        }
        for button in self.buttons.clone() {
            if let Err(error) = self.apply(Bounds::default(), &InputEvent::button(button, false)) {
                result = join(result, error);
            }
        }
        result.map_or(Ok(()), Err)
    }
}
