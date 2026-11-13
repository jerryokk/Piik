//! `SendInput` injection with desktop recovery and rejection observations.

use windows::Win32::Foundation::{GENERIC_ALL, GetLastError};
use windows::Win32::System::StationsAndDesktops::{
    CloseDesktop, DESKTOP_ACCESS_FLAGS, DESKTOP_CONTROL_FLAGS, DESKTOP_READOBJECTS,
    GetThreadDesktop, HDESK, OpenInputDesktop, SetThreadDesktop,
};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBD_EVENT_FLAGS, KEYBDINPUT, MOUSE_EVENT_FLAGS,
    MOUSEINPUT, SendInput, VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetSystemMetrics, GetWindowThreadProcessId, SM_CXVIRTUALSCREEN,
    SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

use super::{current_process_integrity, desktop_name, process_integrity};
use crate::event::{Bounds, InputEvent};
use crate::manager::InputSink;
use crate::packets::{Injector, PacketSink, RawInput, VirtualScreen};
use crate::{Error, InputRejection};

/// Closes an owned desktop handle on drop.
struct OwnedDesktop(HDESK);

impl Drop for OwnedDesktop {
    fn drop(&mut self) {
        // SAFETY: this wrapper exclusively owns the handle from OpenInputDesktop.
        let _ = unsafe { CloseDesktop(self.0) };
    }
}

fn open_input_desktop(access: DESKTOP_ACCESS_FLAGS) -> Option<OwnedDesktop> {
    // SAFETY: plain handle-returning call; the handle is owned by the result.
    unsafe { OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, access) }
        .ok()
        .map(OwnedDesktop)
}

fn thread_desktop() -> HDESK {
    // SAFETY: the thread desktop handle is borrowed and needs no close.
    unsafe { GetThreadDesktop(GetCurrentThreadId()) }.unwrap_or_default()
}

pub(crate) fn to_input(raw: RawInput) -> INPUT {
    match raw {
        RawInput::Mouse {
            dx,
            dy,
            data,
            flags,
        } => INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx,
                    dy,
                    mouseData: data,
                    dwFlags: MOUSE_EVENT_FLAGS(flags),
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        },
        RawInput::Keyboard { scan, flags } => INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(0),
                    wScan: scan,
                    dwFlags: KEYBD_EVENT_FLAGS(flags),
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        },
    }
}

fn send(inputs: &[INPUT]) -> (u32, u32) {
    // SAFETY: `inputs` is a live slice of INPUT records sized by the API.
    let accepted = unsafe { SendInput(inputs, size_of::<INPUT>() as i32) };
    // SAFETY: reads this thread's last error immediately after the call.
    let code = if accepted as usize == inputs.len() {
        0
    } else {
        unsafe { GetLastError() }.0
    };
    (accepted, code)
}

/// Observes permission boundaries only after a rejected operation.
pub(crate) fn rejected_input(accepted: u32, requested: u32, code: u32) -> InputRejection {
    let mut result = InputRejection {
        accepted,
        requested,
        code,
        desktop_accessible: false,
        desktop_matches_thread: "unknown",
        foreground_present: false,
        higher_integrity: "unknown",
        restore: None,
    };
    if let Some(input) = open_input_desktop(DESKTOP_READOBJECTS) {
        result.desktop_accessible = true;
        if let (Some(input), Some(thread)) = (desktop_name(input.0), desktop_name(thread_desktop()))
        {
            result.desktop_matches_thread = if input == thread { "true" } else { "false" };
        }
    }
    // SAFETY: returns a borrowed window handle or null.
    let foreground = unsafe { GetForegroundWindow() };
    result.foreground_present = !foreground.0.is_null();
    if foreground.0.is_null() {
        return result;
    }
    let mut pid = 0u32;
    // SAFETY: `pid` outlives the call.
    unsafe { GetWindowThreadProcessId(foreground, Some(&mut pid)) };
    if let (Some(own), Some(other)) = (current_process_integrity(), process_integrity(pid)) {
        result.higher_integrity = if other > own { "true" } else { "false" };
    }
    result
}

