//! Block B3: YAML project/binding/model config parsing, validation and SI conversion.
//!
//! Requirements: `docs/blocks/B3-config.md`; schema: `docs/02-config-schema.md`.
//!
//! ```no_run
//! use coupling_config::load_project;
//! use std::path::Path;
//!
//! let project = load_project(Path::new("examples/arm2dof/project.yaml")).unwrap();
//! let manifest = project.instances[0].to_manifest(&project.sim, 0x0123_4567_89ab_cdef);
//! let json = serde_json::to_string_pretty(&manifest).unwrap();
//! println!("{json}");
//! ```

pub mod config;
pub mod error;
pub mod manifest;
pub mod units;
pub mod world;

pub use config::{load_project, Launch, ResolvedInstance, ResolvedProject, SimSettings, SlotMeta};
pub use error::ConfigError;
pub use manifest::{EffortPolicy, Entity, Limits, Manifest, SlotEntry, MANIFEST_VERSION};
pub use units::{Dim, UnitError};
pub use world::check_world_defs;

// Re-exported so downstream blocks can build `SlotSpec`s without a second
// dependency line, mirroring the convention used by `coupling-shm`.
pub use coupling_core;
