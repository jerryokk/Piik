// Windows platform, target and gamepad tests. These never inject desktop
// input: they observe hidden fixtures, check ABI layouts and register only a
// free hotkey.

use std::mem::{offset_of, size_of};

use piik_native_capture::CaptureTarget;
use windows::Win32::Foundation::{ERROR_ACCESS_DENIED, HWND};
use windows::Win32::Graphics::Gdi::{MONITOR_DEFAULTTOPRIMARY, MonitorFromPoint};
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::Input::KeyboardAndMouse::{INPUT, INPUT_KEYBOARD, INPUT_MOUSE};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, GetWindowThreadProcessId, SWP_NOACTIVATE, SWP_NOZORDER,
    SetWindowPos, WINDOW_EX_STYLE, WS_POPUP,
};
use windows::core::w;

use super::input::{rejected_input, to_input};
use super::target::{observe, process_creation};
use super::vigem::VigemBackend;
use super::{current_process_integrity, hotkey};
use crate::gamepad::{GamepadBackend, VigemPlugin, VigemRequest, VigemXusbSubmission, XusbReport};
use crate::packets::{KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, button_input, text_inputs};

#[test]
fn rejected_windows_input_diagnostics() {
    for code in [0, ERROR_ACCESS_DENIED.0] {
        let rejection = rejected_input(0, 1, code);
        assert_eq!(
            rejection.code, code,
            "Windows rejection lost its system code"
        );
        let text = rejection.to_string();
        for fact in [
            "Windows accepted 0 of 1",
            "input desktop accessible=",
            "input desktop matches thread=",
            "foreground present=",
            "foreground higher integrity=",
        ] {
            assert!(
                text.contains(fact),
                "Windows rejection lost {fact:?}: {text}"
            );
        }
    }
    let level = current_process_integrity().expect("current process integrity was not observed");
    assert_ne!(level, 0);
}

#[test]
fn committed_text_packets() {
    let inputs: Vec<INPUT> = text_inputs("A中😀").into_iter().map(to_input).collect();
    let units = [0x41u16, 0x4e2d, 0xd83d, 0xde00];
    assert_eq!(inputs.len(), 2 * units.len(), "text packets");
    for (i, input) in inputs.iter().enumerate() {
        assert_eq!(input.r#type, INPUT_KEYBOARD);
        // SAFETY: keyboard records carry the `ki` variant.
        let key = unsafe { input.Anonymous.ki };
        let flags = KEYEVENTF_UNICODE | if i % 2 == 1 { KEYEVENTF_KEYUP } else { 0 };
        assert!(
            key.wVk.0 == 0 && key.wScan == units[i / 2] && key.dwFlags.0 == flags,
            "packet {i}"
        );
    }
}

#[test]
fn mouse_button_packets() {
    for (button, flags) in [2u32, 0x20, 8, 0x80, 0x80].into_iter().enumerate() {
        for down in [true, false] {
            let input = to_input(button_input(button as i64, down).unwrap());
            let want_flags = if down { flags } else { flags << 1 };
            let want_data = if button >= 3 { 1 << (button - 3) } else { 0 };
            assert_eq!(input.r#type, INPUT_MOUSE);
            // SAFETY: mouse records carry the `mi` variant.
            let mouse = unsafe { input.Anonymous.mi };
            assert!(
                mouse.dwFlags.0 == want_flags && mouse.mouseData == want_data,
                "button {button} down={down}"
            );
        }
    }
    assert!(button_input(5, true).is_err(), "unknown button accepted");
}

#[test]
fn native_stop_shortcut_registration_lifetime() {
    // Reserve an available chord through the OS API without injecting input.
    let (key, reserved) = (1..=11)
        .find_map(|key| {
            hotkey::watch_stop_shortcut(key, Box::new(|| {}))
                .ok()
                .map(|stop| (key, stop))
        })
        .expect("no test shortcut is available");
    assert!(
        hotkey::watch_stop_shortcut(key, Box::new(|| {})).is_err(),
        "reserved chord was reported usable"
    );
    reserved();
    let stop = hotkey::watch_stop_shortcut(key, Box::new(|| {}))
        .expect("released chord was not available");
    stop();
    let stop = hotkey::watch_stop_shortcut(key, Box::new(|| {}))
        .expect("stopped watcher retained its chord");
    stop();
}

struct Window(HWND);

impl Drop for Window {
    fn drop(&mut self) {
        // SAFETY: destroys the fixture window this test created.
        let _ = unsafe { DestroyWindow(self.0) };
    }
}

#[test]
fn window_target_observations() {
    // A hidden fixture never activates or injects desktop input.
    // SAFETY: creates a hidden STATIC window owned by this test thread.
    let window = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("STATIC"),
            None,
            WS_POPUP,
            0,
            0,
            320,
            200,
            None,
            None,
            None,
            None,
        )
    }
    .expect("create hidden fixture");
    let fixture = Window(window);
    let mut pid = 0u32;
    // SAFETY: `pid` outlives the call.
    unsafe { GetWindowThreadProcessId(window, Some(&mut pid)) };
    // SAFETY: reads this process's identifier.
    assert_eq!(pid, unsafe { GetCurrentProcessId() });
    let mut target = CaptureTarget {
        kind: "window".into(),
        source_id: (window.0 as usize).to_string(),
        pid,
        creation_time: process_creation(pid).unwrap().to_string(),
        title: "fixture".into(),
    };
    let (bounds, _) = observe(&target).expect("valid geometry");
    assert!(bounds.width > 0 && bounds.height > 0);
    let resize = |cx, cy| {
        // SAFETY: resizes the fixture without activating it.
        unsafe { SetWindowPos(window, None, 0, 0, cx, cy, SWP_NOZORDER | SWP_NOACTIVATE) }.unwrap();
    };
    resize(0, 0);
    assert_eq!(
        observe(&target).unwrap(),
        (Default::default(), false),
        "empty geometry must suspend"
    );
    resize(320, 200);
    assert!(
        observe(&target).unwrap().0.width > 0,
        "geometry did not recover"
    );
    target.creation_time = "1".into();
    assert!(
        observe(&target).is_err(),
        "replaced process identity accepted"
    );
    target.pid += 1;
    assert!(observe(&target).is_err(), "different window owner accepted");
    drop(fixture);
    assert!(observe(&target).is_err(), "destroyed window accepted");
}

