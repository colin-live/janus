//! Segment tests: naming, mapping, header init, slot accessors, JSON areas.

mod support;

use std::sync::atomic::Ordering;
use std::time::Duration;

use coupling_core::{HandshakeState, SyncMode, Validity};
use coupling_shm::{JsonArea, Segment, ShmError};

use support::{motor_layout, unique_names};

#[test]
fn create_and_open_share_bytes_and_header() {
    let n = unique_names("seg-share");
    let layout = motor_layout();
    let host = Segment::create(&n.segment, layout.total_size as usize).unwrap();
    host.init_from_layout(&layout, 0xDEAD_BEEF_0000_0001, 7, 50)
        .unwrap();

    let model = Segment::open(&n.segment).unwrap();
    assert_eq!(model.len(), layout.total_size as usize);
    assert_eq!(model.header().simulation_id, 0xDEAD_BEEF_0000_0001);
    assert_eq!(model.header().instance_id, 7);
    assert_eq!(model.header().model_timeout_ms, 50);
    assert_eq!(model.header().state_slot_count, 2);
    assert_eq!(model.header().effort_slot_count, 1);
    assert_eq!(model.header().total_size, layout.total_size);
    assert_eq!(
        model.header().hs_state.load(Ordering::Acquire),
        HandshakeState::Created.as_raw()
    );
    assert_eq!(
        model.header().sync_mode.load(Ordering::Acquire),
        SyncMode::Lockstep.as_raw()
    );
    model
        .header()
        .validate()
        .expect("header must be valid after init");

    // Adapter writes a state slot through the host handle, model reads it.
    let pos = layout.slot(0).unwrap();
    host.write_f64(pos.value_offset, 1.25);
    host.write_validity(pos.validity_offset, Validity::Valid);
    assert_eq!(model.read_f64(pos.value_offset), 1.25);
    assert_eq!(model.read_validity(pos.validity_offset), Validity::Valid);

    // Model writes an effort slot through its own handle, host reads it.
    let torque = layout.slot(2).unwrap();
    model.write_f64(torque.value_offset, -0.5);
    model.write_validity(torque.validity_offset, Validity::Valid);
    assert_eq!(host.read_f64(torque.value_offset), -0.5);
    assert_eq!(host.read_validity(torque.validity_offset), Validity::Valid);

    // Raw byte view is shared as well.
    let mut raw = [0u8; 8];
    model.read_bytes(pos.value_offset, &mut raw);
    assert_eq!(f64::from_le_bytes(raw), 1.25);
}

#[test]
fn duplicate_create_is_rejected() {
    let n = unique_names("seg-dup");
    let first = Segment::create(&n.segment, 4096).unwrap();
    assert_eq!(
        Segment::create(&n.segment, 4096).unwrap_err(),
        ShmError::AlreadyExists
    );
    drop(first);
}

#[test]
fn open_missing_segment_is_not_found() {
    let n = unique_names("seg-missing");
    assert_eq!(Segment::open(&n.segment).unwrap_err(), ShmError::NotFound);
}

#[test]
fn dropping_the_creator_destroys_the_name() {
    let n = unique_names("seg-drop");
    let layout = motor_layout();
    let seg = Segment::create(&n.segment, layout.total_size as usize).unwrap();
    seg.init_from_layout(&layout, 1, 1, 50).unwrap();
    let peer = Segment::open(&n.segment).unwrap();
    drop(seg);
    // POSIX unlinks the name immediately, Windows keeps the kernel object alive as
    // long as a handle (here: the opener) is open.
    #[cfg(unix)]
    assert!(Segment::open(&n.segment).is_err());
    // The peer still holds a valid mapping and can read the header.
    assert_eq!(peer.len(), layout.total_size as usize);
    assert_eq!(peer.header().simulation_id, 1);
    drop(peer);
    assert_eq!(Segment::open(&n.segment).unwrap_err(), ShmError::NotFound);
}

