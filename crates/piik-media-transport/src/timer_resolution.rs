//! Windows wait granularity for str0m's millisecond pacing and probe deadlines.

use windows::Win32::Media::{TIMERR_NOERROR, timeBeginPeriod, timeEndPeriod};

const PERIOD_MS: u32 = 1;

pub(crate) struct TimerResolution {
    requested: bool,
}

impl TimerResolution {
    #[allow(unsafe_code)]
    pub fn new() -> Self {
        // SAFETY: no pointers; each successful request has one matching release.
        // Windows 10 2004+ scopes this to requesting processes, not the system.
        let requested = unsafe { timeBeginPeriod(PERIOD_MS) } == TIMERR_NOERROR;
        Self { requested }
    }
}

impl Drop for TimerResolution {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        if self.requested {
            // SAFETY: balances this instance's successful request at the same period.
            unsafe { timeEndPeriod(PERIOD_MS) };
        }
    }
}
