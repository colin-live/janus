//! Model process launching. On Windows every child is put into a Job Object with
//! `KILL_ON_JOB_CLOSE`, so the models disappear with the host even when the host is killed.

use std::path::Path;
use std::process::{Child, Command};

#[cfg(windows)]
mod imp {
    use std::mem::{size_of, zeroed};
    use std::os::windows::io::AsRawHandle;
    use std::process::Child;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    pub struct JobGuard(Option<HANDLE>);

    // SAFETY: a job handle is a process-wide kernel handle; it is only used to assign processes and to close.
    unsafe impl Send for JobGuard {}

    impl JobGuard {
        pub fn new() -> JobGuard {
            // SAFETY: plain Win32 calls with valid arguments; the handle is closed in `Drop`.
            unsafe {
                let h = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if h.is_null() {
                    return JobGuard(None);
                }
                let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = zeroed();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                let ok = SetInformationJobObject(
                    h,
                    JobObjectExtendedLimitInformation,
                    &info as *const _ as *const _,
                    size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                );
                if ok == 0 {
                    CloseHandle(h);
                    return JobGuard(None);
                }
                JobGuard(Some(h))
            }
        }

        pub fn is_active(&self) -> bool {
            self.0.is_some()
        }

        /// Returns false when the job is unavailable or the assignment failed.
        pub fn assign(&self, child: &Child) -> bool {
            match self.0 {
                // SAFETY: both handles are valid for the duration of the call.
                Some(job) => unsafe {
                    AssignProcessToJobObject(job, child.as_raw_handle() as HANDLE) != 0
                },
                None => false,
            }
        }
    }

    impl Drop for JobGuard {
        fn drop(&mut self) {
            if let Some(h) = self.0.take() {
                // SAFETY: `h` was created by `CreateJobObjectW` and is closed exactly once; closing it kills
                // every process still in the job.
                unsafe { CloseHandle(h) };
            }
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use std::process::Child;

    pub struct JobGuard;

    impl JobGuard {
        pub fn new() -> JobGuard {
            JobGuard
        }
        pub fn is_active(&self) -> bool {
            false
        }
        pub fn assign(&self, _child: &Child) -> bool {
            false
        }
    }
}

pub use imp::JobGuard;

impl Default for JobGuard {
    fn default() -> Self {
        Self::new()
    }
}

/// Start a model process with `WCP_SIM_ID` (16 lower-case hex digits) and `WCP_INSTANCE_ID` in its
/// environment (protocol §2).
pub fn spawn_model(
    exe: &Path,
    args: &[String],
    cwd: &Path,
    sim_id: u64,
    instance_id: u32,
    job: &JobGuard,
) -> Result<Child, String> {
    let child = Command::new(exe)
        .args(args)
        .current_dir(cwd)
        .env("WCP_SIM_ID", format!("{sim_id:016x}"))
        .env("WCP_INSTANCE_ID", instance_id.to_string())
        .spawn()
        .map_err(|e| format!("{}: {e}", exe.display()))?;
    if !job.assign(&child) && job.is_active() {
        crate::log_warn!(
            "could not put process {} into the job object; it will not die with the host",
            child.id()
        );
    }
    Ok(child)
}
