//! B6 API surface: state setters, report writing and lifecycle helpers.

mod common;

use std::sync::atomic::Ordering;
use std::time::Duration;

use common::{manifest, slot, unique_sim_id, Host};
use coupling_core::manifest::{EntityReport, Report};
use coupling_core::types::{Direction, HandshakeState, Validity, ValueType};
use coupling_shm::JsonArea;
use mock_webots::master::{MasterChannel, MasterError};

fn lifecycle_host() -> Host {
    let sim_id = unique_sim_id();
    let slots = vec![
        slot(
            0,
            0x0001,
            "joint_position",
            ValueType::F64,
            Direction::ToModel,
            "rad",
            true,
            None,
        ),
        slot(
            1,
            0x0101,
            "joint_torque",
            ValueType::F64,
            Direction::ToWebots,
            "N*m",
            true,
            Some(common::limits(10.0, None)),
        ),
    ];
    Host::new(manifest(sim_id, 1, 1_000_000, 20, 3, slots))
}

#[test]
fn setters_write_values_then_validity_and_lifecycle_is_exposed() {
    let sim_id = unique_sim_id();
    let instance_id = 1;
    let slots = vec![
        slot(
            0,
            0x0001,
            "joint_position",
            ValueType::F64,
            Direction::ToModel,
            "rad",
            true,
            None,
        ),
        slot(
            1,
            0x0010,
            "body_position",
            ValueType::Vec3F64,
            Direction::ToModel,
            "m",
            true,
            None,
        ),
        slot(
            2,
            0x0011,
            "body_orientation",
            ValueType::QuatF64,
            Direction::ToModel,
            "1",
            true,
            None,
        ),
        slot(
            3,
            0x0101,
            "joint_torque",
            ValueType::F64,
            Direction::ToWebots,
            "N*m",
            true,
            Some(common::limits(10.0, None)),
        ),
    ];
    let host = Host::new(manifest(sim_id, instance_id, 1_000_000, 20, 3, slots));
    let mut channel =
        MasterChannel::attach(sim_id, instance_id, Duration::from_secs(2)).expect("attach");

    let position = host.slot("joint_position");
    let body = host.slot("body_position");
    let orientation = host.slot("body_orientation");

    channel.set_state_f64(0, 1.25);
    channel.set_state_vec3(1, [1.0, 2.0, 3.0]);
    channel.set_state_quat(2, [0.0, 0.0, 0.0, 1.0]);

    assert_eq!(host.segment.read_f64(position.value_offset), 1.25);
    assert_eq!(
        host.segment.read_validity(position.validity_offset),
        Validity::Valid
    );
    assert_eq!(host.segment.read_f64(body.value_offset), 1.0);
    assert_eq!(host.segment.read_f64(body.value_offset + 8), 2.0);
    assert_eq!(host.segment.read_f64(body.value_offset + 16), 3.0);
    assert_eq!(
        host.segment.read_validity(body.validity_offset),
        Validity::Valid
    );
    assert_eq!(host.segment.read_f64(orientation.value_offset + 24), 1.0);
    assert_eq!(
        host.segment.read_validity(orientation.validity_offset),
        Validity::Valid
    );

    channel.set_state_invalid(0, Validity::Stale);
    assert_eq!(
        host.segment.read_validity(position.validity_offset),
        Validity::Stale
    );
    assert_eq!(host.segment.read_f64(position.value_offset), 0.0);

    let report = Report {
        ok: true,
        adapter: "mock-webots-test".to_string(),
        world_coordinate_system: "ENU".to_string(),
        entities: vec![EntityReport {
            entity_id: 1,
            resolved: true,
            node_type: Some("HingeJoint".to_string()),
            detail: None,
        }],
        errors: Vec::new(),
        warnings: Vec::new(),
    };
    channel.write_report(&report);
    let bytes = host
        .segment
        .read_json(JsonArea::Report)
        .expect("read report");
    let decoded: Report = serde_json::from_slice(&bytes).expect("decode report");
    assert_eq!(decoded, report);

    channel.set_paused();
    assert_eq!(channel.hs_state(), HandshakeState::Paused);
    let result = channel.exchange(0, 1_000_000);
    assert_eq!(result.step_id, 0);
    assert_eq!(host.step_id(), 0);

    channel.set_running();
    assert_eq!(channel.hs_state(), HandshakeState::Running);
    channel.set_closed();
    assert_eq!(channel.hs_state(), HandshakeState::Closed);
}

#[test]
fn terminal_states_are_not_overwritten_by_running_or_paused() {
    let host = lifecycle_host();
    let sim_id = host.sim_id;
    let channel =
        MasterChannel::attach(sim_id, host.instance_id, Duration::from_secs(2)).expect("attach");

    host.segment
        .header()
        .hs_state
        .store(HandshakeState::Error.as_raw(), Ordering::Release);

    channel.set_running();
    assert_eq!(channel.hs_state(), HandshakeState::Error);
    channel.set_paused();
    assert_eq!(channel.hs_state(), HandshakeState::Error);

    channel.set_closed();
    assert_eq!(channel.hs_state(), HandshakeState::Closed);
    channel.set_running();
    assert_eq!(channel.hs_state(), HandshakeState::Closed);
    channel.set_paused();
    assert_eq!(channel.hs_state(), HandshakeState::Closed);
}

#[test]
fn reset_on_closed_instance_is_rejected_without_side_effects() {
    let host = lifecycle_host();
    let sim_id = host.sim_id;
    let mut channel =
        MasterChannel::attach(sim_id, host.instance_id, Duration::from_secs(2)).expect("attach");
    channel.set_closed();

    let err = channel
        .reset(Duration::from_millis(10))
        .expect_err("reset on a closed instance must fail");
    assert!(matches!(
        err,
        MasterError::State {
            state: HandshakeState::Closed,
            ..
        }
    ));
    assert_eq!(channel.hs_state(), HandshakeState::Closed);
    assert_eq!(host.epoch(), 0);
    assert_eq!(host.step_id(), 0);
}
