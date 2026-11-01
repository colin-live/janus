//! Block B6 — mock Webots master and reference plant.
//!
//! The `master` module is the Rust reference implementation of the adapter side of
//! protocol §7 and §10. The Python adapter (B8) must behave identically; the shared
//! vectors in `tests/vectors/effort_validation.json` are consumed by both.
//!
//! The `plant` module is a tiny rotary inertia used by the `mock-webots` executable
//! so B4/B5/B7 can run a closed loop without Webots.

pub mod master;
pub mod plant;
