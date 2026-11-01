//! Error type of the host. Each variant maps to a process exit code (B5 doc):
//! 0 ok, 1 configuration / validation, 2 handshake, 3 fatal runtime error.

use std::fmt;

use coupling_config::ConfigError;
use coupling_shm::ShmError;

#[derive(Debug)]
pub enum HostError {
    /// Bad command line.
    Usage(String),
    /// `project.yaml` failed to load.
    Config(ConfigError),
    /// Offline validation (e.g. world DEFs) found problems.
    Validation(Vec<String>),
    /// Creating or using shared memory / events failed.
    Shm { context: String, source: ShmError },
    /// A model process could not be started.
    Spawn { instance: String, message: String },
    /// The handshake with an instance failed (no hello, negotiation refused, model exited, ...).
    Handshake { instance: String, reason: String },
    /// Any other fatal runtime problem.
    Runtime(String),
}

impl HostError {
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Usage(_) | Self::Config(_) | Self::Validation(_) => 1,
            Self::Spawn { .. } | Self::Handshake { .. } => 2,
            Self::Shm { .. } | Self::Runtime(_) => 3,
        }
    }

    pub(crate) fn shm(context: impl Into<String>, source: ShmError) -> Self {
        Self::Shm {
            context: context.into(),
            source,
        }
    }
}

impl fmt::Display for HostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage(m) => write!(f, "usage: {m}"),
            Self::Config(e) => write!(f, "{e}"),
            Self::Validation(list) => {
                write!(f, "validation failed:")?;
                for item in list {
                    write!(f, "\n  - {item}")?;
                }
                Ok(())
            }
            Self::Shm { context, source } => write!(f, "{context}: {source}"),
            Self::Spawn { instance, message } => {
                write!(f, "instance {instance}: cannot start model: {message}")
            }
            Self::Handshake { instance, reason } => {
                write!(f, "instance {instance}: handshake failed: {reason}")
            }
            Self::Runtime(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for HostError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Config(e) => Some(e),
            Self::Shm { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl From<ConfigError> for HostError {
    fn from(e: ConfigError) -> Self {
        Self::Config(e)
    }
}
