//! The opt-in ViGEmBus lifecycle gate (`PIIK_GAMEPAD_GATE=1`). XInput is
//! read-only observation here.

use std::time::{Duration, Instant};

use windows::Win32::Foundation::ERROR_NOT_SUPPORTED;
use windows::Win32::UI::Input::XboxController::{XINPUT_STATE, XInputGetState};

use super::vigem::{VigemBackend, VigemGamepad};
use crate::gamepad::{
    GamepadBackend, GamepadDevice, GamepadReport, VIGEM_CHECK_VERSION, VIGEM_UNPLUG, VigemRequest,
    XusbReport, to_xusb_report,
};

fn read(index: u32) -> Option<XusbReport> {
    let mut state = XINPUT_STATE::default();
    // SAFETY: `state` outlives the call.
    if unsafe { XInputGetState(index, &mut state) } != 0 {
        return None;
    }
    let pad = state.Gamepad;
    Some(XusbReport {
        buttons: pad.wButtons.0,
        left_trigger: pad.bLeftTrigger,
        right_trigger: pad.bRightTrigger,
        left_x: pad.sThumbLX,
        left_y: pad.sThumbLY,
        right_x: pad.sThumbRX,
        right_y: pad.sThumbRY,
    })
}

fn find(want: XusbReport) -> Option<u32> {
    (0..4).find(|index| read(*index) == Some(want))
}

fn wait(mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !check() {
        assert!(
            Instant::now() < deadline,
            "temporary gamepad state did not converge in XInput"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn control(
    handle: windows::Win32::Foundation::HANDLE,
    code: u32,
    value: u32,
) -> windows::core::Result<()> {
    let request = VigemRequest { size: 8, value };
    let mut transferred = 0u32;
    // SAFETY: `request` is a live 8-byte METHOD_BUFFERED input.
    unsafe {
        windows::Win32::System::IO::DeviceIoControl(
            handle,
            code,
            Some((&raw const request).cast()),
            8,
            None,
            0,
            Some(&mut transferred),
            None,
        )
    }
}

pub(crate) fn run() {
    let first = VigemBackend::open().expect("first ViGEmBus session");
    let second = VigemBackend::open().expect("second ViGEmBus session");
    let open =
        |backend: &VigemBackend| -> VigemGamepad { backend.create().expect("create gamepad") };
    let (mut a, mut b) = (open(&first), open(&second));
    let report_a = GamepadReport {
        buttons: 1,
        left_trigger: 61,
        right_trigger: 17,
        left_x: -17000,
        left_y: 7100,
        right_x: -3900,
        right_y: 12400,
    };
    let report_b = GamepadReport {
        buttons: 2,
        left_trigger: 23,
        right_trigger: 183,
        left_x: 8300,
        left_y: 20000,
        right_x: -9100,
        right_y: -13100,
    };
    // PnP enumeration finishes asynchronously; repeat full reports as the
    // input stream does instead of treating the first write as readiness.
    wait(|| {
        a.update(report_a).unwrap();
        b.update(report_b).unwrap();
        find(to_xusb_report(report_a)).is_some() && find(to_xusb_report(report_b)).is_some()
    });
    let index_a = find(to_xusb_report(report_a)).unwrap();
    let index_b = find(to_xusb_report(report_b)).unwrap();
    let error = control(first.bus_handle(), VIGEM_CHECK_VERSION, 2)
        .expect_err("unsupported protocol accepted");
    assert_eq!(error.code(), ERROR_NOT_SUPPORTED.to_hresult());
    // The driver may acknowledge this, but must not remove another file
    // session's target. Both pads were created by this test.
    let _ = control(second.bus_handle(), VIGEM_UNPLUG, a.serial());
    wait(|| {
        read(index_a) == Some(to_xusb_report(report_a))
            && read(index_b) == Some(to_xusb_report(report_b))
    });
    a.update(GamepadReport::default()).unwrap();
    wait(|| {
        read(index_a) == Some(XusbReport::default())
            && read(index_b) == Some(to_xusb_report(report_b))
    });
    a.close().unwrap();
    wait(|| read(index_a).is_none() && read(index_b) == Some(to_xusb_report(report_b)));
    assert!(
        a.update(report_a).is_err(),
        "closed device accepted an update"
    );
    a.close().unwrap();
    let mut a = open(&first);
    wait(|| {
        a.update(report_a).unwrap();
        find(to_xusb_report(report_a)).is_some()
    });
    let index_a = find(to_xusb_report(report_a)).unwrap();
    first.close().unwrap();
    wait(|| read(index_a).is_none() && read(index_b) == Some(to_xusb_report(report_b)));
    assert!(first.open().is_err(), "closed backend created a device");
    assert!(
        a.update(report_a).is_err(),
        "backend retirement left its device writable"
    );
    a.close().unwrap();
    b.update(GamepadReport::default()).unwrap();
    wait(|| read(index_b) == Some(XusbReport::default()));
    b.close().unwrap();
    wait(|| read(index_b).is_none());
    second.close().unwrap();
}
