use coupling_core::header::offsets as o;
use coupling_core::registry::BASE_SIGNALS;
use coupling_core::*;
use std::collections::HashSet;


fn spec(slot: u32, entity: u32, sig: u32, vt: ValueType, dir: Direction) -> SlotSpec {
    SlotSpec { slot_id: slot, entity_id: entity, signal_id: sig, value_type: vt, direction: dir, frame_id: FrameId::None }
}

#[test]
fn header_is_256_bytes() {
    assert_eq!(size_of::<SegmentHeader>(), HEADER_SIZE);
}

#[test]
fn header_offsets_match_struct() {
    let h: SegmentHeader = unsafe { std::mem::zeroed() };
    let base = &h as *const _ as usize;
    let off = |p: usize| p - base;
    assert_eq!(off(&h.magic as *const _ as usize), o::MAGIC);
    assert_eq!(off(&h.protocol_major as *const _ as usize), o::PROTOCOL_MAJOR);
    assert_eq!(off(&h.total_size as *const _ as usize), o::TOTAL_SIZE);
    assert_eq!(off(&h.simulation_id as *const _ as usize), o::SIMULATION_ID);
    assert_eq!(off(&h.instance_id as *const _ as usize), o::INSTANCE_ID);
    assert_eq!(off(&h.hs_state as *const _ as usize), o::HS_STATE);
    assert_eq!(off(&h.sync_mode as *const _ as usize), o::SYNC_MODE);
    assert_eq!(off(&h.epoch as *const _ as usize), o::EPOCH);
    assert_eq!(off(&h.model_epoch_ack as *const _ as usize), o::MODEL_EPOCH_ACK);
    assert_eq!(off(&h.step_id as *const _ as usize), o::STEP_ID);
    assert_eq!(off(&h.effort_step_id as *const _ as usize), o::EFFORT_STEP_ID);
    assert_eq!(off(&h.sim_time_ns as *const _ as usize), o::SIM_TIME_NS);
    assert_eq!(off(&h.dt_ns as *const _ as usize), o::DT_NS);
    assert_eq!(off(&h.adapter_heartbeat as *const _ as usize), o::ADAPTER_HEARTBEAT);
    assert_eq!(off(&h.model_heartbeat as *const _ as usize), o::MODEL_HEARTBEAT);
    assert_eq!(off(&h.manifest_off as *const _ as usize), o::MANIFEST_OFF);
    assert_eq!(off(&h.state_off as *const _ as usize), o::STATE_OFF);
    assert_eq!(off(&h.effort_off as *const _ as usize), o::EFFORT_OFF);
    assert_eq!(off(&h.state_slot_count as *const _ as usize), o::STATE_SLOT_COUNT);
    assert_eq!(off(&h.model_timeout_ms as *const _ as usize), o::MODEL_TIMEOUT_MS);
    assert_eq!(off(&h.report_off as *const _ as usize), o::REPORT_OFF);
}

#[test]
fn registry_ids_unique_and_consistent() {
    let mut seen = HashSet::new();
    for s in BASE_SIGNALS {
        assert!(seen.insert(s.id), "duplicate id 0x{:04X}", s.id);
        assert!(s.id < 0x8000);
    }
}

#[test]
fn layout_motor_joint() {
    let specs = vec![
        spec(0, 1, 0x0001, ValueType::F64, Direction::ToModel),
        spec(1, 1, 0x0002, ValueType::F64, Direction::ToModel),
        spec(2, 1, 0x0101, ValueType::F64, Direction::ToWebots),
    ];
    let l = Layout::build(&specs, 64 * 1024, 16 * 1024, 16 * 1024).unwrap();
    assert_eq!(l.state_slot_count, 2);
    assert_eq!(l.effort_slot_count, 1);
    for s in &l.slots {
        assert_eq!(s.value_offset % 8, 0);
    }
    assert!(l.hello_off + l.hello_len <= l.report_off);
    assert!(l.report_off + l.report_len <= l.state_off);
    assert!(l.state_off + l.state_len <= l.effort_off);
    assert!(l.effort_off + l.effort_len <= l.total_size);
    assert_eq!(l.total_size % 8, 0);
    // validity bytes precede values, no overlap
    let s0 = l.slot(0).unwrap();
    assert!(s0.validity_offset < s0.value_offset);
}

#[test]
fn layout_rejects_bad_specs() {
    let dup = vec![
        spec(0, 1, 0x0001, ValueType::F64, Direction::ToModel),
        spec(0, 1, 0x0002, ValueType::F64, Direction::ToModel),
    ];
    assert_eq!(Layout::build(&dup, 4096, 4096, 4096).unwrap_err(), CoreError::DuplicateSlotId(0));

    let wrong_type = vec![spec(0, 1, 0x0001, ValueType::F32, Direction::ToModel)];
    assert!(matches!(Layout::build(&wrong_type, 4096, 4096, 4096), Err(CoreError::TypeMismatch { .. })));

    let wrong_dir = vec![spec(0, 1, 0x0101, ValueType::F64, Direction::ToModel)];
    assert!(matches!(Layout::build(&wrong_dir, 4096, 4096, 4096), Err(CoreError::DirectionMismatch { .. })));

    let unknown = vec![spec(0, 1, 0x0999, ValueType::F64, Direction::ToModel)];
    assert!(matches!(Layout::build(&unknown, 4096, 4096, 4096), Err(CoreError::UnknownSignal(_))));
}

#[test]
fn custom_signals_allowed() {
    let specs = vec![spec(0, 1, 0x8001, ValueType::U32, Direction::ToWebots)];
    assert!(Layout::build(&specs, 4096, 4096, 4096).is_ok());
}

#[test]
fn enums_roundtrip() {
    for v in 1u8..=9 {
        assert_eq!(ValueType::from_raw(v).unwrap().as_raw(), v);
    }
    assert!(ValueType::from_raw(0).is_none());
    assert_eq!(Validity::from_raw(0), Some(Validity::Missing));
}
