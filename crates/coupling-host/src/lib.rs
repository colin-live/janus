//! Block B5 — `coupling-host`: creates the per-instance shared-memory segments of a project, launches the
//! model processes, runs the handshake/negotiation (protocol §6) and supervises the instances.
//!
//! The host never takes part in the per-step data exchange; that is between the Webots adapter and the
//! models. Requirements: `docs/blocks/B5-host.md`, protocol: `docs/01-protocol-v1.md`.

pub mod cli;
pub mod error;
pub mod host;
pub mod logging;
pub mod negotiate;
pub mod process;
pub mod runfile;
pub mod signals;
pub mod validate;
pub mod watch;

pub use error::HostError;
pub use host::{Host, HostOptions, TickOutcome};