#[test]
fn init_zeroes_the_segment_and_publishes_static_header() {
    let n = unique_names("seg-init");
    let layout = motor_layout();
    let seg = Segment::create(&n.segment, layout.total_size as usize).unwrap();
    // Garbage from "a previous run" must not survive init.
    seg.write_bytes(0, &[0xAB; 128]);
    seg.init_from_layout(&layout, 0x11, 2, 25).unwrap();

    let h = seg.header();
    h.validate().unwrap();
    assert_eq!(h.magic, coupling_core::MAGIC);
    assert_eq!(h.protocol_major, coupling_core::PROTOCOL_MAJOR);
    assert_eq!(h.header_size, coupling_core::HEADER_SIZE as u32);
    assert_eq!(h.manifest_off, layout.manifest_off);
    assert_eq!(h.manifest_len, layout.manifest_len);
    assert_eq!(h.hello_off, layout.hello_off);
    assert_eq!(h.report_off, layout.report_off);
    assert_eq!(h.state_off, layout.state_off);
    assert_eq!(h.state_len, layout.state_len);
    assert_eq!(h.effort_off, layout.effort_off);
    assert_eq!(h.effort_len, layout.effort_len);

    for slot in &layout.slots {
        assert_eq!(seg.read_validity(slot.validity_offset), Validity::Missing);
        assert_eq!(seg.read_f64(slot.value_offset), 0.0);
    }
    // The un-initialised tail of the segment is zeroed too.
    let mut tail = [0xFFu8; 32];
    seg.read_bytes(seg.len() as u64 - 32, &mut tail);
    assert_eq!(tail, [0u8; 32]);

    // Re-init is allowed (creator only) and wipes previous values.
    seg.write_f64(layout.slot(0).unwrap().value_offset, 9.0);
    seg.init_from_layout(&layout, 0x11, 2, 25).unwrap();
    assert_eq!(seg.read_f64(layout.slot(0).unwrap().value_offset), 0.0);
}

#[test]
fn json_areas_roundtrip_and_share() {
    let n = unique_names("seg-json");
    let layout = motor_layout();
    let host = Segment::create(&n.segment, layout.total_size as usize).unwrap();
    host.init_from_layout(&layout, 1, 1, 50).unwrap();
    let model = Segment::open(&n.segment).unwrap();

    assert!(host.read_json(JsonArea::Hello).unwrap().is_empty());

    let hello = br#"{"protocol":[1,0],"model_name":"dcmotor","steppable":true}"#;
    host.write_json(JsonArea::Hello, hello).unwrap();
    assert_eq!(model.read_json(JsonArea::Hello).unwrap(), hello.to_vec());

    let manifest = br#"{"manifest_version":1,"instance_id":1}"#;
    model.write_json(JsonArea::Manifest, manifest).unwrap();
    assert_eq!(
        host.read_json(JsonArea::Manifest).unwrap(),
        manifest.to_vec()
    );

    let report = br#"{"ok":true}"#;
    host.write_json(JsonArea::Report, report).unwrap();
    assert_eq!(model.read_json(JsonArea::Report).unwrap(), report.to_vec());

    // Empty write clears the area (n = 0).
    host.write_json(JsonArea::Hello, b"").unwrap();
    assert!(model.read_json(JsonArea::Hello).unwrap().is_empty());

    // Areas are independent.
    assert_eq!(
        model.read_json(JsonArea::Manifest).unwrap(),
        manifest.to_vec()
    );
}

#[test]
fn json_capacity_is_enforced() {
    let n = unique_names("seg-json-cap");
    let layout = motor_layout();
    let seg = Segment::create(&n.segment, layout.total_size as usize).unwrap();
    seg.init_from_layout(&layout, 1, 1, 50).unwrap();
    let cap = seg.header().hello_len as usize;

    let too_big = vec![b'x'; cap - 3]; // needs cap + 1 bytes with the prefix
    assert!(
        matches!(
            seg.write_json(JsonArea::Hello, &too_big),
            Err(ShmError::TooSmall { .. })
        ),
        "a payload larger than capacity - 4 must be rejected"
    );
    // The rejected write must not have changed the area.
    assert!(seg.read_json(JsonArea::Hello).unwrap().is_empty());

    let exact = vec![b'y'; cap - 4];
    seg.write_json(JsonArea::Hello, &exact).unwrap();
    assert_eq!(seg.read_json(JsonArea::Hello).unwrap(), exact);

    // Only one byte more must fail.
    let one_more = vec![b'y'; cap - 3];
    assert!(matches!(
        seg.write_json(JsonArea::Hello, &one_more),
        Err(ShmError::TooSmall { .. })
    ));
}

