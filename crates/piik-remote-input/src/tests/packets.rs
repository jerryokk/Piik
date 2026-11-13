// Pure SendInput mapping and the Windows held-input state machine, run
// against a recording injector: committed text, mouse buttons, the gamepad
// XUSB mapping and pending-release recovery after the desktop returns.

use crate::Error;
use crate::event::{Bounds, InputEvent};
use crate::gamepad::{GamepadReport, XusbReport, to_xusb_report};
use crate::keyboard::scan_code;
use crate::packets::*;

#[derive(Default)]
struct Recorder {
    sent: Vec<Vec<RawInput>>,
    reject: bool,
    screen: VirtualScreen,
}

impl Injector for Recorder {
    fn inject(&mut self, inputs: &[RawInput]) -> Result<(), Error> {
        if self.reject {
            return Err(Error::input("rejected"));
        }
        self.sent.push(inputs.to_vec());
        Ok(())
    }

    fn virtual_screen(&self) -> VirtualScreen {
        self.screen
    }
}

fn sink() -> PacketSink<Recorder> {
    PacketSink::new(Recorder {
        screen: VirtualScreen {
            x: -1920,
            y: 0,
            width: 3840,
            height: 1080,
        },
        ..Recorder::default()
    })
}

#[test]
fn committed_text_packets() {
    let inputs = text_inputs("A中😀");
    let units = [0x41u16, 0x4e2d, 0xd83d, 0xde00];
    assert_eq!(inputs.len(), 2 * units.len());
    for (i, input) in inputs.iter().enumerate() {
        let flags = KEYEVENTF_UNICODE | if i % 2 == 1 { KEYEVENTF_KEYUP } else { 0 };
        assert_eq!(
            *input,
            RawInput::Keyboard {
                scan: units[i / 2],
                flags
            },
            "packet {i}"
        );
    }
}

#[test]
fn mouse_button_packets() {
    for (button, flags) in [2u32, 0x20, 8, 0x80, 0x80].into_iter().enumerate() {
        for down in [true, false] {
            let want = RawInput::Mouse {
                dx: 0,
                dy: 0,
                data: if button >= 3 { 1 << (button - 3) } else { 0 },
                flags: if down { flags } else { flags << 1 },
            };
            assert_eq!(
                button_input(button as i64, down).unwrap(),
                want,
                "button {button} down={down}"
            );
        }
    }
    assert!(
        button_input(5, true).is_err() && button_input(-1, true).is_err(),
        "unknown button accepted"
    );
}

#[test]
fn scan_codes_and_extended_keys() {
    assert_eq!(scan_code("KeyA"), Some(0x1e));
    assert_eq!(scan_code("F24"), Some(0x76));
    assert_eq!(scan_code("Unidentified"), None);
    assert_eq!(
        key_input(0x1e, true),
        RawInput::Keyboard {
            scan: 0x1e,
            flags: KEYEVENTF_SCANCODE
        }
    );
    assert_eq!(
        key_input(scan_code("ArrowUp").unwrap(), false),
        RawInput::Keyboard {
            scan: 0x48,
            flags: KEYEVENTF_SCANCODE | KEYEVENTF_KEYUP | KEYEVENTF_EXTENDEDKEY
        }
    );
}

#[test]
fn absolute_pointer_maps_target_into_virtual_desktop() {
    let screen = VirtualScreen {
        x: -1920,
        y: 0,
        width: 3840,
        height: 1080,
    };
    let target = Bounds {
        left: 0,
        top: 0,
        width: 1920,
        height: 1080,
    };
    let RawInput::Mouse { dx, dy, flags, .. } = absolute_move(target, 1.0, 1.0, screen).unwrap()
    else {
        panic!("pointer is a mouse input");
    };
    assert_eq!((dx, dy, flags), (65535, 65535, MOUSEEVENTF_ABSOLUTE_MOVE));
    let RawInput::Mouse { dx, dy, .. } = absolute_move(target, 0.0, 0.0, screen).unwrap() else {
        panic!("pointer is a mouse input");
    };
    assert_eq!((dx, dy), ((1920.0 * 65535.0 / 3839.0) as i32, 0));
    for (x, y) in [(f64::NAN, 0.5), (-0.1, 0.5), (0.5, 1.1)] {
        assert!(
            absolute_move(target, x, y, screen).is_err(),
            "invalid pointer accepted"
        );
    }
    assert!(absolute_move(target, 0.5, 0.5, VirtualScreen::default()).is_err());
}

