//! B6 acceptance: fault injection for protocol §10.

mod common;

use std::time::{Duration, Instant};

use common::{
    limits, manifest, slot, spawn_scripted_model, unique_sim_id, Host, ModelEffort, ScriptStep,
};
use coupling_core::types::{Direction, HandshakeState, Validity, ValueType};
use mock_webots::master::{EffortValue, MasterChannel};

const DT_NS: i64 = 1_000_000;

fn host_for(
    required: bool,
    limits: Option<coupling_core::manifest::Limits>,
    model_timeout_ms: u32,
    max_misses: u32,
) -> (Host, u32) {
    let sim_id = unique_sim_id();
    let instance_id = 1;
    let slots = vec![slot(
        0,
        0x0101,
        "joint_torque",
        ValueType::F64,
        Direction::ToWebots,
        "N*m",
        required,
        limits,
    )];
    let host = Host::new(manifest(
        sim_id,
        instance_id,
        DT_NS as u64,
        model_timeout_ms,
        max_misses,
        slots,
    ));
    let torque = host.slot_id("joint_torque");
    (host, torque)
}

fn attach(host: &Host) -> MasterChannel {
    MasterChannel::attach(host.sim_id, host.instance_id, Duration::from_secs(2)).expect("attach")
}

#[test]
fn model_timeout_zeros_efforts_then_enters_error_after_consecutive_misses() {
    let (host, torque) = host_for(true, Some(limits(5.0, None)), 5, 3);
    let scripts = vec![
        ScriptStep {
            respond: false,
            ..ScriptStep::default()
        };
        3
    ];
    let _model = spawn_scripted_model(&host, "joint_torque", scripts);
    let mut channel = attach(&host);

    for step in 1..=3u64 {
        let started = Instant::now();
        let result = channel.exchange((step as i64 - 1) * DT_NS, DT_NS);
        assert_eq!(result.step_id, step);
        assert!(result.model_timed_out, "step {step} should time out");
        assert_eq!(result.efforts.len(), 1);
        assert_eq!(result.efforts[0].slot_id, torque);
        assert_eq!(result.efforts[0].value, EffortValue::Scalar(0.0));
        assert_eq!(result.efforts[0].status, Validity::Error);
        assert!(
            started.elapsed() < Duration::from_millis(100),
            "timeout took {:?}",
            started.elapsed()
        );
        if step < 3 {
            assert_eq!(channel.hs_state(), HandshakeState::Running);
        }
    }
    assert_eq!(channel.hs_state(), HandshakeState::Error);

    // Error is not a blocking state: exchange returns zeros immediately.
    let started = Instant::now();
    let result = channel.exchange(3 * DT_NS, DT_NS);
    assert!(started.elapsed() < Duration::from_millis(50));
    assert_eq!(result.step_id, 3, "no new step is published in Error");
    assert!(!result.model_timed_out);
    assert_eq!(result.efforts[0].value, EffortValue::Scalar(0.0));
    assert_eq!(result.efforts[0].status, Validity::Error);
}

#[test]
fn nan_and_inf_become_zero_with_invalid_status() {
    let (host, _) = host_for(true, Some(limits(5.0, None)), 20, 10);
    let scripts = vec![
        ScriptStep::scalar(f64::NAN),
        ScriptStep::scalar(f64::INFINITY),
        ScriptStep::scalar(f64::NEG_INFINITY),
    ];
    let _model = spawn_scripted_model(&host, "joint_torque", scripts);
    let mut channel = attach(&host);

    for step in 1..=3u64 {
        let result = channel.exchange((step as i64 - 1) * DT_NS, DT_NS);
        assert!(!result.model_timed_out);
        assert_eq!(result.efforts[0].value, EffortValue::Scalar(0.0));
        assert_eq!(result.efforts[0].status, Validity::Invalid);
        assert_eq!(channel.hs_state(), HandshakeState::Running);
    }
}

