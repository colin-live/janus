//! B4 acceptance: 10 000-step lockstep, spurious wakeups and epoch reset.
//!
//! Each test runs a real segment / notifier pair (B2) and an in-process "host + adapter"
//! thread, so the client uses the same code path as a separate model process.

mod support;

use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use coupling_client::{ClientError, ModelClient, Step, WaitEvent};
use coupling_core::Validity;

use support::{hello, unique_sim_id, TestHost, TIMEOUT};

const STEPS: u64 = 10_000;

#[test]
fn ten_thousand_lockstep_steps_without_loss_or_duplication() {
    let sim_id = unique_sim_id();
    let host = TestHost::motor(sim_id, 1);
    let (tx, rx) = mpsc::channel();

    thread::scope(|scope| {
        scope.spawn(|| {
            let mut client = ModelClient::connect(sim_id, 1, hello(), TIMEOUT).expect("connect");
            let position = client.slot("joint_position").expect("position slot");
            let torque = client.slot("joint_torque").expect("torque slot");
            let mut expected = 1;
            while expected <= STEPS {
                match client.wait_step(TIMEOUT) {
                    Ok(WaitEvent::Step(step)) => {
                        assert_eq!(step.step_id, expected, "step ids must be consecutive");
                        let p = client.read_f64(position).expect("position must be valid");
                        assert_eq!(p, expected as f64 * 0.5);
                        client.write_f64(torque, p * 2.0);
                        client.commit().expect("commit");
                        tx.send(step.step_id).expect("send");
                        expected += 1;
                    }
                    Ok(WaitEvent::Timeout) => {
                        panic!("client timed out waiting for step {expected}")
                    }
                    Ok(WaitEvent::Reset { epoch }) => {
                        panic!("unexpected reset to epoch {epoch} at step {expected}")
                    }
                    Err(e) => panic!("client failed at step {expected}: {e}"),
                }
            }
        });

        host.wait_hello(TIMEOUT);
        host.set_ready();
        for n in 1..=STEPS {
            host.step(n, n as f64 * 0.5);
            assert_eq!(rx.recv_timeout(TIMEOUT).expect("step not acknowledged"), n);
            let (validity, torque) = host.await_effort(n, TIMEOUT);
            assert_eq!(
                validity,
                Validity::Valid,
                "effort of step {n} must be valid"
            );
            assert_eq!(torque, n as f64, "effort applied exactly once for step {n}");
        }
        host.close();
    });
}

