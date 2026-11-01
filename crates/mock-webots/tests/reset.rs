//! B6 acceptance: epoch reset semantics.

mod common;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use common::{limits, manifest, slot, spawn_echo_model, spawn_scripted_model, unique_sim_id, Host};
use coupling_core::types::{Direction, HandshakeState, Validity, ValueType};
use coupling_shm::Segment;
use mock_webots::master::MasterChannel;

const DT_NS: i64 = 1_000_000;

fn setup() -> (Host, MasterChannel) {
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
            Some(limits(10.0, None)),
        ),
    ];
    let host = Host::new(manifest(sim_id, instance_id, DT_NS as u64, 50, 3, slots));
    let channel =
        MasterChannel::attach(sim_id, instance_id, Duration::from_secs(2)).expect("attach");
    (host, channel)
}

#[test]
fn reset_bumps_epoch_clears_validity_and_restarts_step_ids() {
    let (host, mut channel) = setup();
    let model = spawn_echo_model(&host, "joint_torque", 0.2, Duration::from_millis(50));

    let first = channel.exchange(0, DT_NS);
    assert_eq!(first.step_id, 1);
    assert_eq!(host.epoch(), 0);
    assert_eq!(host.step_id(), 1);
    assert_eq!(channel.hs_state(), HandshakeState::Running);
    assert_eq!(host.validity("joint_torque"), Validity::Valid);

    // Observe the reset window from another mapping: Running must not appear after
    // Resetting until the model has acknowledged the new epoch.
    let stop = Arc::new(AtomicBool::new(false));
    let seen_resetting = Arc::new(AtomicBool::new(false));
    let running_after_reset = Arc::new(AtomicBool::new(false));
    let monitor = {
        let segment = Segment::open(&host.names.segment).expect("monitor opens segment");
        let stop = Arc::clone(&stop);
        let seen_resetting = Arc::clone(&seen_resetting);
        let running_after_reset = Arc::clone(&running_after_reset);
        thread::spawn(move || {
            while !stop.load(Ordering::Acquire) {
                let state =
                    HandshakeState::from_raw(segment.header().hs_state.load(Ordering::Acquire))
                        .unwrap_or(HandshakeState::Error);
                if state == HandshakeState::Resetting {
                    seen_resetting.store(true, Ordering::Release);
                }
                if state == HandshakeState::Running && seen_resetting.load(Ordering::Acquire) {
                    running_after_reset.store(true, Ordering::Release);
                }
                thread::sleep(Duration::from_micros(200));
            }
        })
    };

    channel
        .reset(Duration::from_secs(2))
        .expect("reset should be acknowledged");
    stop.store(true, Ordering::Release);
    monitor.join().unwrap();

    assert!(
        seen_resetting.load(Ordering::Acquire),
        "reset state was not observed"
    );
    assert!(
        !running_after_reset.load(Ordering::Acquire),
        "Running must not appear before the model acknowledges the epoch"
    );

    assert_eq!(host.epoch(), 1);
    assert_eq!(host.step_id(), 0);
    assert_eq!(host.effort_step_id(), 0);
    assert_eq!(host.model_epoch_ack(), 1);
    assert_eq!(channel.hs_state(), HandshakeState::Ready);
    assert_eq!(host.validity("joint_position"), Validity::Missing);
    assert_eq!(host.validity("joint_velocity"), Validity::Missing);
    assert_eq!(host.validity("joint_torque"), Validity::Missing);

    let position = host.slot_id("joint_position");
    let velocity = host.slot_id("joint_velocity");
    channel.set_state_f64(position, 0.5);
    channel.set_state_f64(velocity, 0.1);
    let second = channel.exchange(DT_NS, DT_NS);
    assert_eq!(second.step_id, 1, "step ids restart at 1 after reset");
    assert_eq!(host.step_id(), 1);
    assert_eq!(host.effort_step_id(), 1);
    assert_eq!(channel.hs_state(), HandshakeState::Running);

    let seen = model.seen_steps();
    assert_eq!(seen, vec![1, 1], "model saw step 1 in both epochs");
}

#[test]
fn reset_times_out_and_enters_error_when_the_model_does_not_ack() {
    let (host, mut channel) = setup();
    // No scripts: the model thread exits immediately and never acks an epoch.
    let _model = spawn_scripted_model(&host, "joint_torque", Vec::new());

    let started = Instant::now();
    let result = channel.reset(Duration::from_millis(30));
    assert!(result.is_err(), "reset must fail without an ack");
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(channel.hs_state(), HandshakeState::Error);
    assert_eq!(host.epoch(), 1);
    assert_eq!(host.step_id(), 0);
    assert_eq!(host.effort_step_id(), 0);
    assert_eq!(host.model_epoch_ack(), 0);
}
