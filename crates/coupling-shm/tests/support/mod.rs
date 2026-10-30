//! Shared helpers for the B2 integration tests.

#![allow(dead_code)]

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use coupling_core::{Direction, FrameId, Layout, SlotSpec, ValueType};
use coupling_shm::{names, SegmentNames};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Kernel objects are global, so every test run / test case needs its own names.
pub fn unique_names(_tag: &str) -> SegmentNames {
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64;
    let pid = std::process::id() as u64;
    let sim_id = nanos.rotate_left(21).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (pid << 32) ^ seq;
    names(sim_id, seq as u32)
}

fn spec(slot_id: u32, signal_id: u32, vt: ValueType, dir: Direction, frame: FrameId) -> SlotSpec {
    SlotSpec {
        slot_id,
        entity_id: 1,
        signal_id,
        value_type: vt,
        direction: dir,
        frame_id: frame,
    }
}

/// One joint: position + velocity in, torque out (the usual motor binding).
pub fn motor_layout() -> Layout {
    let specs = [
        spec(0, 0x0001, ValueType::F64, Direction::ToModel, FrameId::None),
        spec(1, 0x0002, ValueType::F64, Direction::ToModel, FrameId::None),
        spec(
            2,
            0x0101,
            ValueType::F64,
            Direction::ToWebots,
            FrameId::JointAxis,
        ),
    ];
    Layout::build(&specs, 64 * 1024, 16 * 1024, 16 * 1024).expect("layout")
}