#[test]
fn repeated_signals_do_not_advance_the_step() {
    let sim_id = unique_sim_id();
    let host = TestHost::motor(sim_id, 2);
    let (tx, rx) = mpsc::channel();

    thread::scope(|scope| {
        scope.spawn(|| {
            let mut client = ModelClient::connect(sim_id, 2, hello(), TIMEOUT).expect("connect");
            let position = client.slot("joint_position").expect("position slot");
            let torque = client.slot("joint_torque").expect("torque slot");
            loop {
                match client.wait_step(TIMEOUT) {
                    Ok(WaitEvent::Step(step)) => {
                        let p = client.read_f64(position).unwrap_or(0.0);
                        client.write_f64(torque, p);
                        client.commit().expect("commit");
                        tx.send(step).expect("send");
                    }
                    Ok(WaitEvent::Timeout) => panic!("client timed out"),
                    Ok(WaitEvent::Reset { epoch }) => panic!("unexpected reset to epoch {epoch}"),
                    Err(ClientError::Closed) => break,
                    Err(e) => panic!("client failed: {e}"),
                }
            }
        });

        host.wait_hello(TIMEOUT);
        host.set_ready();

        host.step(1, 1.0);
        assert_eq!(rx.recv_timeout(TIMEOUT).expect("step 1").step_id, 1);
        host.await_effort(1, TIMEOUT);

        for _ in 0..3 {
            host.s2m.signal();
        }
        assert!(
            rx.recv_timeout(Duration::from_millis(200)).is_err(),
            "spurious signals must not produce a step"
        );

        host.step(2, 2.0);
        assert_eq!(rx.recv_timeout(TIMEOUT).expect("step 2").step_id, 2);
        let (validity, torque) = host.await_effort(2, TIMEOUT);
        assert_eq!(validity, Validity::Valid);
        assert_eq!(torque, 2.0);

        host.close();
    });
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Event {
    Step(Step),
    Reset(u64),
}

#[test]
fn epoch_reset_is_reported_acked_then_step_ids_restart_at_one() {
    let sim_id = unique_sim_id();
    let host = TestHost::motor(sim_id, 3);
    let (tx, rx) = mpsc::channel();

    thread::scope(|scope| {
        scope.spawn(|| {
            let mut client = ModelClient::connect(sim_id, 3, hello(), TIMEOUT).expect("connect");
            let position = client.slot("joint_position").expect("position slot");
            let torque = client.slot("joint_torque").expect("torque slot");
            let mut internal = 0.0;
            loop {
                match client.wait_step(TIMEOUT) {
                    Ok(WaitEvent::Step(step)) => {
                        internal += client.read_f64(position).unwrap_or(0.0);
                        client.write_f64(torque, internal);
                        client.commit().expect("commit");
                        tx.send(Event::Step(step)).expect("send");
                    }
                    Ok(WaitEvent::Reset { epoch }) => {
                        // Model-side reset: reinitialise, then acknowledge. No step exists
                        // for a reset wakeup, so commit must reject it.
                        internal = 0.0;
                        assert!(
                            matches!(client.commit(), Err(ClientError::NoPendingStep)),
                            "commit after a reset event must fail with NoPendingStep"
                        );
                        client.ack_reset();
                        tx.send(Event::Reset(epoch)).expect("send");
                    }
                    Ok(WaitEvent::Timeout) => panic!("client timed out"),
                    Err(ClientError::Closed) => break,
                    Err(e) => panic!("client failed: {e}"),
                }
            }
        });

        host.wait_hello(TIMEOUT);
        host.set_ready();

        host.step(1, 1.0);
        let first = match rx.recv_timeout(TIMEOUT).expect("step 1") {
            Event::Step(step) => step,
            other => panic!("expected step 1, got {other:?}"),
        };
        assert_eq!(first.step_id, 1);
        assert_eq!(first.epoch, 0);
        assert_eq!(host.await_effort(1, TIMEOUT), (Validity::Valid, 1.0));

        let epoch = host.begin_reset();
        assert_eq!(
            rx.recv_timeout(TIMEOUT).expect("reset event"),
            Event::Reset(epoch),
            "the epoch change must be reported as a reset event without a step"
        );
        host.wait_ack(epoch, TIMEOUT);
        assert_eq!(
            host.header()
                .step_id
                .load(std::sync::atomic::Ordering::Acquire),
            0,
            "the model must ack the reset without fabricating a step"
        );
        assert_eq!(
            host.header()
                .effort_step_id
                .load(std::sync::atomic::Ordering::Acquire),
            0
        );

        host.step(1, 3.0);
        let reset = match rx.recv_timeout(TIMEOUT).expect("first step of new epoch") {
            Event::Step(step) => step,
            other => panic!("expected the first step of the new epoch, got {other:?}"),
        };
        assert_eq!(reset.step_id, 1, "step ids restart at 1");
        assert_eq!(reset.epoch, epoch);
        assert_eq!(
            host.await_effort(1, TIMEOUT),
            (Validity::Valid, 3.0),
            "model state must be reinitialised on the epoch change"
        );

        host.step(2, 4.0);
        let next = match rx.recv_timeout(TIMEOUT).expect("second step of new epoch") {
            Event::Step(step) => step,
            other => panic!("expected the second step of the new epoch, got {other:?}"),
        };
        assert_eq!(next.step_id, 2);
        assert_eq!(next.epoch, epoch);
        assert_eq!(host.await_effort(2, TIMEOUT), (Validity::Valid, 7.0));

        host.close();
    });
}