#[test]
fn unknown_validity_byte_reads_as_error() {
    let n = unique_names("seg-validity");
    let layout = motor_layout();
    let seg = Segment::create(&n.segment, layout.total_size as usize).unwrap();
    seg.init_from_layout(&layout, 1, 1, 50).unwrap();
    let off = layout.slot(0).unwrap().validity_offset;

    for (raw, expected) in [
        (0u8, Validity::Missing),
        (1, Validity::Valid),
        (2, Validity::Invalid),
        (3, Validity::Stale),
        (4, Validity::OutOfRange),
        (5, Validity::Error),
        (42, Validity::Error),
        (255, Validity::Error),
    ] {
        seg.write_bytes(off, &[raw]);
        assert_eq!(seg.read_validity(off), expected, "raw validity byte {raw}");
    }

    seg.write_validity(off, Validity::Stale);
    let mut raw = [0u8; 1];
    seg.read_bytes(off, &mut raw);
    assert_eq!(raw[0], Validity::Stale.as_raw());
}

#[test]
fn out_of_range_access_is_checked_not_ub() {
    let n = unique_names("seg-bounds");
    let layout = motor_layout();
    let seg = Segment::create(&n.segment, layout.total_size as usize).unwrap();
    seg.init_from_layout(&layout, 1, 1, 50).unwrap();
    let len = seg.len() as u64;

    // Frozen `read_bytes` cannot fail: it zero fills.
    let mut buf = [0x11u8; 8];
    seg.read_bytes(len - 4, &mut buf);
    assert_eq!(buf, [0u8; 8]);
    // ...while the fallible variant reports the problem.
    let mut buf = [0x11u8; 8];
    assert!(matches!(
        seg.try_read_bytes(len - 4, &mut buf),
        Err(ShmError::TooSmall { .. })
    ));
    assert_eq!(buf, [0x11u8; 8]);

    // Frozen `write_bytes` is a no-op instead of panicking.
    seg.write_bytes(len - 4, &[0xAA; 8]);
    assert!(matches!(
        seg.try_write_bytes(len - 4, &[0xAA; 8]),
        Err(ShmError::TooSmall { .. })
    ));

    // Whole-vector overrun as well.
    let mut big = [0u8; 32];
    assert!(seg.try_read_bytes(len - 16, &mut big).is_err());

    // Validity accessors collapse to Error / no-op.
    assert_eq!(seg.read_validity(u64::MAX), Validity::Error);
    seg.write_validity(u64::MAX, Validity::Valid);
    assert_eq!(seg.read_validity(len), Validity::Error);
}

#[test]
#[should_panic(expected = "out of range")]
fn read_f64_out_of_range_panics() {
    let n = unique_names("seg-f64-oob");
    let seg = Segment::create(&n.segment, 4096).unwrap();
    let _ = seg.read_f64(4094);
}

#[test]
fn layout_larger_than_segment_is_rejected() {
    let n = unique_names("seg-small");
    let seg = Segment::create(&n.segment, 512).unwrap();
    let layout = motor_layout();
    assert!(matches!(
        seg.init_from_layout(&layout, 1, 1, 50),
        Err(ShmError::TooSmall { .. })
    ));
    assert!(matches!(
        Segment::create(&unique_names("seg-zero").segment, 0).unwrap_err(),
        ShmError::TooSmall { .. }
    ));
}

#[test]
fn header_is_eight_byte_aligned() {
    let n = unique_names("seg-align");
    let layout = motor_layout();
    let seg = Segment::create(&n.segment, layout.total_size as usize).unwrap();
    seg.init_from_layout(&layout, 1, 1, 50).unwrap();
    assert_eq!((seg.header() as *const _ as usize) % 8, 0);
}

#[test]
fn wait_result_is_copy_and_comparable() {
    use coupling_shm::WaitResult;
    let a = WaitResult::Signaled;
    let b = a;
    assert_eq!(a, b);
    assert_ne!(a, WaitResult::TimedOut);
    let _ = Duration::from_millis(0);
}
