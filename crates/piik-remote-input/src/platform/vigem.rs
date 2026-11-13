//! Xbox 360 targets on an already-installed ViGEmBus, through the bus's
//! public device interface and IOCTLs. No client DLL is loaded, bundled or
//! installed.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use windows::Win32::Devices::DeviceAndDriverInstallation::{
    CM_GET_DEVICE_INTERFACE_LIST_PRESENT, CM_Get_Device_Interface_List_SizeW,
    CM_Get_Device_Interface_ListW, CR_SUCCESS,
};
use windows::Win32::Foundation::{
    ERROR_INVALID_PARAMETER, ERROR_NO_MORE_ITEMS, GENERIC_READ, GENERIC_WRITE, HANDLE,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::IO::DeviceIoControl;
use windows::core::{GUID, HSTRING, PCWSTR};

use super::OwnedHandle;
use super::xinput;
use crate::gamepad::{
    GamepadBackend, GamepadDevice, GamepadReport, VIGEM_CHECK_VERSION, VIGEM_COMMON_VERSION,
    VIGEM_PLUG_IN, VIGEM_SUBMIT_XUSB, VIGEM_UNPLUG, VIGEM_WAIT_READY, VigemPlugin, VigemRequest,
    VigemXusbSubmission, XBOX360_PRODUCT, XBOX360_VENDOR, gamepad_unavailable, to_xusb_report,
};
use crate::{Error, join};

/// `GUID_DEVINTERFACE_BUSENUM_VIGEM`.
const VIGEM_BUS_INTERFACE: GUID = GUID::from_values(
    0x96e4_2b22,
    0xf5e9,
    0x42f8,
    [0xb0, 0x43, 0xed, 0x0f, 0x93, 0x2f, 0x01, 0x4f],
);

/// A neutral report waits at most one 60 Hz frame for a queued USB request.
const NEUTRAL_RETRY: Duration = Duration::from_micros(16_667);
/// A freshly plugged pad answers XInput only after the driver enumerates it.
/// Poll briefly rather than declaring failure on the first sample.
const PLUG_ENUMERATION_POLLS: usize = 40;
const PLUG_ENUMERATION_POLL: Duration = Duration::from_millis(25);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

fn is_win32(error: &windows::core::Error, code: windows::Win32::Foundation::WIN32_ERROR) -> bool {
    error.code() == code.to_hresult()
}

fn interface_paths() -> Result<Vec<String>, Error> {
    let fail = |result: u32| {
        gamepad_unavailable(&format!(
            "enumerate ViGEmBus: configuration manager error {result}"
        ))
    };
    for _ in 0..4 {
        let mut length = 0u32;
        // SAFETY: `length` outlives the call; the GUID is a static.
        let result = unsafe {
            CM_Get_Device_Interface_List_SizeW(
                &mut length,
                &VIGEM_BUS_INTERFACE,
                PCWSTR::null(),
                CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
            )
        };
        if result != CR_SUCCESS {
            return Err(fail(result.0));
        }
        let mut buffer = vec![0u16; length.max(1) as usize];
        // SAFETY: `buffer` is writable for its whole length.
        let result = unsafe {
            CM_Get_Device_Interface_ListW(
                &VIGEM_BUS_INTERFACE,
                PCWSTR::null(),
                &mut buffer,
                CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
            )
        };
        // CR_BUFFER_SMALL: an interface arrived between the two calls.
        if result.0 == 26 {
            continue;
        }
        if result != CR_SUCCESS {
            return Err(fail(result.0));
        }
        return Ok(buffer
            .split(|unit| *unit == 0)
            .filter(|path| !path.is_empty())
            .map(String::from_utf16_lossy)
            .collect());
    }
    Err(fail(26))
}

/// A fresh file object owns a separate driver session. Closing it also
/// removes that session's targets after a crash or a failed explicit unplug.
fn open_session(path: &str) -> Result<OwnedHandle, Error> {
    // SAFETY: `HSTRING` keeps the NUL-terminated path alive for the call.
    unsafe {
        CreateFileW(
            &HSTRING::from(path),
            (GENERIC_READ | GENERIC_WRITE).0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
    }
    .map(OwnedHandle)
    .map_err(|error| Error::input(format!("open ViGEmBus: {}", error.message())))
}

fn control<T>(handle: HANDLE, code: u32, input: &T) -> windows::core::Result<()> {
    let mut transferred = 0u32;
    // SAFETY: `input` is a live, `#[repr(C)]` request of the size passed;
    // the bus writes no output for these METHOD_BUFFERED requests.
    unsafe {
        DeviceIoControl(
            handle,
            code,
            Some((input as *const T).cast()),
            size_of::<T>() as u32,
            None,
            0,
            Some(&mut transferred),
            None,
        )
    }
}

fn request(value: u32) -> VigemRequest {
    VigemRequest {
        size: size_of::<VigemRequest>() as u32,
        value,
    }
}

struct BackendState {
    /// The bus session that proves protocol compatibility; `None` once closed.
    handle: Option<OwnedHandle>,
    devices: BTreeSet<u64>,
    next: u64,
    registry: Vec<(u64, Arc<Mutex<DeviceState>>)>,
}

pub(crate) struct VigemBackend {
    path: String,
    state: Arc<Mutex<BackendState>>,
}

/// Opens the first ViGEmBus interface speaking protocol version 1.
pub(crate) fn open_backend() -> Option<Arc<dyn GamepadBackend>> {
    match VigemBackend::open() {
        Ok(backend) => Some(Arc::new(backend)),
        Err(error) => {
            tracing::debug!(event = "gamepad-unavailable", error = %error, "remote-input");
            None
        }
    }
}

/// Whether a compatible ViGEmBus is installed. Reads the device interface list
/// and closes the handle immediately: this is a question, not a session, so it
/// must not create a target or hold anything open.
pub(crate) fn driver_present() -> bool {
    if !cfg!(target_arch = "x86_64") {
        return false;
    }
    let Ok(paths) = interface_paths() else {
        return false;
    };
    paths.iter().any(|path| {
        let Ok(handle) = open_session(path) else {
            return false;
        };
        control(
            handle.0,
            VIGEM_CHECK_VERSION,
            &request(VIGEM_COMMON_VERSION),
        )
        .is_ok()
    })
}

impl VigemBackend {
    pub fn open() -> Result<Self, Error> {
        if !cfg!(target_arch = "x86_64") {
            return Err(gamepad_unavailable("ViGEmBus gamepads require Windows x64"));
        }
        let mut failure = None;
        for path in interface_paths()? {
            let handle = match open_session(&path) {
                Ok(handle) => handle,
                Err(error) => {
                    failure = join(failure, error);
                    continue;
                }
            };
            if let Err(error) = control(
                handle.0,
                VIGEM_CHECK_VERSION,
                &request(VIGEM_COMMON_VERSION),
            ) {
                failure = join(
                    failure,
                    Error::input(format!("ViGEmBus protocol version 1: {}", error.message())),
                );
                continue;
            }
            return Ok(Self::with_handle(path, Some(handle)));
        }
        let detail = failure
            .map(|error| format!(": {error}"))
            .unwrap_or_default();
        Err(gamepad_unavailable(&format!(
            "install the official ViGEmBus driver{detail}"
        )))
    }

    pub(crate) fn with_handle(path: String, handle: Option<OwnedHandle>) -> Self {
        Self {
            path,
            state: Arc::new(Mutex::new(BackendState {
                handle,
                devices: BTreeSet::new(),
                next: 0,
                registry: Vec::new(),
            })),
        }
    }

    #[cfg(test)]
    pub(crate) fn bus_handle(&self) -> HANDLE {
        lock(&self.state)
            .handle
            .as_ref()
            .map_or_else(HANDLE::default, |handle| handle.0)
    }

    pub(crate) fn create(&self) -> Result<VigemGamepad, Error> {
        // Each target owns a file session, so enumerating a new target cannot
        // serialize reports or retirement for another target behind the bus
        // readiness request.
        let handle = open_session(&self.path)?;
        let device = Arc::new(Mutex::new(DeviceState {
            handle: Some(handle),
            serial: 0,
            closed: false,
        }));
        let fail = |error: Error| {
            let cleanup = lock(&device).close();
            Err(cleanup.err().map_or(error.clone(), |cleanup| {
                join(Some(error), cleanup).expect("joined")
            }))
        };
        for serial in 1..=u32::from(u16::MAX) {
            let plugin = VigemPlugin {
                size: size_of::<VigemPlugin>() as u32,
                serial,
                target_type: 0, // Xbox360Wired
                vendor: XBOX360_VENDOR,
                product: XBOX360_PRODUCT,
            };
            let handle = lock(&device).raw();
            match control(handle, VIGEM_PLUG_IN, &plugin) {
                Err(error) if is_win32(&error, ERROR_INVALID_PARAMETER) => continue,
                Err(error) => {
                    return fail(Error::input(format!(
                        "create virtual Xbox 360: {}",
                        error.message()
                    )));
                }
                Ok(()) => {}
            }
            lock(&device).serial = serial;
            if let Err(error) = control(handle, VIGEM_WAIT_READY, &request(serial)) {
                return fail(Error::input(format!(
                    "initialize virtual Xbox 360: {}",
                    error.message()
                )));
            }
            if let Err(error) = lock(&device).submit(GamepadReport::default()) {
                return fail(error);
            }
            let mut state = lock(&self.state);
            if state.handle.is_none() {
                drop(state);
                return fail(Error::input("virtual gamepad backend is closed"));
            }
            state.next += 1;
            let id = state.next;
            state.devices.insert(id);
            state.registry.push((id, device.clone()));
            return Ok(VigemGamepad {
                id,
                device,
                backend: self.state.clone(),
            });
        }
        fail(Error::input("ViGEmBus has no free controller serial"))
    }
}

impl GamepadBackend for VigemBackend {
    fn open(&self) -> Result<Box<dyn GamepadDevice>, Error> {
        {
            let state = lock(&self.state);
            if state.handle.is_none() {
                return Err(Error::input("virtual gamepad backend is closed"));
            }
        }
        // XInput owns the budget, not our own device list: the Host's physical
        // pads share the same four slots, and ViGEm picks whichever index is
        // free rather than taking one we name.
        if xinput::free_count() == 0 {
            return Err(Error::input("all four Windows gamepad slots are in use"));
        }
        let before = xinput::occupied();
        let pad = self.create()?;
        // The driver accepted the plug. XInput enumerates a new pad as its own
        // device rather than on the spot, so give it a moment to answer before
        // deciding: a pad that never appears must not be reported as working.
        let mut after = xinput::occupied();
        for _ in 0..PLUG_ENUMERATION_POLLS {
            if xinput::newly_occupied(&before, &after).is_some() {
                break;
            }
            std::thread::sleep(PLUG_ENUMERATION_POLL);
            after = xinput::occupied();
        }
        if xinput::newly_occupied(&before, &after).is_none() {
            // Close the device we just made: no game can see it.
            let mut pad = pad;
            let _ = pad.close();
            return Err(Error::input(
                "virtual gamepad did not appear in Windows' controller slots",
            ));
        }
        Ok(Box::new(pad))
    }

    fn close(&self) -> Result<(), Error> {
        let (handle, registry) = {
            let mut state = lock(&self.state);
            let Some(handle) = state.handle.take() else {
                return Ok(());
            };
            state.devices.clear();
            (handle, std::mem::take(&mut state.registry))
        };
        let mut result = None;
        for (_, device) in registry {
            if let Err(error) = lock(&device).close() {
                result = join(result, error);
            }
        }
        drop(handle);
        result.map_or(Ok(()), Err)
    }
}

pub(crate) struct DeviceState {
    pub handle: Option<OwnedHandle>,
    pub serial: u32,
    pub closed: bool,
}

impl DeviceState {
    fn raw(&self) -> HANDLE {
        self.handle
            .as_ref()
            .map_or_else(HANDLE::default, |handle| handle.0)
    }

    fn submit(&self, report: GamepadReport) -> Result<(), Error> {
        self.submit_raw(report).map_err(|error| {
            Error::input(format!(
                "write virtual Xbox 360 report: {}",
                error.message()
            ))
        })
    }

    fn submit_raw(&self, report: GamepadReport) -> windows::core::Result<()> {
        let submission = VigemXusbSubmission {
            size: size_of::<VigemXusbSubmission>() as u32,
            serial: self.serial,
            report: to_xusb_report(report),
        };
        control(self.raw(), VIGEM_SUBMIT_XUSB, &submission)
    }

    fn close(&mut self) -> Result<(), Error> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        let mut result = None;
        if self.serial != 0 {
            match self.submit_raw(GamepadReport::default()) {
                Err(error) if !is_win32(&error, ERROR_NO_MORE_ITEMS) => {
                    result = join(
                        result,
                        Error::input(format!(
                            "write virtual Xbox 360 report: {}",
                            error.message()
                        )),
                    );
                }
                _ => {}
            }
            if let Err(error) = control(self.raw(), VIGEM_UNPLUG, &request(self.serial)) {
                result = join(
                    result,
                    Error::input(format!("remove virtual Xbox 360: {}", error.message())),
                );
            }
        }
        // File close is also the driver's final cleanup for this target alone.
        self.handle = None;
        self.serial = 0;
        result.map_or(Ok(()), Err)
    }
}

pub(crate) struct VigemGamepad {
    id: u64,
    pub(crate) device: Arc<Mutex<DeviceState>>,
    backend: Arc<Mutex<BackendState>>,
}

impl VigemGamepad {
    #[cfg(test)]
    pub(crate) fn serial(&self) -> u32 {
        lock(&self.device).serial
    }
}

impl GamepadDevice for VigemGamepad {
    fn update(&mut self, report: GamepadReport) -> Result<(), Error> {
        let device = lock(&self.device);
        if device.closed || device.handle.is_none() {
            return Err(Error::input("virtual gamepad device is closed"));
        }
        let deadline = Instant::now() + NEUTRAL_RETRY;
        loop {
            match device.submit_raw(report) {
                Err(error) if is_win32(&error, ERROR_NO_MORE_ITEMS) => {
                    // XusbPdo drops changed reports when no USB input request
                    // is queued. Ordinary full states repeat at 60 Hz; never
                    // delay them behind a retry.
                    if report != GamepadReport::default() {
                        return Ok(());
                    }
                    // A one-off focus reset has no following report. Give its
                    // neutral state one frame, then retire this pad.
                    if Instant::now() >= deadline {
                        return Err(Error::input(format!(
                            "write virtual Xbox 360 report: {}",
                            error.message()
                        )));
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(error) => {
                    return Err(Error::input(format!(
                        "write virtual Xbox 360 report: {}",
                        error.message()
                    )));
                }
                Ok(()) => return Ok(()),
            }
        }
    }

    fn close(&mut self) -> Result<(), Error> {
        let result = lock(&self.device).close();
        let mut backend = lock(&self.backend);
        backend.devices.remove(&self.id);
        backend.registry.retain(|(id, _)| *id != self.id);
        result
    }
}