#[test]
fn sink_bounds_and_held_tracking() {
    let mut sink = sink();
    let relative = |dx| InputEvent {
        dx,
        ..InputEvent::of("relative")
    };
    assert!(sink.apply(Bounds::default(), &relative(32768)).is_err());
    sink.apply(Bounds::default(), &relative(-32767)).unwrap();
    let wheel = |delta| InputEvent {
        delta,
        ..InputEvent::of("wheel")
    };
    assert!(sink.apply(Bounds::default(), &wheel(1201)).is_err());
    sink.apply(Bounds::default(), &wheel(-120)).unwrap();
    assert_eq!(
        sink.injector.sent.last().unwrap()[0],
        RawInput::Mouse {
            dx: 0,
            dy: 0,
            data: (-120i32) as u32,
            flags: MOUSEEVENTF_WHEEL
        }
    );
    assert!(
        sink.apply(Bounds::default(), &InputEvent::of("paste"))
            .is_err()
    );
    let sent = sink.injector.sent.len();
    // Unknown codes and unheld releases inject nothing.
    sink.apply(Bounds::default(), &InputEvent::key("Unidentified", true))
        .unwrap();
    sink.apply(Bounds::default(), &InputEvent::key("KeyA", false))
        .unwrap();
    sink.apply(Bounds::default(), &InputEvent::button(0, false))
        .unwrap();
    assert_eq!(sink.injector.sent.len(), sent);
    sink.apply(Bounds::default(), &InputEvent::key("KeyA", true))
        .unwrap();
    sink.apply(Bounds::default(), &InputEvent::button(1, true))
        .unwrap();
    assert!(sink.keys.contains(&0x1e) && sink.buttons.contains(&1));
    // A rejected reset keeps its release debt; a later reset recovers.
    sink.injector.reject = true;
    assert!(sink.reset().is_err());
    assert!(sink.keys.contains(&0x1e) && sink.buttons.contains(&1));
    sink.injector.reject = false;
    sink.reset().unwrap();
    assert!(sink.keys.is_empty() && sink.buttons.is_empty());
    let releases: Vec<RawInput> = sink
        .injector
        .sent
        .iter()
        .rev()
        .take(2)
        .map(|inputs| inputs[0])
        .collect();
    assert!(releases.contains(&key_input(0x1e, false)));
    assert!(releases.contains(&button_input(1, false).unwrap()));
}

#[test]
fn gamepad_xusb_mapping() {
    // SDK Common.h XUSB_BUTTON values. Browser trigger buttons are analog in
    // XUSB_REPORT; Guide is independent of ordinary face/shoulder buttons.
    let want = [
        0x1000u16, 0x2000, 0x4000, 0x8000, 0x100, 0x200, 0, 0, 0x20, 0x10, 0x40, 0x80, 1, 2, 4, 8,
        0x400,
    ];
    for (index, want) in want.into_iter().enumerate() {
        let report = GamepadReport {
            buttons: 1 << index,
            ..GamepadReport::default()
        };
        assert_eq!(to_xusb_report(report).buttons, want, "W3C button {index}");
    }
    assert_eq!(
        to_xusb_report(GamepadReport::default()),
        XusbReport::default()
    );
    let got = to_xusb_report(GamepadReport {
        buttons: 1 | (1 << 4) | (1 << 12) | (1 << 16),
        left_trigger: 37,
        right_trigger: 255,
        left_x: -32768,
        left_y: -32768,
        right_x: 32767,
        right_y: 32767,
    });
    let want = XusbReport {
        buttons: 0x1501,
        left_trigger: 37,
        right_trigger: 255,
        left_x: -32768,
        left_y: 32767,
        right_x: 32767,
        right_y: -32767,
    };
    assert_eq!(got, want, "buttons/triggers/stick axes changed");
}
