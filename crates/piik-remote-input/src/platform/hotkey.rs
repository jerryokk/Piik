//! A native hotkey supplies local revocation even while a Browser has focus.
//! It observes this one chord; no keyboard hook or key recording is installed.

use std::sync::mpsc;

use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, RegisterHotKey, UnregisterHotKey, VK_F1,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetMessageW, MSG, PM_NOREMOVE, PeekMessageW, PostThreadMessageW, WM_HOTKEY, WM_QUIT,
};

use crate::Error;
use crate::manager::StopHandle;

const HOTKEY_ID: i32 = 1;

/// Registers Ctrl+Alt+F`key` on a dedicated message thread. The returned
/// handle unregisters it and joins the thread.
pub(crate) fn watch_stop_shortcut(
    key: i32,
    stop: Box<dyn Fn() + Send>,
) -> Result<StopHandle, Error> {
    let (ready, registered) = mpsc::channel();
    let thread = std::thread::Builder::new()
        .name("piik-remote-stop".into())
        .spawn(move || {
            let mut message = MSG::default();
            // SAFETY: creates this thread's message queue before posting to it.
            let _ = unsafe { PeekMessageW(&mut message, None, 0, 0, PM_NOREMOVE) };
            let vk = u32::from(VK_F1.0) + (key - 1) as u32;
            // SAFETY: a thread hotkey bound to this message queue.
            if unsafe { RegisterHotKey(None, HOTKEY_ID, MOD_CONTROL | MOD_ALT | MOD_NOREPEAT, vk) }
                .is_err()
            {
                let _ = ready.send(None);
                return;
            }
            // SAFETY: reads this thread's identifier.
            let _ = ready.send(Some(unsafe { GetCurrentThreadId() }));
            loop {
                // SAFETY: `message` outlives the call.
                let result = unsafe { GetMessageW(&mut message, None, 0, 0) };
                if result.0 <= 0 {
                    break;
                }
                if message.message == WM_HOTKEY {
                    stop();
                }
            }
            // SAFETY: unregisters this thread's own hotkey.
            let _ = unsafe { UnregisterHotKey(None, HOTKEY_ID) };
        })
        .map_err(|error| Error::input(format!("start remote-control shortcut: {error}")))?;
    let Ok(Some(thread_id)) = registered.recv() else {
        let _ = thread.join();
        return Err(Error::ShortcutInUse);
    };
    Ok(Box::new(move || {
        // SAFETY: posts WM_QUIT to the live message thread joined below.
        let _ = unsafe { PostThreadMessageW(thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) };
        let _ = thread.join();
    }))
}
