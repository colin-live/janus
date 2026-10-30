//! Auto-reset binary wakeup event (spec §2).
//!
//! A [`Notifier`] is either created (host) or opened (client / adapter) by name
//! and is process-wide. The protocol never relies on how many times `signal`
//! was called: after waking up, the receiver re-reads `step_id` / `effort_step_id`
//! and treats a repeated id as a spurious wakeup (protocol §7).
//!
//! Windows events are true binary auto-reset objects, so several signals before
//! one `wait` collapse into a single wakeup, as required by B2 requirement 3.
//! POSIX named semaphores are counting; see the status section of
//! `docs/blocks/B2-shm.md` for the resulting platform difference.

use std::time::Duration;

use crate::platform::{Event, WaitResult};
use crate::ShmError;

/// A named wakeup event.
pub struct Notifier {
    event: Event,
}

// SAFETY: a named event is process-wide kernel state; `signal`/`wait` are thread-safe.
unsafe impl Send for Notifier {}
// SAFETY: see above.
unsafe impl Sync for Notifier {}

impl std::fmt::Debug for Notifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Notifier").finish_non_exhaustive()
    }
}

impl Notifier {
    /// Create a new event, fail with [`ShmError::AlreadyExists`] if the name is taken.
    ///
    /// The object is destroyed / unlinked when the creator drops it.
    pub fn create(name: &str) -> Result<Notifier, ShmError> {
        Ok(Notifier {
            event: Event::create(name)?,
        })
    }

    /// Open an existing event, fail with [`ShmError::NotFound`] if it does not exist.
    pub fn open(name: &str) -> Result<Notifier, ShmError> {
        Ok(Notifier {
            event: Event::open(name)?,
        })
    }

    /// Wake the waiting side (`SetEvent` / `sem_post`). Never blocks, never fails
    /// observably.
    pub fn signal(&self) {
        self.event.signal();
    }

    /// Wait until signalled or `timeout` elapses.
    ///
    /// For the first `spin` of the wait the event is polled without parking the
    /// thread (as required by B2 requirement 4); afterwards the thread blocks.
    /// A `timeout` of zero means "poll once".
    pub fn wait(&self, timeout: Duration, spin: Duration) -> WaitResult {
        self.event.wait(timeout, spin)
    }
}
