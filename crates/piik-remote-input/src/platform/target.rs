//! Shared-source identity, geometry and focus observation.

use piik_native_capture::CaptureTarget;
use windows::Win32::Foundation::{FILETIME, HWND, RECT};
use windows::Win32::Graphics::Dwm::{DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, HMONITOR, MONITORINFO};
use windows::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GA_ROOT, GetAncestor, GetForegroundWindow, GetWindowRect, GetWindowThreadProcessId, IsIconic,
    SW_RESTORE, SetForegroundWindow, ShowWindow,
};

use super::OwnedHandle;
use crate::Error;
use crate::event::Bounds;

/// Packs a FILETIME as the capture helper's decimal creation time.
pub(crate) fn filetime(value: FILETIME) -> u64 {
    (u64::from(value.dwHighDateTime) << 32) | u64::from(value.dwLowDateTime)
}

pub(crate) fn process_creation(pid: u32) -> Option<u64> {
    // SAFETY: opens a query-only handle owned below.
    let process =
        OwnedHandle(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?);
    let (mut created, mut exited, mut kernel, mut user) = Default::default();
    // SAFETY: all four out-parameters outlive the call.
    unsafe { GetProcessTimes(process.0, &mut created, &mut exited, &mut kernel, &mut user) }
        .ok()?;
    Some(filetime(created))
}

/// Brings the shared window to the foreground, so a Viewer asking to control a
/// background window can actually use it. Windows only honours this while the
/// caller owns the foreground, so a refused raise simply leaves the observation
/// below reporting the window as unfocused; the grant itself stays valid.
///
/// Called once when control is granted, never per input event: raising the
/// window on every pointer move would steal the Host's foreground repeatedly.
pub(crate) fn raise(target: &CaptureTarget) {
    if target.kind != "window" {
        return;
    }
    let Some(handle) = target
        .source_id
        .parse::<u64>()
        .ok()
        .filter(|id| *id != 0)
        .map(|id| HWND(id as usize as *mut core::ffi::c_void))
    else {
        return;
    };
    // SAFETY: `handle` is the capture helper's HWND; invalid values simply fail.
    unsafe {
        if IsIconic(handle).as_bool() {
            let _ = ShowWindow(handle, SW_RESTORE);
        }
        let _ = SetForegroundWindow(handle);
    }
}

/// `Ok((empty, false))` means a temporary observation failure: the grant is
/// suspended, not retired. `Err` means the source identity is gone.
pub(crate) fn observe(target: &CaptureTarget) -> Result<(Bounds, bool), Error> {
    let id = target
        .source_id
        .parse::<u64>()
        .ok()
        .filter(|id| *id != 0)
        .ok_or_else(|| Error::input("shared source is unavailable"))?;
    // Source IDs are the helper's HMONITOR/HWND values as decimal integers.
    let handle = id as usize as *mut core::ffi::c_void;
    let mut focused = true;
    let area = match target.kind.as_str() {
        "display" => {
            let mut info = MONITORINFO {
                cbSize: size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            // SAFETY: `info` is sized and outlives the call; a stale handle fails.
            if !unsafe { GetMonitorInfoW(HMONITOR(handle), &mut info) }.as_bool() {
                // A failed monitor observation does not prove the capture was retired.
                return Ok((Bounds::default(), false));
            }
            info.rcMonitor
        }
        "window" => {
            let window = HWND(handle);
            let mut pid = 0u32;
            // SAFETY: `pid` outlives the call; a stale handle yields 0.
            unsafe { GetWindowThreadProcessId(window, Some(&mut pid)) };
            if pid == 0 || pid != target.pid {
                return Err(Error::input("shared window is unavailable"));
            }
            let creation = target
                .creation_time
                .parse::<u64>()
                .ok()
                .filter(|creation| *creation != 0)
                .ok_or_else(|| Error::input("shared window identity is invalid"))?;
            // Match the capture target's process incarnation, not a recycled HWND/PID.
            let Some(created) = process_creation(pid) else {
                return Ok((Bounds::default(), false));
            };
            if creation != created {
                return Err(Error::input("shared window was replaced"));
            }
            let mut area = RECT::default();
            // SAFETY: `area` is a RECT of the size passed.
            let frame = unsafe {
                DwmGetWindowAttribute(
                    window,
                    DWMWA_EXTENDED_FRAME_BOUNDS,
                    (&raw mut area).cast(),
                    size_of::<RECT>() as u32,
                )
            };
            // SAFETY: `area` outlives the call.
            if frame.is_err() && unsafe { GetWindowRect(window, &mut area) }.is_err() {
                return Ok((Bounds::default(), false));
            }
            // SAFETY: borrowed window handles only.
            let (root, minimized) = unsafe {
                (
                    GetAncestor(GetForegroundWindow(), GA_ROOT),
                    IsIconic(window).as_bool(),
                )
            };
            focused = root == window && !minimized;
            area
        }
        _ => return Err(Error::input("source cannot accept input")),
    };
    if area.right <= area.left || area.bottom <= area.top {
        return Ok((Bounds::default(), false));
    }
    Ok((
        Bounds {
            left: area.left,
            top: area.top,
            width: area.right - area.left,
            height: area.bottom - area.top,
        },
        focused,
    ))
}
