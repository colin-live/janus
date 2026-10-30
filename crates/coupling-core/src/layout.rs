//! Deterministic segment layout from a list of slot specs (spec §4).
//! Region layout: `[validity bytes, one per slot, padded to 8][slot values, each padded to 8]`.

use crate::error::CoreError;
use crate::header::HEADER_SIZE;
use crate::registry;
use crate::types::{Direction, FrameId, SignalClass, ValueType};
use std::collections::HashSet;

pub const DEFAULT_MANIFEST_CAP: u64 = 64 * 1024;
pub const DEFAULT_HELLO_CAP: u64 = 16 * 1024;
pub const DEFAULT_REPORT_CAP: u64 = 16 * 1024;

#[derive(Debug, Clone)]
pub struct SlotSpec {
    pub slot_id: u32,
    pub entity_id: u32,
    pub signal_id: u32,
    pub value_type: ValueType,
    pub direction: Direction,
    pub frame_id: FrameId,
}

#[derive(Debug, Clone)]
pub struct SlotLayout {
    pub spec: SlotSpec,
    /// Index inside its region (ToModel / ToWebots).
    pub index: u32,
    /// Absolute byte offset (from segment start) of the validity byte.
    pub validity_offset: u64,
    /// Absolute byte offset of the value.
    pub value_offset: u64,
}

#[derive(Debug, Clone)]
pub struct Layout {
    pub manifest_off: u64,
    pub manifest_len: u64,
    pub hello_off: u64,
    pub hello_len: u64,
    pub report_off: u64,
    pub report_len: u64,
    pub state_off: u64,
    pub state_len: u64,
    pub effort_off: u64,
    pub effort_len: u64,
    pub state_slot_count: u32,
    pub effort_slot_count: u32,
    pub total_size: u64,
    pub slots: Vec<SlotLayout>,
}

fn align8(n: u64) -> u64 {
    (n + 7) & !7
}

impl Layout {
    pub fn build(specs: &[SlotSpec], manifest_cap: u64, hello_cap: u64, report_cap: u64) -> Result<Layout, CoreError> {
        if manifest_cap < 1024 {
            return Err(CoreError::CapacityTooSmall("manifest"));
        }
        if hello_cap < 1024 {
            return Err(CoreError::CapacityTooSmall("hello"));
        }
        if report_cap < 1024 {
            return Err(CoreError::CapacityTooSmall("report"));
        }
        let mut ids = HashSet::new();
        let mut sigs = HashSet::new();
        for s in specs {
            if !ids.insert(s.slot_id) {
                return Err(CoreError::DuplicateSlotId(s.slot_id));
            }
            if !sigs.insert((s.entity_id, s.signal_id, s.direction.as_raw())) {
                return Err(CoreError::DuplicateSignal {
                    entity_id: s.entity_id,
                    signal_id: s.signal_id,
                    direction: if s.direction == Direction::ToModel { "to_model" } else { "to_webots" },
                });
            }
            if !registry::is_custom(s.signal_id) {
                let def = registry::lookup(s.signal_id).ok_or(CoreError::UnknownSignal(s.signal_id))?;
                if def.value_type != s.value_type {
                    return Err(CoreError::TypeMismatch {
                        signal_id: s.signal_id,
                        expected: def.value_type.name(),
                        got: s.value_type.name(),
                    });
                }
                let is_effort = def.class == SignalClass::Effort;
                if is_effort != (s.direction == Direction::ToWebots) {
                    return Err(CoreError::DirectionMismatch { signal_id: s.signal_id });
                }
            }
        }

        let manifest_off = HEADER_SIZE as u64;
        let hello_off = align8(manifest_off + manifest_cap);
        let report_off = align8(hello_off + hello_cap);
        let state_off = align8(report_off + report_cap);

        let (state_len, state_n, mut slots) = Self::region(specs, Direction::ToModel, state_off);
        let effort_off = align8(state_off + state_len);
        let (effort_len, effort_n, effort_slots) = Self::region(specs, Direction::ToWebots, effort_off);
        slots.extend(effort_slots);

        Ok(Layout {
            manifest_off,
            manifest_len: manifest_cap,
            hello_off,
            hello_len: hello_cap,
            report_off,
            report_len: report_cap,
            state_off,
            state_len,
            effort_off,
            effort_len,
            state_slot_count: state_n,
            effort_slot_count: effort_n,
            total_size: align8(effort_off + effort_len),
            slots,
        })
    }

    fn region(specs: &[SlotSpec], dir: Direction, base: u64) -> (u64, u32, Vec<SlotLayout>) {
        let mine: Vec<&SlotSpec> = specs.iter().filter(|s| s.direction == dir).collect();
        let n = mine.len() as u64;
        let mut cursor = align8(n);
        let mut out = Vec::with_capacity(mine.len());
        for (i, s) in mine.into_iter().enumerate() {
            out.push(SlotLayout {
                spec: s.clone(),
                index: i as u32,
                validity_offset: base + i as u64,
                value_offset: base + cursor,
            });
            cursor += align8(s.value_type.size() as u64);
        }
        (cursor, n as u32, out)
    }

    pub fn slot(&self, slot_id: u32) -> Option<&SlotLayout> {
        self.slots.iter().find(|s| s.spec.slot_id == slot_id)
    }

    /// Rebuild the layout on the consumer side (client / adapter) from the manifest's slot specs and
    /// the header found in shared memory. The header's area capacities are the build parameters, so the
    /// result is identical to the host's; any disagreement with the header's offsets is an error.
    pub fn from_header(specs: &[SlotSpec], h: &crate::header::SegmentHeader) -> Result<Layout, CoreError> {
        h.validate()?;
        let l = Layout::build(specs, h.manifest_len, h.hello_len, h.report_len)?;
        if l.state_off != h.state_off
            || l.state_len != h.state_len
            || l.effort_off != h.effort_off
            || l.effort_len != h.effort_len
            || l.total_size != h.total_size
            || l.state_slot_count != h.state_slot_count
            || l.effort_slot_count != h.effort_slot_count
        {
            return Err(CoreError::BadHeader("layout does not match manifest slots"));
        }
        Ok(l)
    }
}
