//! Windows implementation: `CreateFileMappingW` / `CreateEventW` and friends.
//!
//! Objects live in the session-local `Local\` namespace (spec §2), so two
//! processes of the same user session can share them without global privileges.

use std::ffi::{c_void, OsStr};
use std::os::windows::ffi::OsStrExt;
use std::ptr;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND, ERROR_NOT_FOUND, HANDLE,
    INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::System::Memory::{
    CreateFileMappingW, MapViewOfFile, OpenFileMappingW, UnmapViewOfFile, VirtualQuery,
    FILE_MAP_ALL_ACCESS, MEMORY_BASIC_INFORMATION, MEMORY_MAPPED_VIEW_ADDRESS, PAGE_READWRITE,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, OpenEventW, SetEvent, WaitForSingleObject, EVENT_ALL_ACCESS,
};

use super::{ceil_ms, logical_len, too_small, WaitResult};
use crate::ShmError;

fn wide(name: &str) -> Vec<u16> {
    OsStr::new(name)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn last_error() -> u32 {
    // SAFETY: `GetLastError` has no preconditions and only reads thread-local state.
    unsafe { GetLastError() }
}

/// An owned section handle plus its mapped view.
pub(crate) struct Mapping {
    handle: HANDLE,
    view: *mut u8,
    len: usize,
}

// SAFETY: the mapping is process-wide shared memory; all mutating accesses go
// through atomics or explicit ordering in `Segment`. The handle itself is a plain
// kernel handle that may be used from any thread.
unsafe impl Send for Mapping {}
// SAFETY: see above; concurrent access is bounded by the protocol's ownership rules.
unsafe impl Sync for Mapping {}

impl Mapping {
    pub(crate) fn create(name: &str, size: usize) -> Result<Mapping, ShmError> {
        if size == 0 {
            return Err(too_small("segment", 1, 0));
        }
        if size > u32::MAX as usize {
            return Err(too_small("segment", size, u32::MAX as usize));
        }
        let wname = wide(name);
        // SAFETY: `wname` is NUL-terminated and outlives the call; a null security
        // descriptor means "default" and page-file backed mapping (INVALID_HANDLE_VALUE).
        let handle = unsafe {
            CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                ptr::null(),
                PAGE_READWRITE,
                0,
                size as u32,
                wname.as_ptr(),
            )
        };
        if handle.is_null() {
            return Err(ShmError::Os(last_error()));
        }
        if last_error() == ERROR_ALREADY_EXISTS {
            // SAFETY: `handle` is a valid handle returned by `CreateFileMappingW`.
            unsafe { CloseHandle(handle) };
            return Err(ShmError::AlreadyExists);
        }
        // SAFETY: `handle` is valid and open; mapping the whole section is requested with 0.
        let view = unsafe { MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, 0) };
        if view.Value.is_null() {
            let code = last_error();
            // SAFETY: `handle` is still open here.
            unsafe { CloseHandle(handle) };
            return Err(ShmError::Os(code));
        }
        Ok(Mapping {
            handle,
            view: view.Value as *mut u8,
            len: size,
        })
    }

    pub(crate) fn open(name: &str) -> Result<Mapping, ShmError> {
        let wname = wide(name);
        // SAFETY: `wname` is NUL-terminated and outlives the call.
        let handle = unsafe { OpenFileMappingW(FILE_MAP_ALL_ACCESS, 0, wname.as_ptr()) };
        if handle.is_null() {
            let code = last_error();
            return Err(if code == ERROR_FILE_NOT_FOUND || code == ERROR_NOT_FOUND {
                ShmError::NotFound
            } else {
                ShmError::Os(code)
            });
        }
        // SAFETY: `handle` is valid and open.
        let view = unsafe { MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, 0) };
        if view.Value.is_null() {
            let code = last_error();
            // SAFETY: `handle` is still open here.
            unsafe { CloseHandle(handle) };
            return Err(ShmError::Os(code));
        }
        let ptr = view.Value as *mut u8;
        let capacity = view_capacity(ptr);
        if capacity < 256 {
            // SAFETY: the view was mapped successfully above.
            unsafe {
                UnmapViewOfFile(view);
                CloseHandle(handle);
            }
            return Err(too_small("segment", 256, capacity));
        }
        let len = logical_len(ptr, capacity);
        Ok(Mapping {
            handle,
            view: ptr,
            len,
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
        // SAFETY: `view` and `handle` were obtained from a successful
        // `MapViewOfFile` / `CreateFileMappingW` or `OpenFileMappingW` pair and are
        // released exactly once here.
        unsafe {
            UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS {
                Value: self.view as *mut c_void,
            });
            CloseHandle(self.handle);
        }
    }
}

