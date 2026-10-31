//! Block B4: model-side SDK (Rust API, C ABI, Python binding).
//!
//! Requirements: `docs/blocks/B4-client.md`, protocol: `docs/01-protocol-v1.md` §7 (model loop).
//!
//! The Rust API is the reference implementation; [`ffi`] exposes the frozen C names and
//! `python/wcp_client.py` wraps the C ABI with `ctypes`.
//!
//! ```no_run
//! use std::time::Duration;
//! use coupling_client::{ModelClient, WaitEvent};
//! use coupling_core::manifest::Hello;
//!
//! # fn main() -> Result<(), coupling_client::ClientError> {
//! let hello = Hello {
//!     protocol: [1, 0],
//!     model_name: "example".into(),
//!     model_version: "0.1.0".into(),
//!     steppable: true,
//!     sync_modes: vec!["lockstep".into()],
//!     internal_dt_ns: None,
//!     supported_signals: vec![1, 257],
//!     custom_signals: vec![],
//! };
//! let mut client = ModelClient::connect(0x1234, 1, hello, Duration::from_secs(5))?;
//! let pos = client.slot("joint_position").expect("slot from manifest");
//! let torque = client.slot("joint_torque").expect("slot from manifest");
//! match client.wait_step(Duration::from_secs(1))? {
//!     WaitEvent::Step(step) => {
//!         let p = client.read_f64(pos).unwrap_or(0.0);
//!         client.write_f64(torque, -p * 10.0);
//!         client.commit()?;
//!         println!("applied effort for step {}", step.step_id);
//!     }
//!     WaitEvent::Reset { epoch } => {
//!         // Reinitialise all model state here, then acknowledge the reset.
//!         client.ack_reset();
//!         println!("reset to epoch {epoch}");
//!     }
//!     WaitEvent::Timeout => {}
//! }
//! # Ok(())
//! # }
//! ```

mod client;
mod error;
pub mod ffi;

pub use client::{ModelClient, SlotHandle, Step, WaitEvent};
pub use error::ClientError;

/// Protocol re-export so models only need one crate in their `Cargo.toml`.
pub use coupling_core;
