//! Block B1 — protocol contract. Pure `std`, no OS calls, no I/O.
//! Normative spec: `docs/01-protocol-v1.md`. If code and spec disagree, the spec wins
//! and the code is a bug (or the spec needs a versioned change).

pub mod error;
pub mod header;
pub mod layout;
pub mod manifest;
pub mod registry;
pub mod types;

pub use error::CoreError;
pub use header::{SegmentHeader, HEADER_SIZE, MAGIC, PROTOCOL_MAJOR, PROTOCOL_MINOR};
pub use layout::{Layout, SlotLayout, SlotSpec};
pub use types::{Direction, FrameId, HandshakeState, SignalClass, SyncMode, Validity, ValueType};
