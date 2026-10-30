use coupling_config::units::Dim;
use coupling_config::{check_world_defs, load_project, ConfigError, Launch, Manifest};
use coupling_core::types::{Direction, FrameId, SyncMode, ValueType};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

const VALID: &str = r#"
coupling_version: 1
simulation:
  basic_time_step_ms: 1.0
  sync_mode: lockstep
  model_timeout_ms: 50
models:
  m:
    attach: true
instances:
  inst: { model: m, id: 1 }
bindings:
  - instance: inst
    entities:
      j: { webots_def: J, kind: joint }
    slots:
      - { signal: joint_position, entity: j, required: true }
      - { signal: joint_velocity, entity: j, required: true }
      - { signal: joint_torque, entity: j, required: true, limits: { max_abs: 5.0 } }
"#;

fn write_project(yaml: &str) -> PathBuf {
    let n = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("janus-b3-test-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("project.yaml");
    std::fs::write(&path, yaml).unwrap();
    path
}

fn load(yaml: &str) -> Result<coupling_config::ResolvedProject, ConfigError> {
    load_project(&write_project(yaml))
}

fn fail(yaml: &str) -> ConfigError {
    load(yaml).expect_err("configuration should have been rejected")
}

#[test]
fn valid_project_resolves_ids_and_settings() {
    let project = load(VALID).unwrap();
    assert_eq!(project.sim.dt_ns, 1_000_000);
    assert_eq!(project.sim.sync_mode, SyncMode::Lockstep);
    assert_eq!(project.sim.model_timeout_ms, 50);
    assert_eq!(project.sim.world_frame, "ENU");
    assert_eq!(project.instances.len(), 1);
    let inst = &project.instances[0];
    assert_eq!(inst.name, "inst");
    assert_eq!(inst.instance_id, 1);
    assert_eq!(inst.entities.len(), 1);
    assert_eq!(inst.entities[0].entity_id, 1);
    assert_eq!(inst.entities[0].webots_def, "J");
    assert_eq!(inst.entities[0].kind, "joint");
    assert_eq!(inst.slots.len(), 3);
    assert_eq!(
        inst.slots.iter().map(|s| s.slot_id).collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    assert_eq!(inst.slots[2].signal_id, 0x0101);
    assert_eq!(inst.slots[2].direction, Direction::ToWebots);
    assert_eq!(inst.slots[2].frame_id, FrameId::JointAxis);
    match &inst.launch {
        Launch::Attach => {}
        other => panic!("expected attach, got {other:?}"),
    }
}

#[test]
fn example_arm2dof_project_parses() {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/arm2dof/project.yaml");
    let project = load_project(&path).expect("examples/arm2dof/project.yaml must parse");
    let inst = project.instance("shoulder_motor").unwrap();
    assert_eq!(inst.instance_id, 1);
    assert_eq!(inst.entities[0].webots_def, "SHOULDER_JOINT");
    assert_eq!(inst.slots.len(), 3);
    // Spawn paths are relative to the project file.
    match &inst.launch {
        Launch::Spawn { exe, args, cwd } => {
            assert!(exe.ends_with("bin/dcmotor-model") || exe.ends_with("bin\\dcmotor-model"));
            assert_eq!(
                args,
                &vec![
                    "--params".to_string(),
                    "models/shoulder_motor.yaml".to_string()
                ]
            );
            assert_eq!(cwd, &path.parent().unwrap());
        }
        other => panic!("expected spawn, got {other:?}"),
    }
}

#[test]
fn manifest_roundtrips_and_matches_protocol_shape() {
    let project = load(VALID).unwrap();
    let inst = &project.instances[0];
    let manifest = inst.to_manifest(&project.sim, 1);
    assert_eq!(manifest.simulation_id, "0x0000000000000001");
    assert_eq!(manifest.slots[0].name, "joint_position");
    assert_eq!(manifest.slots[0].value_type, "f64");
    assert_eq!(manifest.slots[0].direction, "to_model");
    assert_eq!(manifest.slots[0].frame, "none");
    assert_eq!(manifest.slots[0].unit, "rad");
    assert_eq!(manifest.slots[2].frame, "joint_axis");
    assert_eq!(manifest.slots[2].unit, "N*m");
    assert_eq!(manifest.slots[2].limits.as_ref().unwrap().max_abs, 5.0);

    let json = serde_json::to_string(&manifest).unwrap();
    let mut value: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(value["manifest_version"], 1);
    assert_eq!(value["sync_mode"], "lockstep");
    assert_eq!(value["world_frame"], "ENU");
    assert_eq!(value["entities"][0]["webots_def"], "J");

    // Unknown JSON fields must be ignored (protocol §11).
    value["future_field"] = serde_json::json!({"x": 1});
    let decoded: Manifest = serde_json::from_value(value).unwrap();
    assert_eq!(decoded, manifest);
}

#[test]
fn entity_motor_and_sensor_overrides_survive_manifest_roundtrip() {
    let yaml = VALID.replace(
        "j: { webots_def: J, kind: joint }",
        "j: { webots_def: J, kind: joint, motor: SHOULDER_MOTOR, sensor: SHOULDER_SENSOR }",
    );
    let project = load(&yaml).unwrap();
    let entity = &project.instances[0].entities[0];
    assert_eq!(entity.motor.as_deref(), Some("SHOULDER_MOTOR"));
    assert_eq!(entity.sensor.as_deref(), Some("SHOULDER_SENSOR"));

    let manifest = project.instances[0].to_manifest(&project.sim, 1);
    let json = serde_json::to_string(&manifest).unwrap();
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(value["entities"][0]["motor"], "SHOULDER_MOTOR");
    assert_eq!(value["entities"][0]["sensor"], "SHOULDER_SENSOR");

    let decoded: Manifest = serde_json::from_value(value).unwrap();
    assert_eq!(decoded, manifest);
    assert_eq!(decoded.entities[0].motor.as_deref(), Some("SHOULDER_MOTOR"));
    assert_eq!(
        decoded.entities[0].sensor.as_deref(),
        Some("SHOULDER_SENSOR")
    );
}

#[test]
fn entity_motor_and_sensor_are_optional_and_omitted_from_manifest_json() {
    let project = load(VALID).unwrap();
    let entity = &project.instances[0].entities[0];
    assert!(entity.motor.is_none());
    assert!(entity.sensor.is_none());

    let manifest = project.instances[0].to_manifest(&project.sim, 1);
    let json = serde_json::to_string(&manifest).unwrap();
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(value["entities"][0].get("motor").is_none());
    assert!(value["entities"][0].get("sensor").is_none());

    let decoded: Manifest = serde_json::from_value(value).unwrap();
    assert_eq!(decoded, manifest);
    assert!(decoded.entities[0].motor.is_none());
    assert!(decoded.entities[0].sensor.is_none());
}

#[test]
fn explicit_linear_unit_conflicts_with_joint_torque() {
    let yaml = VALID.replace(
        "signal: joint_position, entity: j, required: true",
        "signal: joint_position, entity: j, required: true, unit: m",
    );
    let err = fail(&yaml);
    assert_eq!(err.path(), Some("bindings[0].slots[2].signal"));
    assert!(err.to_string().contains("joint_torque"), "{err}");
    assert!(err.to_string().contains("linear"), "{err}");
}

#[test]
fn explicit_rotational_unit_conflicts_with_joint_force() {
    let yaml = VALID
        .replace(
            "signal: joint_position, entity: j, required: true",
            "signal: joint_position, entity: j, required: true, unit: rad",
        )
        .replace(
            "signal: joint_torque, entity: j, required: true, limits: { max_abs: 5.0 }",
            "signal: joint_force, entity: j, required: true, limits: { max_abs: 5.0 }",
        );
    let err = fail(&yaml);
    assert_eq!(err.path(), Some("bindings[0].slots[2].signal"));
    assert!(err.to_string().contains("joint_force"), "{err}");
    assert!(err.to_string().contains("rotational"), "{err}");
}

#[test]
fn linear_joint_signals_are_consistent_with_joint_force() {
    let yaml = VALID
        .replace(
            "signal: joint_position, entity: j, required: true",
            "signal: joint_position, entity: j, required: true, unit: m",
        )
        .replace(
            "signal: joint_velocity, entity: j, required: true",
            "signal: joint_velocity, entity: j, required: true, unit: m/s",
        )
        .replace(
            "signal: joint_torque, entity: j, required: true, limits: { max_abs: 5.0 }",
            "signal: joint_force, entity: j, required: true, limits: { max_abs: 5.0 }",
        );
    let project = load(&yaml).unwrap();
    let manifest = project.instances[0].to_manifest(&project.sim, 1);
    assert_eq!(manifest.slots[0].unit, "m");
    assert_eq!(manifest.slots[1].unit, "m/s");
    assert_eq!(manifest.slots[2].unit, "N");
}

#[test]
fn integer_yaml_numbers_are_accepted_as_floats() {
    let yaml = VALID
        .replace("basic_time_step_ms: 1.0", "basic_time_step_ms: 1")
        .replace("max_abs: 5.0", "max_abs: 5");
    let project = load(&yaml).unwrap();
    assert_eq!(project.sim.basic_time_step_ms, 1.0);
    assert_eq!(project.sim.dt_ns, 1_000_000);
    assert_eq!(
        project.instances[0].slot_meta[2]
            .limits
            .as_ref()
            .unwrap()
            .max_abs,
        5.0
    );
}

#[test]
fn duplicate_instance_id_is_rejected() {
    let yaml = VALID.replace(
        "  inst: { model: m, id: 1 }",
        "  inst: { model: m, id: 1 }\n  other: { model: m, id: 1 }",
    );
    let err = fail(&yaml);
    assert_eq!(err.path(), Some("instances.other.id"));
    assert!(err.to_string().contains("duplicate instance id 1"), "{err}");
}

#[test]
fn zero_instance_id_is_rejected() {
    let err = fail(&VALID.replace("id: 1", "id: 0"));
    assert_eq!(err.path(), Some("instances.inst.id"));
    assert!(err.to_string().contains("must be > 0"), "{err}");
}

#[test]
fn unknown_model_is_rejected() {
    let err = fail(&VALID.replace("model: m", "model: missing"));
    assert_eq!(err.path(), Some("instances.inst.model"));
    assert!(err.to_string().contains("unknown model"), "{err}");
}

#[test]
fn binding_must_reference_existing_instance() {
    let err = fail(&VALID.replace("instance: inst", "instance: ghost"));
    assert_eq!(err.path(), Some("bindings[0].instance"));
    assert!(err.to_string().contains("unknown instance"), "{err}");
}

#[test]
fn slot_must_reference_declared_entity() {
    let err = fail(&VALID.replace("entity: j", "entity: ghost"));
    assert_eq!(err.path(), Some("bindings[0].slots[0].entity"));
    assert!(err.to_string().contains("unknown entity"), "{err}");
}

#[test]
fn slot_must_reference_known_signal() {
    let err = fail(&VALID.replace("joint_position", "made_up_signal"));
    assert_eq!(err.path(), Some("bindings[0].slots[0].signal"));
    assert!(err.to_string().contains("unknown signal"), "{err}");
}

#[test]
fn effort_slot_requires_limits() {
    let yaml = VALID.replace(", limits: { max_abs: 5.0 }", "");
    let err = fail(&yaml);
    assert_eq!(err.path(), Some("bindings[0].slots[2].limits"));
    assert_eq!(
        err.to_string(),
        "bindings[0].slots[2].limits: required for effort signal"
    );
}

#[test]
fn limits_require_max_abs() {
    let yaml = VALID.replace(
        "limits: { max_abs: 5.0 }",
        "limits: { max_rate_per_s: 10.0 }",
    );
    let err = fail(&yaml);
    assert_eq!(err.path(), Some("bindings[0].slots[2].limits.max_abs"));
    assert!(
        err.to_string().contains("required for effort signal"),
        "{err}"
    );
}

#[test]
fn negative_limit_is_rejected() {
    let err = fail(&VALID.replace("max_abs: 5.0", "max_abs: -1.0"));
    assert_eq!(err.path(), Some("bindings[0].slots[2].limits.max_abs"));
    assert!(err.to_string().contains(">= 0"), "{err}");
}

#[test]
fn body_force_requires_frame() {
    let yaml = VALID.replace(
        "      - { signal: joint_torque, entity: j, required: true, limits: { max_abs: 5.0 } }",
        "      - { signal: body_force, entity: j, required: true, limits: { max_abs: 1.0 } }",
    );
    let err = fail(&yaml);
    assert_eq!(err.path(), Some("bindings[0].slots[2].frame"));
    assert!(
        err.to_string()
            .contains("required for body_force/body_torque"),
        "{err}"
    );
}

#[test]
fn body_force_with_world_frame_is_accepted() {
    let yaml = VALID.replace(
        "      - { signal: joint_torque, entity: j, required: true, limits: { max_abs: 5.0 } }",
        "      - { signal: body_force, entity: j, required: true, frame: world, point: [0.1, 0.2, 0.3], limits: { max_abs: 1.0 } }",
    );
    let project = load(&yaml).unwrap();
    let slot = &project.instances[0].slots[2];
    assert_eq!(slot.value_type, ValueType::Vec3F64);
    assert_eq!(slot.frame_id, FrameId::World);
    assert_eq!(
        project.instances[0].slot_meta[2].point,
        Some([0.1, 0.2, 0.3])
    );
}

#[test]
fn body_force_rejects_non_world_frame() {
    let yaml = VALID.replace(
        "      - { signal: joint_torque, entity: j, required: true, limits: { max_abs: 5.0 } }",
        "      - { signal: body_force, entity: j, required: true, frame: joint_axis, limits: { max_abs: 1.0 } }",
    );
    let err = fail(&yaml);
    assert_eq!(err.path(), Some("bindings[0].slots[2].frame"));
    assert!(
        err.to_string().contains("must be 'world' or 'body_local'"),
        "{err}"
    );
}

#[test]
fn duplicate_entity_signal_direction_is_rejected() {
    let yaml = VALID.replace("joint_velocity", "joint_position");
    let err = fail(&yaml);
    assert!(err.to_string().contains("duplicate signal"), "{err}");
    assert_eq!(err.path(), Some("bindings[0].slots[1].signal"));
}

#[test]
fn async_sync_mode_is_rejected() {
    let err = fail(&VALID.replace("sync_mode: lockstep", "sync_mode: async"));
    assert_eq!(err.path(), Some("simulation.sync_mode"));
    assert!(err.to_string().contains("v1 does not support"), "{err}");
}

#[test]
fn zero_time_step_is_rejected() {
    let err = fail(&VALID.replace("basic_time_step_ms: 1.0", "basic_time_step_ms: 0"));
    assert_eq!(err.path(), Some("simulation.basic_time_step_ms"));
    assert!(err.to_string().contains("> 0"), "{err}");
}

#[test]
fn fractional_nanoseconds_are_rejected() {
    let err = fail(&VALID.replace("basic_time_step_ms: 1.0", "basic_time_step_ms: 0.0000005"));
    assert_eq!(err.path(), Some("simulation.basic_time_step_ms"));
    assert!(
        err.to_string().contains("integer number of nanoseconds"),
        "{err}"
    );
}

#[test]
fn missing_time_step_is_rejected() {
    let err = fail(&VALID.replace("  basic_time_step_ms: 1.0\n", ""));
    assert_eq!(err.path(), Some("simulation.basic_time_step_ms"));
}

#[test]
fn launch_and_attach_are_mutually_exclusive() {
    let yaml = VALID.replace(
        "    attach: true",
        "    attach: true\n    launch: { exe: bin/m }",
    );
    let err = fail(&yaml);
    assert_eq!(err.path(), Some("models.m"));
    assert!(err.to_string().contains("not both"), "{err}");
}

#[test]
fn spawn_paths_are_relative_to_project_file() {
    let yaml = VALID.replace(
        "    attach: true",
        "    launch:\n      exe: bin/model\n      args: [\"--x\", \"1\"]\n      cwd: run",
    );
    let path = write_project(&yaml);
    let project = load_project(&path).unwrap();
    let base = path.parent().unwrap();
    match &project.instances[0].launch {
        Launch::Spawn { exe, args, cwd } => {
            assert_eq!(exe, &base.join("bin/model"));
            assert_eq!(cwd, &base.join("run"));
            assert_eq!(args, &vec!["--x".to_string(), "1".to_string()]);
        }
        other => panic!("expected spawn, got {other:?}"),
    }
}

#[test]
fn custom_signal_is_resolved() {
    let yaml = r#"
coupling_version: 1
simulation: { basic_time_step_ms: 1.0 }
models:
  m: { attach: true }
instances:
  inst:
    model: m
    id: 1
    custom_signals:
      - { signal_id: 32769, name: dc_bus_voltage, value_type: f64, direction: to_model, unit: V }
bindings:
  - instance: inst
    entities:
      j: { webots_def: J, kind: joint }
    slots:
      - { signal: dc_bus_voltage, entity: j, required: true }
"#;
    let project = load(yaml).unwrap();
    let slot = &project.instances[0].slots[0];
    assert_eq!(slot.signal_id, 32769);
    assert_eq!(slot.value_type, ValueType::F64);
    assert_eq!(slot.direction, Direction::ToModel);
    let manifest = project.instances[0].to_manifest(&project.sim, 7);
    assert_eq!(manifest.slots[0].unit, "V");
    assert_eq!(manifest.slots[0].signal_id, 32769);
}

#[test]
fn custom_signal_id_must_be_in_custom_range() {
    let yaml = VALID.replace(
        "  inst: { model: m, id: 1 }",
        "  inst:\n    model: m\n    id: 1\n    custom_signals:\n      - { signal_id: 16, name: x, value_type: f64, direction: to_model, unit: V }",
    );
    let err = fail(&yaml);
    assert_eq!(
        err.path(),
        Some("instances.inst.custom_signals[0].signal_id")
    );
    assert!(err.to_string().contains("0x8000..=0xFFFF"), "{err}");
}

#[test]
fn duplicate_custom_signal_name_is_rejected() {
    let yaml = VALID.replace(
        "  inst: { model: m, id: 1 }",
        "  inst:\n    model: m\n    id: 1\n    custom_signals:\n      - { signal_id: 32769, name: x, value_type: f64, direction: to_model, unit: V }\n      - { signal_id: 32770, name: x, value_type: f64, direction: to_model, unit: V }",
    );
    let err = fail(&yaml);
    assert_eq!(err.path(), Some("instances.inst.custom_signals[1].name"));
    assert!(
        err.to_string().contains("duplicate custom signal name"),
        "{err}"
    );
}

#[test]
fn explicit_unit_dimension_must_match_signal() {
    let yaml = VALID.replace(
        "limits: { max_abs: 5.0 }",
        "unit: mm, limits: { max_abs: 5.0 }",
    );
    let err = fail(&yaml);
    assert_eq!(err.path(), Some("bindings[0].slots[2].unit"));
    assert!(err.to_string().contains("does not match signal"), "{err}");
    assert_eq!(Dim::Torque.canonical_unit(), "N*m");
}

#[test]
fn explicit_direction_must_match_registry() {
    let yaml = VALID.replace(
        "signal: joint_torque, entity: j",
        "signal: joint_torque, direction: to_model, entity: j",
    );
    let err = fail(&yaml);
    assert_eq!(err.path(), Some("bindings[0].slots[2].direction"));
    assert!(
        err.to_string().contains("conflicts with signal class"),
        "{err}"
    );
}

#[test]
fn unknown_fields_are_ignored() {
    let yaml = VALID
        .replace("coupling_version: 1", "coupling_version: 1\nfuture_top: 1")
        .replace(
            "  model_timeout_ms: 50",
            "  model_timeout_ms: 50\n  future_sim: 2",
        )
        .replace("    attach: true", "    attach: true\n    future_model: 3")
        .replace(
            "  inst: { model: m, id: 1 }",
            "  inst: { model: m, id: 1, future_inst: 4 }",
        )
        .replace("kind: joint }", "kind: joint, future_entity: 5 }")
        .replace(
            "signal: joint_position, entity: j, required: true }",
            "signal: joint_position, entity: j, required: true, future_slot: 6 }",
        );
    let project = load(&yaml).unwrap();
    assert_eq!(project.instances.len(), 1);
    assert_eq!(project.instances[0].slots.len(), 3);
}

#[test]
fn multiple_bindings_share_instance_and_continue_ids() {
    let yaml = r#"
coupling_version: 1
simulation: { basic_time_step_ms: 2.5 }
models:
  m: { attach: true }
instances:
  inst: { model: m, id: 3 }
bindings:
  - instance: inst
    entities:
      joint: { webots_def: J, kind: joint }
    slots:
      - { signal: joint_position, entity: joint }
  - instance: inst
    entities:
      link: { webots_def: L, kind: solid }
    slots:
      - { signal: body_position, entity: link }
      - { signal: body_orientation, entity: link }
"#;
    let project = load(yaml).unwrap();
    let inst = &project.instances[0];
    assert_eq!(project.sim.dt_ns, 2_500_000);
    assert_eq!(inst.entities.len(), 2);
    assert_eq!(inst.entities[0].entity_id, 1);
    assert_eq!(inst.entities[1].entity_id, 2);
    assert_eq!(
        inst.slots.iter().map(|s| s.slot_id).collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    assert_eq!(inst.slots[1].entity_id, 2);
    assert_eq!(inst.slots[1].frame_id, FrameId::World);
    assert_eq!(inst.slots[2].value_type, ValueType::QuatF64);
    assert!(inst.slot_meta[1].required); // default
}

#[test]
fn instances_are_sorted_by_id() {
    let yaml = VALID.replace(
        "  inst: { model: m, id: 1 }",
        "  zeta: { model: m, id: 9 }\n  alpha: { model: m, id: 2 }",
    );
    // `inst` is gone, so bindings must be dropped to keep the config valid.
    let yaml = yaml.replace(
        r#"bindings:
  - instance: inst
    entities:
      j: { webots_def: J, kind: joint }
    slots:
      - { signal: joint_position, entity: j, required: true }
      - { signal: joint_velocity, entity: j, required: true }
      - { signal: joint_torque, entity: j, required: true, limits: { max_abs: 5.0 } }"#,
        "bindings: []",
    );
    let project = load(&yaml).unwrap();
    assert_eq!(
        project
            .instances
            .iter()
            .map(|i| i.name.as_str())
            .collect::<Vec<_>>(),
        vec!["alpha", "zeta"]
    );
}

#[test]
fn world_def_check_reports_missing_names() {
    let dir = std::env::temp_dir().join(format!("janus-b3-defs-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let world = dir.join("world.wbt");
    std::fs::write(&world, "DEF   PRESENT Solid {\n}\nDEF\n  OTHER Robot {}\n").unwrap();
    assert_eq!(
        check_world_defs(&world, &["PRESENT", "OTHER", "ABSENT"]),
        vec!["ABSENT".to_string()]
    );
    assert_eq!(
        check_world_defs(&dir.join("does-not-exist.wbt"), &["A"]),
        vec!["A".to_string()]
    );
    let _ = std::fs::remove_file(&world);
}
