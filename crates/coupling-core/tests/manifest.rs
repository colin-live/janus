use coupling_core::manifest::*;
use coupling_core::*;

fn sample_manifest() -> Manifest {
    let slot = |id, sig, name: &str, dir: &str, frame: &str, unit: &str| SlotEntry {
        slot_id: id,
        entity_id: 1,
        signal_id: sig,
        name: name.into(),
        value_type: "f64".into(),
        direction: dir.into(),
        frame: frame.into(),
        unit: unit.into(),
        required: true,
        enabled: true,
        limits: None,
        point: None,
    };
    let mut torque = slot(2, 0x0101, "joint_torque", "to_webots", "joint_axis", "N*m");
    torque.limits = Some(Limits { max_abs: 5.0, max_rate_per_s: Some(2000.0) });
    Manifest {
        manifest_version: MANIFEST_VERSION,
        simulation_id: "0x1234abcd5678ef90".into(),
        instance_id: 1,
        instance_name: "shoulder_motor".into(),
        sync_mode: "lockstep".into(),
        dt_ns: 1_000_000,
        model_timeout_ms: 50,
        effort_policy: EffortPolicy::default(),
        world_frame: "ENU".into(),
        entities: vec![Entity {
            entity_id: 1,
            name: "joint".into(),
            webots_def: "SHOULDER_JOINT".into(),
            kind: "joint".into(),
            motor: None,
            sensor: None,
        }],
        slots: vec![
            slot(0, 0x0001, "joint_position", "to_model", "none", "rad"),
            slot(1, 0x0002, "joint_velocity", "to_model", "none", "rad/s"),
            torque,
        ],
    }
}

#[test]
fn manifest_json_roundtrip() {
    let m = sample_manifest();
    let json = serde_json::to_string(&m).unwrap();
    let back: Manifest = serde_json::from_str(&json).unwrap();
    assert_eq!(m, back);
    assert_eq!(back.simulation_id_u64().unwrap(), 0x1234abcd5678ef90);
}

#[test]
fn manifest_ignores_unknown_fields() {
    let mut v = serde_json::to_value(sample_manifest()).unwrap();
    v["future_field"] = serde_json::json!({"x": 1});
    v["slots"][0]["future_slot_field"] = serde_json::json!(true);
    let m: Manifest = serde_json::from_value(v).unwrap();
    assert_eq!(m.slots.len(), 3);
}

#[test]
fn slot_defaults_apply() {
    let v = serde_json::json!({
        "slot_id": 0, "entity_id": 1, "signal_id": 1, "name": "joint_position",
        "value_type": "f64", "direction": "to_model", "frame": "none", "unit": "rad"
    });
    let s: SlotEntry = serde_json::from_value(v).unwrap();
    assert!(s.enabled);
    assert!(!s.required);
}

#[test]
fn specs_from_manifest_build_layout_and_match_header() {
    let m = sample_manifest();
    let specs = m.slot_specs().unwrap();
    let l = Layout::build(&specs, 64 * 1024, 16 * 1024, 16 * 1024).unwrap();

    // Simulate what coupling-shm writes at Created.
    let mut h: SegmentHeader = unsafe { std::mem::zeroed() };
    h.magic = MAGIC;
    h.protocol_major = PROTOCOL_MAJOR;
    h.protocol_minor = PROTOCOL_MINOR;
    h.header_size = HEADER_SIZE as u32;
    h.total_size = l.total_size;
    h.manifest_off = l.manifest_off;
    h.manifest_len = l.manifest_len;
    h.hello_off = l.hello_off;
    h.hello_len = l.hello_len;
    h.report_off = l.report_off;
    h.report_len = l.report_len;
    h.state_off = l.state_off;
    h.state_len = l.state_len;
    h.effort_off = l.effort_off;
    h.effort_len = l.effort_len;
    h.state_slot_count = l.state_slot_count;
    h.effort_slot_count = l.effort_slot_count;

    let l2 = Layout::from_header(&specs, &h).unwrap();
    assert_eq!(l2.total_size, l.total_size);
    for (a, b) in l.slots.iter().zip(l2.slots.iter()) {
        assert_eq!(a.value_offset, b.value_offset);
        assert_eq!(a.validity_offset, b.validity_offset);
    }

    // A manifest with an extra slot must be rejected against the old header.
    let mut more = m.clone();
    let mut extra = more.slots[0].clone();
    extra.slot_id = 9;
    extra.signal_id = 0x0010;
    extra.value_type = "vec3f64".into();
    extra.name = "body_position".into();
    more.slots.push(extra);
    let specs2 = more.slot_specs().unwrap();
    assert!(Layout::from_header(&specs2, &h).is_err());
}

#[test]
fn bad_names_are_reported() {
    let mut m = sample_manifest();
    m.slots[0].direction = "sideways".into();
    assert!(matches!(m.slot_specs(), Err(CoreError::BadManifest(_))));
}

#[test]
fn hello_and_report_roundtrip() {
    let hello = Hello {
        protocol: [1, 0],
        model_name: "dcmotor".into(),
        model_version: "0.1.0".into(),
        steppable: true,
        sync_modes: vec!["lockstep".into()],
        internal_dt_ns: Some(100_000),
        supported_signals: vec![1, 2, 257],
        custom_signals: vec![CustomSignal {
            signal_id: 0x8001,
            name: "dc_bus_voltage".into(),
            value_type: "f64".into(),
            direction: "to_webots".into(),
            unit: "V".into(),
            domain: "electrical".into(),
        }],
    };
    let back: Hello = serde_json::from_str(&serde_json::to_string(&hello).unwrap()).unwrap();
    assert_eq!(hello, back);

    let report: Report = serde_json::from_str(r#"{"ok":false,"adapter":"webots-py 0.1","errors":["DEF X not found"]}"#).unwrap();
    assert!(!report.ok && report.errors.len() == 1 && report.entities.is_empty());
}

#[test]
fn name_helpers_roundtrip() {
    for d in [Direction::ToModel, Direction::ToWebots] {
        assert_eq!(manifest::direction_from_name(manifest::direction_name(d)), Some(d));
    }
    for f in [FrameId::None, FrameId::World, FrameId::BodyLocal, FrameId::JointAxis, FrameId::ModelLocal] {
        assert_eq!(manifest::frame_from_name(manifest::frame_name(f)), Some(f));
    }
    for v in 1u8..=9 {
        let t = ValueType::from_raw(v).unwrap();
        assert_eq!(manifest::value_type_from_name(t.name()), Some(t));
    }
}