pub(crate) struct WindowsInjector {
    /// The backend's original desktop; a changed active desktop grants no
    /// new input authority.
    pub desktop: String,
}

impl WindowsInjector {
    pub fn new() -> Result<Self, Error> {
        // The capture helper reports physical pixels. Match that coordinate
        // space before reading monitor bounds, including mixed DPI. An App
        // manifest may already have set it, which makes this call fail.
        // SAFETY: process-wide setting with a predefined context value.
        let _ =
            unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        let desktop = desktop_name(thread_desktop())
            .ok_or_else(|| Error::input("input backend desktop is unavailable"))?;
        Ok(Self { desktop })
    }
}

impl Injector for WindowsInjector {
    fn inject(&mut self, raw: &[RawInput]) -> Result<(), Error> {
        let inputs: Vec<INPUT> = raw.iter().copied().map(to_input).collect();
        let requested = inputs.len() as u32;
        let (accepted, code) = send(&inputs);
        if accepted == requested {
            return Ok(());
        }
        // Windows does not identify UIPI blocking through GetLastError.
        // Preserve the accepted count and any code without inventing a cause.
        let original = rejected_input(accepted, requested, code);
        // Input injection needs more than object observation.
        let Some(active) = open_input_desktop(DESKTOP_ACCESS_FLAGS(GENERIC_ALL.0)) else {
            return Err(Error::Rejected(original));
        };
        let thread = thread_desktop();
        match (desktop_name(active.0), desktop_name(thread)) {
            (Some(active_name), Some(thread_name))
                if active_name == self.desktop && active_name != thread_name => {}
            // Only repair a thread that drifted away from the original desktop.
            _ => return Err(Error::Rejected(original)),
        }
        // SAFETY: `active` stays open until after the thread is restored.
        if unsafe { SetThreadDesktop(active.0) }.is_err() {
            return Err(Error::Rejected(original));
        }
        // Never replay the accepted prefix of a partially accepted operation.
        let (retried, retry_code) = send(&inputs[accepted as usize..]);
        let mut result = (retried != requested - accepted)
            .then(|| rejected_input(accepted + retried, requested, retry_code));
        // SAFETY: restores the borrowed original thread desktop.
        if let Err(error) = unsafe { SetThreadDesktop(thread) } {
            let rejection = result.get_or_insert_with(|| rejected_input(requested, requested, 0));
            rejection.restore = Some(error.message());
        }
        result.map_or(Ok(()), |rejection| Err(Error::Rejected(rejection)))
    }

    fn virtual_screen(&self) -> VirtualScreen {
        // SAFETY: plain metric queries.
        unsafe {
            VirtualScreen {
                x: GetSystemMetrics(SM_XVIRTUALSCREEN),
                y: GetSystemMetrics(SM_YVIRTUALSCREEN),
                width: GetSystemMetrics(SM_CXVIRTUALSCREEN),
                height: GetSystemMetrics(SM_CYVIRTUALSCREEN),
            }
        }
    }
}

/// The Windows input owner: target observation plus tracked injection.
pub(crate) struct WindowsSink(pub PacketSink<WindowsInjector>);

impl WindowsSink {
    pub fn new() -> Result<Self, Error> {
        Ok(Self(PacketSink::new(WindowsInjector::new()?)))
    }
}

impl InputSink for WindowsSink {
    fn target(
        &mut self,
        target: &piik_native_capture::CaptureTarget,
    ) -> Result<(Bounds, bool), Error> {
        super::target::observe(target)
    }

    fn raise(&mut self, target: &piik_native_capture::CaptureTarget) {
        super::target::raise(target);
    }

    fn apply(&mut self, target: Bounds, event: &InputEvent) -> Result<(), Error> {
        self.0.apply(target, event)
    }

    fn reset(&mut self) -> Result<(), Error> {
        self.0.reset()
    }
}