/// Size of the mapped view as reported by the memory manager (page rounded,
/// possibly allocation-granularity rounded). Only used to bound the size read
/// back from the header.
fn view_capacity(view: *mut u8) -> usize {
    let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: `info` is a properly sized writable out-buffer and `view` points into
    // memory we own.
    let written = unsafe {
        VirtualQuery(
            view as *const c_void,
            &mut info,
            std::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
        )
    };
    if written == 0 {
        0
    } else {
        info.RegionSize
    }
}

/// Auto-reset binary event.
pub(crate) struct Event {
    handle: HANDLE,
}

// SAFETY: a kernel event handle is usable from any thread; auto-reset semantics are
// enforced by the kernel.
unsafe impl Send for Event {}
// SAFETY: see above.
unsafe impl Sync for Event {}

impl Event {
    pub(crate) fn create(name: &str) -> Result<Event, ShmError> {
        let wname = wide(name);
        // SAFETY: `wname` is NUL-terminated and outlives the call; manual reset = FALSE
        // gives the auto-reset binary event required by the protocol.
        let handle = unsafe { CreateEventW(ptr::null(), 0, 0, wname.as_ptr()) };
        if handle.is_null() {
            return Err(ShmError::Os(last_error()));
        }
        if last_error() == ERROR_ALREADY_EXISTS {
            // SAFETY: valid handle obtained above.
            unsafe { CloseHandle(handle) };
            return Err(ShmError::AlreadyExists);
        }
        Ok(Event { handle })
    }

    pub(crate) fn open(name: &str) -> Result<Event, ShmError> {
        let wname = wide(name);
        // SAFETY: `wname` is NUL-terminated and outlives the call.
        let handle = unsafe { OpenEventW(EVENT_ALL_ACCESS, 0, wname.as_ptr()) };
        if handle.is_null() {
            let code = last_error();
            return Err(if code == ERROR_FILE_NOT_FOUND || code == ERROR_NOT_FOUND {
                ShmError::NotFound
            } else {
                ShmError::Os(code)
            });
        }
        Ok(Event { handle })
    }

    pub(crate) fn signal(&self) {
        // SAFETY: `handle` is a valid event handle owned by `self`.
        unsafe { SetEvent(self.handle) };
    }

    pub(crate) fn wait(&self, timeout: Duration, spin: Duration) -> WaitResult {
        let start = Instant::now();
        let deadline = start.checked_add(timeout).unwrap_or(start);

        // Spin phase. A Windows event offers no locally observable state, so the only
        // non-blocking check is a zero-timeout wait; the observable behaviour is the
        // same as a blocking wait, just without parking the thread.
        if !spin.is_zero() {
            let spin_deadline = start.checked_add(spin).unwrap_or(deadline).min(deadline);
            while Instant::now() < spin_deadline {
                if self.poll() {
                    return WaitResult::Signaled;
                }
                std::hint::spin_loop();
            }
        }

        if self.poll() {
            return WaitResult::Signaled;
        }
        self.wait_until(deadline)
    }

    /// Blocking wait with a precise deadline.
    ///
    /// `WaitForSingleObject` timeouts are rounded up to the system timer tick
    /// (up to ~15.6 ms by default, so `WaitForSingleObject(10ms)` can return at
    /// 20.6 ms), which would break the "< 5 ms error" requirement. We therefore
    /// only block for the bulk of the remaining time, keeping a 20 ms guard band
    /// that is polled with `yield_now()` instead.
    fn wait_until(&self, deadline: Instant) -> WaitResult {
        const GUARD: Duration = Duration::from_millis(20);
        loop {
            let now = Instant::now();
            if now >= deadline {
                return WaitResult::TimedOut;
            }
            let remaining = deadline - now;
            if remaining > GUARD {
                let slice = ceil_ms(remaining - GUARD).max(1);
                // SAFETY: valid event handle owned by `self`.
                match unsafe { WaitForSingleObject(self.handle, slice) } {
                    WAIT_OBJECT_0 => return WaitResult::Signaled,
                    WAIT_TIMEOUT => {}
                    _ => return WaitResult::TimedOut,
                }
            } else {
                if self.poll() {
                    return WaitResult::Signaled;
                }
                std::thread::yield_now();
            }
        }
    }

    fn poll(&self) -> bool {
        // SAFETY: valid event handle owned by `self`; timeout 0 never blocks.
        unsafe { WaitForSingleObject(self.handle, 0) == WAIT_OBJECT_0 }
    }
}

impl Drop for Event {
    fn drop(&mut self) {
        // SAFETY: `handle` was returned by `CreateEventW`/`OpenEventW` and is closed
        // exactly once.
        unsafe { CloseHandle(self.handle) };
    }
}
