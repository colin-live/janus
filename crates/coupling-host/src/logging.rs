//! Minimal single-line logger: `[+12.345s] LEVEL message` on stderr. Key events (segment creation, hello,
//! negotiation result, Ready, Error reasons, cleanup) are logged through it.

use std::fmt;
use std::sync::OnceLock;
use std::time::Instant;

static START: OnceLock<Instant> = OnceLock::new();

pub fn init() {
    START.get_or_init(Instant::now);
}

#[doc(hidden)]
pub fn emit(level: &str, args: fmt::Arguments<'_>) {
    let t = START.get_or_init(Instant::now).elapsed();
    eprintln!("[+{:>7.3}s] {:<5} {}", t.as_secs_f64(), level, args);
}

#[macro_export]
macro_rules! log_info {
    ($($arg:tt)*) => { $crate::logging::emit("INFO", format_args!($($arg)*)) };
}

#[macro_export]
macro_rules! log_warn {
    ($($arg:tt)*) => { $crate::logging::emit("WARN", format_args!($($arg)*)) };
}

#[macro_export]
macro_rules! log_error {
    ($($arg:tt)*) => { $crate::logging::emit("ERROR", format_args!($($arg)*)) };
}
