//! A Windows Job Object that kills its processes when the last handle closes,
//! so a crashed or killed App cannot leave the tunnel running.

use std::io;

mod platform {
    use std::io;

    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject,
    };
    use windows::core::PCWSTR;

    pub(crate) struct Job(HANDLE);

    // SAFETY: a job handle is a kernel object reference usable from any thread.
    unsafe impl Send for Job {}
    // SAFETY: the job APIs used here are thread-safe on a shared handle.
    unsafe impl Sync for Job {}

    fn os(error: windows::core::Error) -> io::Error {
        io::Error::from_raw_os_error(error.code().0 & 0xFFFF)
    }

    impl Job {
        pub(crate) fn new() -> io::Result<Self> {
            // SAFETY: no security attributes and an unnamed job.
            let handle = unsafe { CreateJobObjectW(None, PCWSTR::null()) }.map_err(os)?;
            let job = Self(handle);
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            // SAFETY: `limits` is a live value of the documented type and size.
            unsafe {
                SetInformationJobObject(
                    job.0,
                    JobObjectExtendedLimitInformation,
                    (&raw const limits).cast(),
                    size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            }
            .map_err(os)?;
            Ok(job)
        }

        pub(crate) fn assign(&self, process: std::os::windows::io::RawHandle) -> io::Result<()> {
            // SAFETY: `process` is the live handle of a child this process owns.
            unsafe { AssignProcessToJobObject(self.0, HANDLE(process)) }.map_err(os)
        }

        /// Kills every process in the job, including descendants.
        pub(crate) fn terminate(&self) {
            // SAFETY: the handle is owned and open until drop.
            let _ = unsafe { TerminateJobObject(self.0, 1) };
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            // SAFETY: the handle is owned and closed exactly once.
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
}

pub(crate) use platform::Job;

/// Places `child` in `job`; the child is killed if it cannot be contained.
pub(crate) fn contain(job: &Job, child: &mut tokio::process::Child) -> io::Result<()> {
    let result = match child.raw_handle() {
        Some(handle) => job.assign(handle),
        None => Err(io::Error::other(
            "public tunnel process handle is unavailable",
        )),
    };
    if result.is_err() {
        let _ = child.start_kill();
    }
    result
}
