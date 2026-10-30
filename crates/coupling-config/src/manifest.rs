//! Manifest value types (`docs/01-protocol-v1.md` §8).
//!
//! The canonical definitions live in `coupling_core::manifest` (B1.1). They are re-exported here so
//! existing `coupling_config::{Manifest, ...}` paths keep working.

pub use coupling_core::manifest::{
    EffortPolicy, Entity, Limits, Manifest, SlotEntry, MANIFEST_VERSION,
};
