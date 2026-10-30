//! Block B2 — OS layer for the Webots ⇄ model coupling protocol.
//!
//! Provides the named shared-memory segment, the per-instance origin/region
//! accessors and the auto-reset wakeup event defined in `docs/01-protocol-v1.md`
//! (§2, §3). The protocol state machine lives in B4/B5/B6/B8, not here.
//!
//! ```no_run
//! use coupling_core::{Direction, FrameId, Layout, SlotSpec, ValueType};
//! use coupling_shm::{names, Notifier, Segment, WaitResult};
//! use std::time::Duration;
//!
//! let n = names(0x0123_4567_89ab_cdef, 1);
//! let slots = [SlotSpec {
//!     slot_id: 0,
//!     entity_id: 1,
//!     signal_id: 0x0101,
//!     value_type: ValueType::F64,
//!     direction: Direction::ToWebots,
//!     frame_id: FrameId::JointAxis,
//! }];
//! let layout = Layout::build(&slots, 64 * 1024, 16 * 1024, 16 * 1024).unwrap();
//!
//! // host
//! let host = Segment::create(&n.segment, layout.total_size as usize).unwrap();
//! host.init_from_layout(&layout, 0x0123_4567_89ab_cdef, 1, 50).unwrap();
//! let s2m = Notifier::create(&n.s2m).unwrap();
//!
//! // client
//! let model = Segment::open(&n.segment).unwrap();
//! assert_eq!(model.header().simulation_id, 0x0123_4567_89ab_cdef);
//! assert_eq!(s2m.wait(Duration::from_millis(1), Duration::from_micros(50)), WaitResult::TimedOut);
//! ```
//!
//! Platform notes: Windows uses `CreateFileMappingW` / `CreateEventW` in the
//! `Local\` namespace, Linux uses `shm_open` + `mmap` and named POSIX
//! semaphores. Both creators unlink/destroy their object on `Drop`.

mod error;
mod names;
mod notifier;
mod platform;
mod segment;

pub use error::ShmError;
pub use names::{names, SegmentNames};
pub use notifier::Notifier;
pub use platform::WaitResult;
pub use segment::{JsonArea, Segment};

// Re-exported so users of this crate can validate headers / build layouts
// without adding a second dependency line.
pub use coupling_core;
