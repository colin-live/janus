//! Base signal registry (spec §5). IDs are stable forever within a major version.
//! Ranges: 0x0000-0x0FFF base | 0x1000 electrical | 0x2000 mechanical-ext | 0x3000 thermal
//!         0x4000 fluid | 0x5000 hydraulic | 0x6000 magnetic | 0x8000-0xFFFF per-instance custom.

use crate::types::{SignalClass, ValueType};

#[derive(Debug, Clone, Copy)]
pub struct SignalDef {
    pub id: u32,
    pub name: &'static str,
    pub class: SignalClass,
    pub value_type: ValueType,
    pub unit: &'static str,
    /// Whether a frame (and for forces an application point) must be given in the binding.
    pub needs_frame: bool,
}

use SignalClass::{Effort, Observation, State};
use ValueType::{QuatF64, Vec3F64, F64};

pub const BASE_SIGNALS: &[SignalDef] = &[
    SignalDef { id: 0x0001, name: "joint_position", class: State, value_type: F64, unit: "rad|m", needs_frame: false },
    SignalDef { id: 0x0002, name: "joint_velocity", class: State, value_type: F64, unit: "rad/s|m/s", needs_frame: false },
    SignalDef { id: 0x0003, name: "joint_torque_feedback", class: Observation, value_type: F64, unit: "N*m", needs_frame: false },
    SignalDef { id: 0x0010, name: "body_position", class: State, value_type: Vec3F64, unit: "m", needs_frame: true },
    SignalDef { id: 0x0011, name: "body_orientation", class: State, value_type: QuatF64, unit: "1", needs_frame: true },
    SignalDef { id: 0x0012, name: "body_linear_velocity", class: State, value_type: Vec3F64, unit: "m/s", needs_frame: true },
    SignalDef { id: 0x0013, name: "body_angular_velocity", class: State, value_type: Vec3F64, unit: "rad/s", needs_frame: true },
    SignalDef { id: 0x0101, name: "joint_torque", class: Effort, value_type: F64, unit: "N*m", needs_frame: false },
    SignalDef { id: 0x0102, name: "joint_force", class: Effort, value_type: F64, unit: "N", needs_frame: false },
    SignalDef { id: 0x0110, name: "body_force", class: Effort, value_type: Vec3F64, unit: "N", needs_frame: true },
    SignalDef { id: 0x0111, name: "body_torque", class: Effort, value_type: Vec3F64, unit: "N*m", needs_frame: true },
];

pub fn lookup(id: u32) -> Option<&'static SignalDef> {
    BASE_SIGNALS.iter().find(|s| s.id == id)
}

pub fn lookup_name(name: &str) -> Option<&'static SignalDef> {
    BASE_SIGNALS.iter().find(|s| s.name == name)
}

pub fn is_custom(id: u32) -> bool {
    id >= 0x8000
}
