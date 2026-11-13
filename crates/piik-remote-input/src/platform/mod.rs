//! Windows calls. Every `unsafe` block is a direct Win32 call with
//! caller-owned buffers whose sizes are passed alongside them.

pub(crate) mod hotkey;
pub(crate) mod input;
pub(crate) mod target;
pub(crate) mod vigem;
pub(crate) mod xinput;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod vigem_gate;

use windows::Win32::Foundation::{CloseHandle, ERROR_INSUFFICIENT_BUFFER, HANDLE};
use windows::Win32::Security::{
    GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, TOKEN_MANDATORY_LABEL,
    TOKEN_QUERY, TokenIntegrityLevel,
};
use windows::Win32::System::StationsAndDesktops::{GetUserObjectInformationW, HDESK, UOI_NAME};
use windows::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};

/// Closes a kernel handle on drop.
pub(crate) struct OwnedHandle(pub HANDLE);

// SAFETY: a kernel file/process handle is a process-wide value usable from
// any thread; every use is serialized by its owner's mutex.
unsafe impl Send for OwnedHandle {}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_invalid() && !self.0.0.is_null() {
            // SAFETY: this wrapper exclusively owns the open handle.
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
}

/// The desktop object's name, never logged.
pub(crate) fn desktop_name(desktop: HDESK) -> Option<String> {
    if desktop.0.is_null() {
        return None;
    }
    let handle = HANDLE(desktop.0);
    let mut size = 0u32;
    // SAFETY: a size query with no buffer; `size` outlives the call.
    let _ = unsafe { GetUserObjectInformationW(handle, UOI_NAME, None, 0, Some(&mut size)) };
    if size == 0 {
        return None;
    }
    let mut buffer = vec![0u16; size.div_ceil(2) as usize];
    let bytes = u32::try_from(buffer.len() * 2).ok()?;
    // SAFETY: `buffer` holds `bytes` writable bytes for the duration of the call.
    unsafe {
        GetUserObjectInformationW(
            handle,
            UOI_NAME,
            Some(buffer.as_mut_ptr().cast()),
            bytes,
            Some(&mut size),
        )
    }
    .ok()?;
    let end = buffer
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(buffer.len());
    Some(String::from_utf16_lossy(&buffer[..end]))
}

/// The mandatory integrity RID of a token.
pub(crate) fn token_integrity(token: HANDLE) -> Option<u32> {
    let mut size = 0u32;
    // SAFETY: a size query with no buffer; `size` outlives the call.
    let probe = unsafe { GetTokenInformation(token, TokenIntegrityLevel, None, 0, &mut size) };
    if probe.err().map(|error| error.code()) != Some(ERROR_INSUFFICIENT_BUFFER.to_hresult()) {
        return None;
    }
    // u64 storage keeps the TOKEN_MANDATORY_LABEL header pointer-aligned.
    let mut buffer = vec![0u64; (size as usize).div_ceil(8)];
    // SAFETY: `buffer` holds at least `size` writable, aligned bytes.
    unsafe {
        GetTokenInformation(
            token,
            TokenIntegrityLevel,
            Some(buffer.as_mut_ptr().cast()),
            size,
            &mut size,
        )
    }
    .ok()?;
    // SAFETY: on success the buffer starts with a TOKEN_MANDATORY_LABEL whose
    // SID points into the same live buffer.
    unsafe {
        let label = &*buffer.as_ptr().cast::<TOKEN_MANDATORY_LABEL>();
        let sid = label.Label.Sid;
        if sid.0.is_null() {
            return None;
        }
        let count = *GetSidSubAuthorityCount(sid);
        if count == 0 {
            return None;
        }
        Some(*GetSidSubAuthority(sid, u32::from(count) - 1))
    }
}

pub(crate) fn current_process_integrity() -> Option<u32> {
    let mut token = HANDLE::default();
    // SAFETY: the pseudo handle needs no close; `token` receives a new handle.
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }.ok()?;
    let token = OwnedHandle(token);
    token_integrity(token.0)
}

pub(crate) fn process_integrity(pid: u32) -> Option<u32> {
    // SAFETY: opens a query-only handle owned below.
    let process =
        OwnedHandle(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?);
    let mut token = HANDLE::default();
    // SAFETY: `process` is open; `token` receives a new handle.
    unsafe { OpenProcessToken(process.0, TOKEN_QUERY, &mut token) }.ok()?;
    let token = OwnedHandle(token);
    token_integrity(token.0)
}
