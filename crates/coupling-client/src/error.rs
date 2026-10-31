//! Errors raised by the model-side SDK.

use std::fmt;

use coupling_core::CoreError;
use coupling_shm::ShmError;

/// Failure of a client operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientError {
    /// Shared-memory / wakeup layer failure.
    Shm(ShmError),
    /// Protocol-layer failure while rebuilding the layout from the manifest.
    Core(CoreError),
    /// Invalid JSON in the manifest, hello or report area.
    Json(String),
    /// A deadline expired; the string names the phase.
    Timeout(&'static str),
    /// The peer violated the handshake / ownership rules.
    BadState(String),
    /// The host or adapter reported `hs_state = Error` (report content included when present).
    Remote(String),
    /// The segment was closed while waiting for a step.
    Closed,
    /// `wait_step` was called before the previous step was committed.
    StepNotCommitted,
    /// `commit` was called without a step from `wait_step`, or twice for the same step.
    NoPendingStep,
    /// `ack_reset` / `try_ack_reset` was called without a pending reset event.
    NoPendingReset,
    /// A write/invalidate targeted a slot that is not writable for that operation.
    WrongSlot(String),
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Shm(e) => write!(f, "shared memory: {e}"),
            Self::Core(e) => write!(f, "protocol: {e}"),
            Self::Json(why) => write!(f, "invalid JSON: {why}"),
            Self::Timeout(phase) => write!(f, "timed out: {phase}"),
            Self::BadState(why) => write!(f, "bad state: {why}"),
            Self::Remote(why) => write!(f, "peer reported an error: {why}"),
            Self::Closed => write!(f, "segment closed"),
            Self::StepNotCommitted => {
                write!(f, "wait_step called before committing the previous step")
            }
            Self::NoPendingStep => write!(f, "commit called without a pending step"),
            Self::NoPendingReset => write!(f, "ack_reset called without a pending reset"),
            Self::WrongSlot(why) => write!(f, "slot misuse: {why}"),
        }
    }
}

impl std::error::Error for ClientError {}

impl From<ShmError> for ClientError {
    fn from(e: ShmError) -> Self {
        Self::Shm(e)
    }
}

impl From<CoreError> for ClientError {
    fn from(e: CoreError) -> Self {
        Self::Core(e)
    }
}
