use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreError {
    DuplicateSlotId(u32),
    DuplicateSignal { entity_id: u32, signal_id: u32, direction: &'static str },
    UnknownSignal(u32),
    TypeMismatch { signal_id: u32, expected: &'static str, got: &'static str },
    DirectionMismatch { signal_id: u32 },
    BadMagic,
    UnsupportedVersion { major: u16, minor: u16 },
    BadHeader(&'static str),
    CapacityTooSmall(&'static str),
    BadManifest(String),
}

impl fmt::Display for CoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateSlotId(id) => write!(f, "duplicate slot_id {id}"),
            Self::DuplicateSignal { entity_id, signal_id, direction } => write!(
                f,
                "duplicate signal (entity {entity_id}, signal 0x{signal_id:04X}, {direction})"
            ),
            Self::UnknownSignal(id) => write!(f, "signal 0x{id:04X} not in base registry"),
            Self::TypeMismatch { signal_id, expected, got } => {
                write!(f, "signal 0x{signal_id:04X}: expected {expected}, got {got}")
            }
            Self::DirectionMismatch { signal_id } => {
                write!(f, "signal 0x{signal_id:04X}: direction conflicts with registry class")
            }
            Self::BadMagic => write!(f, "bad segment magic"),
            Self::UnsupportedVersion { major, minor } => {
                write!(f, "unsupported protocol version {major}.{minor}")
            }
            Self::BadHeader(why) => write!(f, "invalid header: {why}"),
            Self::CapacityTooSmall(what) => write!(f, "capacity too small: {what}"),
            Self::BadManifest(why) => write!(f, "invalid manifest: {why}"),
        }
    }
}

impl std::error::Error for CoreError {}