#[test]
fn stale_effort_step_id_is_not_applied() {
    let (host, _) = host_for(true, Some(limits(5.0, None)), 5, 10);
    let scripts = vec![
        ScriptStep {
            effort_step_id_ok: false,
            ..ScriptStep::scalar(3.0)
        },
        ScriptStep::scalar(4.0),
    ];
    let _model = spawn_scripted_model(&host, "joint_torque", scripts);
    let mut channel = attach(&host);

    let first = channel.exchange(0, DT_NS);
    assert!(
        first.model_timed_out,
        "stale effort_step_id must wait for timeout"
    );
    assert_eq!(first.efforts[0].value, EffortValue::Scalar(0.0));
    assert_eq!(first.efforts[0].status, Validity::Error);

    let second = channel.exchange(DT_NS, DT_NS);
    assert!(!second.model_timed_out);
    assert_eq!(second.efforts[0].value, EffortValue::Scalar(4.0));
    assert_eq!(second.efforts[0].status, Validity::Valid);
}

#[test]
fn clamping_then_rate_limiting_is_applied_per_component() {
    let (host, _) = host_for(true, Some(limits(5.0, Some(1000.0))), 20, 10);
    let scripts = vec![
        ScriptStep::scalar(0.0),
        ScriptStep::scalar(100.0),
        ScriptStep::scalar(-100.0),
    ];
    let _model = spawn_scripted_model(&host, "joint_torque", scripts);
    let mut channel = attach(&host);

    let first = channel.exchange(0, DT_NS);
    assert_eq!(first.efforts[0].value, EffortValue::Scalar(0.0));
    assert_eq!(first.efforts[0].status, Validity::Valid);

    let second = channel.exchange(DT_NS, DT_NS);
    assert_eq!(second.efforts[0].value, EffortValue::Scalar(1.0));
    assert_eq!(second.efforts[0].status, Validity::OutOfRange);

    let third = channel.exchange(2 * DT_NS, DT_NS);
    assert_eq!(third.efforts[0].value, EffortValue::Scalar(0.0));
    assert_eq!(third.efforts[0].status, Validity::OutOfRange);
}

#[test]
fn vec3_limits_are_applied_per_component() {
    let sim_id = unique_sim_id();
    let instance_id = 1;
    let slots = vec![slot(
        0,
        0x0110,
        "body_force",
        ValueType::Vec3F64,
        Direction::ToWebots,
        "N",
        true,
        Some(limits(5.0, Some(1000.0))),
    )];
    let host = Host::new(manifest(sim_id, instance_id, DT_NS as u64, 20, 10, slots));
    let scripts = vec![
        ScriptStep::vec3([0.0, 0.0, 0.0]),
        ScriptStep::vec3([10.0, -10.0, 2.0]),
    ];
    let _model = spawn_scripted_model(&host, "body_force", scripts);
    let mut channel = attach(&host);

    let _ = channel.exchange(0, DT_NS);
    let second = channel.exchange(DT_NS, DT_NS);
    assert_eq!(second.efforts[0].value, EffortValue::Vec3([1.0, -1.0, 1.0]));
    assert_eq!(second.efforts[0].status, Validity::OutOfRange);
}

#[test]
fn missing_limits_is_fail_safe() {
    let (host, _) = host_for(true, None, 20, 3);
    let _model = spawn_scripted_model(&host, "joint_torque", vec![ScriptStep::scalar(3.0)]);
    let mut channel = attach(&host);

    let result = channel.exchange(0, DT_NS);
    assert_eq!(result.efforts[0].value, EffortValue::Scalar(0.0));
    assert_eq!(result.efforts[0].status, Validity::Error);
    assert_eq!(channel.hs_state(), HandshakeState::Running);
}

#[test]
fn non_required_invalid_does_not_count_as_a_miss() {
    let (host, _) = host_for(false, Some(limits(5.0, None)), 20, 1);
    let scripts = vec![
        ScriptStep {
            validity: Validity::Invalid,
            effort: Some(ModelEffort::Scalar(3.0)),
            ..ScriptStep::default()
        };
        3
    ];
    let _model = spawn_scripted_model(&host, "joint_torque", scripts);
    let mut channel = attach(&host);

    for step in 1..=3u64 {
        let result = channel.exchange((step as i64 - 1) * DT_NS, DT_NS);
        assert_eq!(result.efforts[0].value, EffortValue::Scalar(0.0));
        assert_eq!(result.efforts[0].status, Validity::Invalid);
        assert_eq!(channel.hs_state(), HandshakeState::Running);
    }
}
