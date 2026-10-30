//! Block B1.1 — JSON control-plane types: manifest (protocol §8), hello (§9), report (§6).
//!
//! These are init-time only; runtime data never goes through JSON. Unknown fields are ignored
//! on read (forward compatibility, protocol §11). Field names MUST match the protocol text.

use crate::error::CoreError;
use crate::layout::SlotSpec;
use crate::types::{Direction, FrameId, ValueType};
use serde::{Deserialize, Serialize};

pub const MANIFEST_VERSION: u32 = 1;

// ---------------------------------------------------------------- name <-> enum helpers

pub fn value_type_from_name(s: &str) -> Option<ValueType> {
    Some(match s {
        "bool" => ValueType::Bool,
        "i32" => ValueType::I32,
        "u32" => ValueType::U32,
        "i64" => ValueType::I64,
        "f32" => ValueType::F32,
        "f64" => ValueType::F64,
        "vec3f64" => ValueType::Vec3F64,
        "quatf64" => ValueType::QuatF64,
        "mat3f64" => ValueType::Mat3F64,
        _ => return None,
    })
}

pub fn direction_from_name(s: &str) -> Option<Direction> {
    match s {
        "to_model" => Some(Direction::ToModel),
        "to_webots" => Some(Direction::ToWebots),
        _ => None,
    }
}

pub fn direction_name(d: Direction) -> &'static str {
    match d {
        Direction::ToModel => "to_model",
        Direction::ToWebots => "to_webots",
    }
}

pub fn frame_from_name(s: &str) -> Option<FrameId> {
    match s {
        "none" => Some(FrameId::None),
        "world" => Some(FrameId::World),
        "body_local" => Some(FrameId::BodyLocal),
        "joint_axis" => Some(FrameId::JointAxis),
        "model_local" => Some(FrameId::ModelLocal),
        _ => None,
    }
}

pub fn frame_name(f: FrameId) -> &'static str {
    match f {
        FrameId::None => "none",
        FrameId::World => "world",
        FrameId::BodyLocal => "body_local",
        FrameId::JointAxis => "joint_axis",
        FrameId::ModelLocal => "model_local",
    }
}

// ---------------------------------------------------------------- manifest (host-owned)

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub manifest_version: u32,
    /// `"0x…"` hex string of the u64 simulation id.
    pub simulation_id: String,
    pub instance_id: u32,
    pub instance_name: String,
    pub sync_mode: String,
    pub dt_ns: u64,
    pub model_timeout_ms: u32,
    pub effort_policy: EffortPolicy,
    pub world_frame: String,
    pub entities: Vec<Entity>,
    pub slots: Vec<SlotEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entity {
    pub entity_id: u32,
    pub name: String,
    pub webots_def: String,
    /// `"joint"` or `"solid"`.
    pub kind: String,
    /// Optional overrides for joints; the adapter auto-discovers devices when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub motor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sensor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SlotEntry {
    pub slot_id: u32,
    pub entity_id: u32,
    pub signal_id: u32,
    pub name: String,
    pub value_type: String,
    pub direction: String,
    pub frame: String,
    pub unit: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<Limits>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub point: Option<[f64; 3]>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Limits {
    pub max_abs: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_rate_per_s: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffortPolicy {
    pub ttl_steps: u32,
    pub on_timeout: String,
    pub max_consecutive_misses: u32,
}

impl Default for EffortPolicy {
    fn default() -> Self {
        Self { ttl_steps: 1, on_timeout: "zero".to_string(), max_consecutive_misses: 3 }
    }
}

impl SlotEntry {
    pub fn to_spec(&self) -> Result<SlotSpec, CoreError> {
        let bad = |what: &str| CoreError::BadManifest(format!("slot {}: unknown {what}", self.slot_id));
        Ok(SlotSpec {
            slot_id: self.slot_id,
            entity_id: self.entity_id,
            signal_id: self.signal_id,
            value_type: value_type_from_name(&self.value_type).ok_or_else(|| bad("value_type"))?,
            direction: direction_from_name(&self.direction).ok_or_else(|| bad("direction"))?,
            frame_id: frame_from_name(&self.frame).ok_or_else(|| bad("frame"))?,
        })
    }
}

impl Manifest {
    /// All slots (including `enabled=false` ones: the layout never changes after Created).
    pub fn slot_specs(&self) -> Result<Vec<SlotSpec>, CoreError> {
        self.slots.iter().map(SlotEntry::to_spec).collect()
    }

    pub fn simulation_id_u64(&self) -> Result<u64, CoreError> {
        let s = self.simulation_id.trim_start_matches("0x").trim_start_matches("0X");
        u64::from_str_radix(s, 16)
            .map_err(|_| CoreError::BadManifest(format!("simulation_id {:?}", self.simulation_id)))
    }

    pub fn slot_by_name(&self, name: &str) -> Option<&SlotEntry> {
        self.slots.iter().find(|s| s.name == name)
    }
}

// ---------------------------------------------------------------- hello (client-owned)

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hello {
    pub protocol: [u16; 2],
    pub model_name: String,
    pub model_version: String,
    pub steppable: bool,
    pub sync_modes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub internal_dt_ns: Option<u64>,
    pub supported_signals: Vec<u32>,
    #[serde(default)]
    pub custom_signals: Vec<CustomSignal>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CustomSignal {
    pub signal_id: u32,
    pub name: String,
    pub value_type: String,
    pub direction: String,
    pub unit: String,
    #[serde(default)]
    pub domain: String,
}

// ---------------------------------------------------------------- report (adapter-owned)

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Report {
    pub ok: bool,
    pub adapter: String,
    /// Webots `WorldInfo.coordinateSystem` as found (`ENU`/`NUE`/`EUN`).
    #[serde(default)]
    pub world_coordinate_system: String,
    #[serde(default)]
    pub entities: Vec<EntityReport>,
    #[serde(default)]
    pub errors: Vec<String>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntityReport {
    pub entity_id: u32,
    pub resolved: bool,
    /// Webots node type actually found (`HingeJoint`, `Solid`, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}