#[test]
fn monitor_observation() {
    // SAFETY: plain monitor lookup.
    let monitor = unsafe { MonitorFromPoint(Default::default(), MONITOR_DEFAULTTOPRIMARY) };
    if monitor.0.is_null() {
        return; // No interactive display.
    }
    let mut target = CaptureTarget {
        kind: "display".into(),
        source_id: (monitor.0 as usize).to_string(),
        ..CaptureTarget::default()
    };
    let (bounds, focused) = observe(&target).unwrap();
    assert!(bounds.width > 0 && focused);
    // An unknown monitor is a temporary observation failure, not retirement.
    target.source_id = "1".into();
    assert_eq!(observe(&target).unwrap(), (Default::default(), false));
    target.source_id = "0".into();
    assert!(observe(&target).is_err());
}

#[test]
fn gamepad_report_abi() {
    assert_eq!(size_of::<XusbReport>(), 12);
    assert_eq!(offset_of!(XusbReport, left_trigger), 2);
    assert_eq!(offset_of!(XusbReport, right_trigger), 3);
    assert_eq!(offset_of!(XusbReport, left_x), 4);
    assert_eq!(offset_of!(XusbReport, left_y), 6);
    assert_eq!(offset_of!(XusbReport, right_x), 8);
    assert_eq!(offset_of!(XusbReport, right_y), 10);
}

#[test]
fn gamepad_bus_request_abi() {
    use crate::gamepad::{
        VIGEM_CHECK_VERSION, VIGEM_PLUG_IN, VIGEM_SUBMIT_XUSB, VIGEM_UNPLUG, VIGEM_WAIT_READY,
    };
    assert_eq!(size_of::<VigemRequest>(), 8);
    assert_eq!(size_of::<VigemPlugin>(), 16);
    assert_eq!(offset_of!(VigemPlugin, vendor), 12);
    assert_eq!(size_of::<VigemXusbSubmission>(), 20);
    assert_eq!(offset_of!(VigemXusbSubmission, report), 8);
    // CTL_CODE(FILE_DEVICE_BUS_EXTENDER, function, METHOD_BUFFERED, FILE_WRITE_DATA).
    for (function, got) in [
        (0x801u32, VIGEM_PLUG_IN),
        (0x802, VIGEM_UNPLUG),
        (0x803, VIGEM_CHECK_VERSION),
        (0x804, VIGEM_WAIT_READY),
        (0xa02, VIGEM_SUBMIT_XUSB),
    ] {
        assert_eq!(
            got,
            (0x2a << 16) | (2 << 14) | (function << 2),
            "ViGEm IOCTL function {function:#x}"
        );
    }
}

#[test]
fn gamepad_capacity_comes_from_windows_slots() {
    // Capacity is whatever XInput reports, never our own device count: the
    // Host's physical pads share the same four slots. A closed backend is
    // refused before any native call is attempted.
    let backend =
        VigemBackend::with_handle(String::new(), Some(super::OwnedHandle(Default::default())));
    assert_eq!(
        super::xinput::free_count()
            + super::xinput::occupied()
                .iter()
                .filter(|used| **used)
                .count(),
        crate::gamepad::MAX_WINDOWS_GAMEPADS,
        "the slot sample did not cover every Windows controller slot"
    );
    backend.close().expect("closing an empty backend");
    let error = backend
        .open()
        .err()
        .expect("a closed backend must not allocate a device");
    assert!(error.to_string().contains("closed"), "got: {error}");
}

#[test]
fn a_plug_that_never_reaches_xinput_is_not_a_slot() {
    // The driver can accept a plug that games never see. Counting it would
    // report a working controller that does not exist.
    let before = [false, false, false, false];
    assert_eq!(
        super::xinput::newly_occupied(&before, &before),
        None,
        "an unchanged sample must not read as a new controller"
    );
}

/// Opt-in: creates only temporary targets on an installed ViGEmBus and reads
/// XInput state; production allocation uses bus sessions alone.
#[test]
fn gamepad_native_lifecycle() {
    if std::env::var("PIIK_GAMEPAD_GATE").as_deref() != Ok("1") {
        eprintln!("set PIIK_GAMEPAD_GATE=1 with the official ViGEmBus driver installed");
        return;
    }
    super::vigem_gate::run();
}
