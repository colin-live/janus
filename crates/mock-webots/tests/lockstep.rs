//! B6 acceptance: 10 000 lockstep steps without loss or duplication.

mod common;

use std::time::Duration;

use common::{manifest, slot, unique_sim_id, Host};
use coupling_core::types::{Direction, Validity, ValueType};
use mock_webots::master::MasterChannel;

#[test]
fn ten_thousand_lockstep_steps_without_loss_or_duplication() {
    let sim_id = unique_sim_id();
    let instance_id = 1;
    let dt_ns = 1_000_000i64;
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
            0x0002,
            "joint_velocity",
            ValueType::F64,
            Direction::ToModel,
            "rad/s",
            true,
            None,
        ),
        slot(
            2,
            0x0101,
            "joint_torque",
            ValueType::F64,
            Direction::ToWebots,
            "N*m",
            true,
            Some(common::limits(10.0, None)),
        ),
    ];
    let host = Host::new(manifest(sim_id, instance_id, dt_ns as u64, 50, 3, slots));
    let model = common::spawn_echo_model(&host, "joint_torque", 0.1, Duration::ZERO);
    let mut channel =
        MasterChannel::attach(sim_id, instance_id, Duration::from_secs(2)).expect("attach");

    let position = host.slot_id("joint_position");
    let velocity = host.slot_id("joint_velocity");
    let torque = host.slot_id("joint_torque");

    for step in 1..=10_000u64 {
        channel.set_state_f64(position, step as f64 * 1e-6);
        channel.set_state_f64(velocity, 1.0);
        let result = channel.exchange((step as i64 - 1) * dt_ns, dt_ns);

        assert_eq!(result.step_id, step, "exchange step id");
        assert_eq!(host.step_id(), step, "header step id");
        assert_eq!(
            host.effort_step_id(),
            step,
            "model acknowledged the same step"
        );
        assert!(!result.model_timed_out, "step {step} timed out");
        assert_eq!(result.efforts.len(), 1);
        let effort = result.efforts[0];
        assert_eq!(effort.slot_id, torque);
        assert_eq!(effort.value.as_scalar(), Some(0.1));
        assert_eq!(effort.status, Validity::Valid);
    }

    let seen = model.seen_steps();
    assert_eq!(seen.len(), 10_000);
    assert_eq!(seen.first().copied(), Some(1));
    assert_eq!(seen.last().copied(), Some(10_000));
    assert!(seen.windows(2).all(|pair| pair[1] == pair[0] + 1));
}
