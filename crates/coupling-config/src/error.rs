//! Error type for configuration loading. Every validation error carries a
//! dotted path into the YAML document, e.g.
//! `bindings[0].slots[2].limits: required for effort signal`.

use crate::units::UnitError;
use coupling_core::CoreError;
use std::fmt;
use std::path::PathBuf;

#[derive(Debug)]
pub enum ConfigError {
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    Yaml {
        path: PathBuf,
        source: serde_yaml::Error,
    },
    /// A semantic validation rule failed. `path` points at the offending field.
    Validation {
        path: String,
        message: String,
    },
    Unit {
        path: String,
        source: UnitError,
    },
    /// `Layout::build` rejected an already assembled instance.
    Layout {
        instance: String,
        source: CoreError,
    },
}

impl ConfigError {
    pub(crate) fn validation(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Validation {
            path: path.into(),
            message: message.into(),
        }
    }

    pub(crate) fn unit(path: impl Into<String>, source: UnitError) -> Self {
        Self::Unit {
            path: path.into(),
            source,
        }
    }

    /// Dotted path of the failing field, when the error is a validation error.
    pub fn path(&self) -> Option<&str> {
        match self {
            Self::Validation { path, .. } | Self::Unit { path, .. } => Some(path),
            _ => None,
        }
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(f, "failed to read {}: {source}", path.display()),
            Self::Yaml { path, source } => {
                write!(f, "failed to parse {}: {source}", path.display())
            }
            Self::Validation { path, message } => write!(f, "{path}: {message}"),
            Self::Unit { path, source } => write!(f, "{path}: {source}"),
            Self::Layout { instance, source } => write!(f, "instances.{instance}.slots: {source}"),
        }
    }
}

impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Yaml { source, .. } => Some(source),
            Self::Unit { source, .. } => Some(source),
            Self::Layout { source, .. } => Some(source),
            Self::Validation { .. } => None,
        }
    }
}
