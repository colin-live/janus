//! Wire-level enums. Numeric values are part of the protocol and MUST NOT change within a major version.

macro_rules! wire_enum {
    ($(#[$m:meta])* $name:ident : $repr:ty { $($(#[$vm:meta])* $var:ident = $val:expr),+ $(,)? }) => {
        $(#[$m])*
        #[repr($repr)]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $name { $($(#[$vm])* $var = $val),+ }
        impl $name {
            pub fn from_raw(v: $repr) -> Option<Self> {
                match v { $($val => Some(Self::$var),)+ _ => None }
            }
            pub fn as_raw(self) -> $repr { self as $repr }
        }
    };
}

wire_enum!(
    /// Value encoding of one slot. All little-endian.
    ValueType: u8 {
        Bool = 1,
        I32 = 2,
        U32 = 3,
        I64 = 4,
        F32 = 5,
        F64 = 6,
        /// 3 x f64
        Vec3F64 = 7,
        /// x, y, z, w
        QuatF64 = 8,
        /// row-major
        Mat3F64 = 9,
    }
);

impl ValueType {
    pub fn size(self) -> usize {
        match self {
            Self::Bool => 1,
            Self::I32 | Self::U32 | Self::F32 => 4,
            Self::I64 | Self::F64 => 8,
            Self::Vec3F64 => 24,
            Self::QuatF64 => 32,
            Self::Mat3F64 => 72,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Bool => "bool",
            Self::I32 => "i32",
            Self::U32 => "u32",
            Self::I64 => "i64",
            Self::F32 => "f32",
            Self::F64 => "f64",
            Self::Vec3F64 => "vec3f64",
            Self::QuatF64 => "quatf64",
            Self::Mat3F64 => "mat3f64",
        }
    }
}

wire_enum!(
    /// Data flow direction of a slot; decides the region and which side owns writes.
    Direction: u8 {
        /// Webots adapter writes, model reads (state / observation).
        ToModel = 0,
        /// Model writes, adapter reads (effort).
        ToWebots = 1,
    }
);

wire_enum!(
    /// Per-slot validity byte. Zero-initialised memory is `Missing`, never "valid zero".
    Validity: u8 {
        Missing = 0,
        Valid = 1,
        Invalid = 2,
        Stale = 3,
        OutOfRange = 4,
        Error = 5,
    }
);

wire_enum!(
    SignalClass: u8 {
        State = 1,
        Observation = 2,
        Effort = 3,
        Parameter = 4,
    }
);

wire_enum!(
    FrameId: u16 {
        None = 0,
        World = 1,
        BodyLocal = 2,
        JointAxis = 3,
        ModelLocal = 4,
    }
);

wire_enum!(
    SyncMode: u32 {
        Lockstep = 0,
        Async = 1,
    }
);

wire_enum!(
    /// Segment lifecycle, stored in header.hs_state.
    HandshakeState: u32 {
        /// host built the segment + manifest
        Created = 0,
        /// client wrote its hello
        Hello = 1,
        /// host accepted; adapter may start stepping
        Ready = 2,
        Running = 3,
        Paused = 4,
        /// epoch bump in progress
        Resetting = 5,
        Error = 6,
        Closed = 7,
    }
);
