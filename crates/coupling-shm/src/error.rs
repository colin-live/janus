//! Errors raised by the OS layer.

use std::fmt;

/// Failure of a shared-memory / wakeup operation.
///
/// The variants intentionally mirror `docs/blocks/B2-shm.md`: `NotFound`,
/// `AlreadyExists`, `TooSmall`, `Os(code)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShmError {
    /// The named object (segment or notifier) does not exist.
    NotFound,
    /// The named object already exists (returned by the `create` constructors).
    AlreadyExists,
    /// A region, mapping or destination buffer is too small for the request.
    TooSmall {
        what: &'static str,
        needed: u64,
        have: u64,
    },
    /// Raw OS error code (`GetLastError` on Windows, `errno` on Linux).
    Os(u32),
}

impl ShmError {
    /// Raw OS code behind the error, if any.
    pub fn os_code(&self) -> Option<u32> {
        match self {
            Self::Os(code) => Some(*code),
            _ => None,
        }
    }

    pub(crate) fn too_small(what: &'static str, needed: u64, have: u64) -> Self {
        Self::TooSmall { what, needed, have }
    }
}

impl fmt::Display for ShmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound => write!(f, "named object not found"),
            Self::AlreadyExists => write!(f, "named object already exists"),
            Self::TooSmall { what, needed, have } => {
                write!(f, "{what} too small: need {needed} bytes, have {have}")
            }
            Self::Os(code) => write!(f, "os error {code}"),
        }
    }
}

impl std::error::Error for ShmError {}
