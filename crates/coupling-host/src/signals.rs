//! Ctrl-C / termination handling: sets a process-wide flag that the supervision loop polls.

use std::sync::atomic::{AtomicBool, Ordering};

static STOP: AtomicBool = AtomicBool::new(false);

pub fn stop_requested() -> bool {
    STOP.load(Ordering::Relaxed)
}

pub fn request_stop() {
    STOP.store(true, Ordering::Relaxed);
}

#[cfg(windows)]
pub fn install() {
    use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;

    // CTRL_C_EVENT = 0, CTRL_BREAK_EVENT = 1, CTRL_CLOSE_EVENT = 2, CTRL_LOGOFF_EVENT = 5, CTRL_SHUTDOWN_EVENT = 6
    unsafe extern "system" fn handler(kind: u32) -> i32 {
        match kind {
            0 | 1 | 2 | 5 | 6 => {
                request_stop();
                1
            }
            _ => 0,
        }
    }
    // SAFETY: `handler` has the required signature and only touches an atomic.
    unsafe { SetConsoleCtrlHandler(Some(handler), 1) };
}

#[cfg(unix)]
pub fn install() {
    extern "C" fn handler(_sig: libc::c_int) {
        request_stop();
    }
    // SAFETY: the handler only stores to an atomic, which is async-signal-safe.
    unsafe {
        libc::signal(libc::SIGINT, handler as libc::sighandler_t);
        libc::signal(libc::SIGTERM, handler as libc::sighandler_t);
    }
}
