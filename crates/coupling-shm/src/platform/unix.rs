//! POSIX implementation: `shm_open` + `mmap` for the segment, named POSIX
//! semaphores for the wakeup, as required by spec §2 / B2 doc.
//!
//! The creator unlinks the kernel names in `Drop`; openers never unlink.

use std::ffi::{c_void, CString};
use std::ptr;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::{logical_len, too_small, WaitResult};
use crate::ShmError;

fn cstring(name: &str) -> Result<CString, ShmError> {
    CString::new(name).map_err(|_| ShmError::Os(libc::EINVAL as u32))
}

fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

fn os_err() -> ShmError {
    ShmError::Os(errno() as u32)
}

fn sem_failed() -> *mut libc::sem_t {
    usize::MAX as *mut libc::sem_t
}

/// An owned shared-memory mapping (and its name, so the creator can unlink it).
pub(crate) struct Mapping {
    view: *mut u8,
    /// Bytes handed out to callers (logical size, see [`logical_len`]).
    len: usize,
    /// Bytes actually mapped; needed for `munmap`.
    mapped: usize,
    name: CString,
    owner: bool,
}

// SAFETY: the mapping is process-shared memory; `Segment` bounds concurrent access
// through atomics / explicit ordering and the protocol's ownership rules.
unsafe impl Send for Mapping {}
// SAFETY: see above.
unsafe impl Sync for Mapping {}

impl Mapping {
    pub(crate) fn create(name: &str, size: usize) -> Result<Mapping, ShmError> {
        if size == 0 {
            return Err(too_small("segment", 1, 0));
        }
        let name = cstring(name)?;
        // SAFETY: `name` is NUL-terminated and outlives the call.
        let fd = unsafe {
            libc::shm_open(
                name.as_ptr(),
                libc::O_CREAT | libc::O_EXCL | libc::O_RDWR,
                0o600 as libc::mode_t,
            )
        };
        if fd < 0 {
            return Err(if errno() == libc::EEXIST {
                ShmError::AlreadyExists
            } else {
                os_err()
            });
        }
        // SAFETY: `fd` is a valid file descriptor owned by this function.
        if unsafe { libc::ftruncate(fd, size as libc::off_t) } != 0 {
            let err = os_err();
            // SAFETY: valid fd and a name we just created.
            unsafe {
                libc::close(fd);
                libc::shm_unlink(name.as_ptr());
            }
            return Err(err);
        }
        // SAFETY: `fd` is open and writable; the mapping is shared and page aligned.
        let view = unsafe {
            libc::mmap(
                ptr::null_mut(),
                size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd,
                0,
            )
        };
        // SAFETY: `fd` is no longer needed once the mapping exists.
        unsafe { libc::close(fd) };
        if view == libc::MAP_FAILED {
            let err = os_err();
            // SAFETY: `name` refers to the object we just created.
            unsafe { libc::shm_unlink(name.as_ptr()) };
            return Err(err);
        }
        Ok(Mapping {
            view: view as *mut u8,
            len: size,
            mapped: size,
            name,
            owner: true,
        })
    }

    pub(crate) fn open(name: &str) -> Result<Mapping, ShmError> {
        let name = cstring(name)?;
        // SAFETY: `name` is NUL-terminated and outlives the call.
        let fd = unsafe { libc::shm_open(name.as_ptr(), libc::O_RDWR, 0o600 as libc::mode_t) };
        if fd < 0 {
            return Err(if errno() == libc::ENOENT {
                ShmError::NotFound
            } else {
                os_err()
            });
        }
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: `fd` is valid and `st` is a valid out-buffer.
        if unsafe { libc::fstat(fd, &mut st) } != 0 {
            let err = os_err();
            // SAFETY: valid fd owned by this function.
            unsafe { libc::close(fd) };
            return Err(err);
        }
        let capacity = st.st_size.max(0) as usize;
        if capacity == 0 {
            // SAFETY: valid fd owned by this function.
            unsafe { libc::close(fd) };
            return Err(too_small("segment", 256, 0));
        }
        // SAFETY: `fd` is open; mapping `capacity` bytes of the shared object.
        let view = unsafe {
            libc::mmap(
                ptr::null_mut(),
                capacity,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd,
                0,
            )
        };
        // SAFETY: the descriptor is not needed after a successful mapping.
        unsafe { libc::close(fd) };
        if view == libc::MAP_FAILED {
            return Err(os_err());
        }
        let ptr = view as *mut u8;
        if capacity < 256 {
            // SAFETY: the mapping was created above and is no longer used.
            unsafe { libc::munmap(view, capacity) };
            return Err(too_small("segment", 256, capacity));
        }
        let len = logical_len(ptr, capacity);
        Ok(Mapping {
            view: ptr,
            len,
            mapped: capacity,
            name,
            owner: false,
        })
    }

