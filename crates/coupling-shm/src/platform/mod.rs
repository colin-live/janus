//! Platform switch for the raw OS primitives.
//!
//! The public API in [`crate::segment`] and [`crate::notifier`] is platform
//! independent; only the small `Mapping` / `Event` types below are per-OS.

use crate::ShmError;
use coupling_core::header::{offsets, HEADER_SIZE, MAGIC};

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
pub(crate) use unix::{Event, Mapping};
#[cfg(windows)]
pub(crate) use windows::{Event, Mapping};

/// Result of [`crate::Notifier::wait`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitResult {
    /// The event was (or became) signalled.
    Signaled,
    /// The timeout elapsed first.
    TimedOut,
}

/// Logical size of a mapped view.
///
/// A creator knows the exact size; an opener has to read it back. The header's
/// `total_size` is authoritative once the mapping carries a valid magic (the
/// host writes `magic` last and then publishes `hs_state=Created`), otherwise
/// the capacity reported by the OS is used.
pub(crate) fn logical_len(view: *mut u8, capacity: usize) -> usize {
    if capacity < HEADER_SIZE {
        return capacity;
    }
    // SAFETY: `capacity >= HEADER_SIZE`, so the fixed 256-byte header lies inside the
    // mapping, and `view` is the base pointer of a page-aligned shared mapping.
    let magic = unsafe { std::slice::from_raw_parts(view as *const u8, MAGIC.len()) };
    if magic != &MAGIC[..] {
        return capacity;
    }
    // SAFETY: `TOTAL_SIZE` is 16, i.e. inside the 256-byte header we just verified, and
    // `read_unaligned` does not require alignment.
    let total = unsafe { std::ptr::read_unaligned(view.add(offsets::TOTAL_SIZE) as *const u64) };
    if total >= HEADER_SIZE as u64 && total <= capacity as u64 {
        total as usize
    } else {
        capacity
    }
}

/// Convert a duration into whole milliseconds, rounding up (0 stays 0).
/// Only the Windows timeout path needs this; Linux passes a `timespec`.
#[cfg(windows)]
pub(crate) fn ceil_ms(d: std::time::Duration) -> u32 {
    if d.is_zero() {
        return 0;
    }
    let ms = d.as_nanos().div_ceil(1_000_000);
    ms.min(u32::MAX as u128) as u32
}

pub(crate) fn too_small(what: &'static str, needed: usize, have: usize) -> ShmError {
    ShmError::too_small(what, needed as u64, have as u64)
}