    pub(crate) fn ptr(&self) -> *mut u8 {
        self.view
    }

    pub(crate) fn len(&self) -> usize {
        self.len
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: the view/mapping pair came from `mmap`/`shm_open`; both are released
        // exactly once here, and only the creator unlinks the name.
        unsafe {
            libc::munmap(self.view as *mut c_void, self.mapped);
            if self.owner {
                libc::shm_unlink(self.name.as_ptr());
            }
        }
    }
}

/// Named POSIX semaphore posing as the auto-reset binary event of the protocol.
pub(crate) struct Event {
    sem: *mut libc::sem_t,
    name: CString,
    owner: bool,
}

// SAFETY: POSIX semaphores are process-shared and may be used from any thread.
unsafe impl Send for Event {}
// SAFETY: see above.
unsafe impl Sync for Event {}

impl Event {
    pub(crate) fn create(name: &str) -> Result<Event, ShmError> {
        let name = cstring(name)?;
        // SAFETY: `name` is NUL-terminated and outlives the call; initial value 0.
        let sem = unsafe {
            libc::sem_open(
                name.as_ptr(),
                libc::O_CREAT | libc::O_EXCL,
                0o600 as libc::mode_t,
                0 as libc::c_uint,
            )
        };
        if sem == sem_failed() {
            return Err(if errno() == libc::EEXIST {
                ShmError::AlreadyExists
            } else {
                os_err()
            });
        }
        Ok(Event {
            sem,
            name,
            owner: true,
        })
    }

    pub(crate) fn open(name: &str) -> Result<Event, ShmError> {
        let name = cstring(name)?;
        // SAFETY: `name` is NUL-terminated and outlives the call.
        let sem = unsafe { libc::sem_open(name.as_ptr(), 0) };
        if sem == sem_failed() {
            return Err(if errno() == libc::ENOENT {
                ShmError::NotFound
            } else {
                os_err()
            });
        }
        Ok(Event {
            sem,
            name,
            owner: false,
        })
    }

    pub(crate) fn signal(&self) {
        // SAFETY: `sem` is a valid semaphore handle owned by `self`. A full semaphore
        // (count > SEM_VALUE_MAX) is harmless: the protocol never relies on counts.
        unsafe { libc::sem_post(self.sem) };
    }

    pub(crate) fn wait(&self, timeout: Duration, spin: Duration) -> WaitResult {
        let start = Instant::now();
        let deadline = start.checked_add(timeout).unwrap_or(start);

        if !spin.is_zero() {
            let spin_deadline = start.checked_add(spin).unwrap_or(deadline).min(deadline);
            while Instant::now() < spin_deadline {
                if self.try_wait() {
                    return WaitResult::Signaled;
                }
                std::hint::spin_loop();
            }
        }

        if self.try_wait() {
            return WaitResult::Signaled;
        }

        loop {
            let now = Instant::now();
            if now >= deadline {
                return WaitResult::TimedOut;
            }
            let ts = abs_timespec(deadline - now);
            // SAFETY: `sem` is valid and `ts` is a valid absolute timespec.
            let rc = unsafe { libc::sem_timedwait(self.sem, &ts) };
            if rc == 0 {
                return WaitResult::Signaled;
            }
            match errno() {
                libc::EINTR => continue,
                // `wait` has no error channel in the frozen interface; report an
                // unexpected failure as a timeout so callers fall back to their
                // protocol-level timeout handling.
                _ => return WaitResult::TimedOut,
            }
        }
    }

    fn try_wait(&self) -> bool {
        loop {
            // SAFETY: `sem` is a valid semaphore handle owned by `self`.
            if unsafe { libc::sem_trywait(self.sem) } == 0 {
                return true;
            }
            if errno() != libc::EINTR {
                return false;
            }
        }
    }
}

impl Drop for Event {
    fn drop(&mut self) {
        // SAFETY: `sem` came from `sem_open` and is closed exactly once; only the
        // creator unlinks the name.
        unsafe {
            libc::sem_close(self.sem);
            if self.owner {
                libc::sem_unlink(self.name.as_ptr());
            }
        }
    }
}

fn abs_timespec(d: Duration) -> libc::timespec {
    let at = SystemTime::now()
        .checked_add(d)
        .unwrap_or(SystemTime::now());
    let since = at.duration_since(UNIX_EPOCH).unwrap_or_default();
    libc::timespec {
        tv_sec: since.as_secs() as libc::time_t,
        tv_nsec: since.subsec_nanos() as libc::c_long,
    }
}
