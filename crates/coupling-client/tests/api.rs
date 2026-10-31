//! B4 API details: commit bookkeeping, disabled slots and validity of non-finite efforts.

mod support;

use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use coupling_client::{ClientError, ModelClient, WaitEvent};
use coupling_core::Validity;

use support::{hello, motor_slots, unique_sim_id, TestHost, TIMEOUT};

#[test]
fn commit_sequencing_is_enforced() {
    let sim_id = unique_sim_id();
    let host = TestHost::motor(sim_id, 4);
    let (tx, rx) = mpsc::channel();

    thread::scope(|scope| {
        scope.spawn(|| {
            let mut client = ModelClient::connect(sim_id, 4, hello(), TIMEOUT).expect("connect");
            let torque = client.slot("joint_torque").expect("torque slot");
            assert!(matches!(client.commit(), Err(ClientError::NoPendingStep)));

            let step = match client.wait_step(TIMEOUT).expect("step") {
                WaitEvent::Step(step) => step,
                other => panic!("expected a step, got {other:?}"),
            };
            assert_eq!(step.step_id, 1);
            assert!(
                matches!(
                    client.wait_step(TIMEOUT),
                    Err(ClientError::StepNotCommitted)
                ),
                "a second wait_step before commit must be rejected"
            );
            client.write_f64(torque, 2.0);
            client.commit().expect("first commit");
            assert!(
                matches!(client.commit(), Err(ClientError::NoPendingStep)),
                "a duplicate commit must be rejected"
            );
            tx.send(()).expect("send");

            assert!(matches!(
                client.wait_step(TIMEOUT),
                Err(ClientError::Closed)
            ));
        });

        host.wait_hello(TIMEOUT);
        host.set_ready();
        host.step(1, 1.0);
        rx.recv_timeout(TIMEOUT).expect("client ready");
        let (validity, torque) = host.await_effort(1, TIMEOUT);
        assert_eq!(validity, Validity::Valid);
        assert_eq!(torque, 2.0);
        host.close();
    });
}

#[test]
fn disabled_slots_are_hidden_from_the_model() {
    let sim_id = unique_sim_id();
    let mut slots = motor_slots();
    slots[1].enabled = false;
    let host = TestHost::new(sim_id, 5, slots);
    let (tx, rx) = mpsc::channel();

    thread::scope(|scope| {
        scope.spawn(|| {
            let client = ModelClient::connect(sim_id, 5, hello(), TIMEOUT).expect("connect");
            assert!(
                client.slot("joint_torque").is_none(),
                "disabled slot must be None"
            );
            assert!(client.slot("joint_position").is_some());
            assert!(client.slot("no_such_slot").is_none());
            let position = client.slot("joint_position").expect("position slot");
            assert!(client.read_f64(position).is_none());
            tx.send(()).expect("send");
        });

        host.wait_hello(TIMEOUT);
        host.set_ready();
        rx.recv_timeout(TIMEOUT).expect("client ready");
        host.close();
    });
}

#[test]
fn non_finite_effort_marks_the_slot_invalid() {
    let sim_id = unique_sim_id();
    let host = TestHost::motor(sim_id, 6);
    let (tx, rx) = mpsc::channel();

    thread::scope(|scope| {
        scope.spawn(|| {
            let mut client = ModelClient::connect(sim_id, 6, hello(), TIMEOUT).expect("connect");
            let torque = client.slot("joint_torque").expect("torque slot");
            let step = match client.wait_step(TIMEOUT).expect("step") {
                WaitEvent::Step(step) => step,
                other => panic!("expected a step, got {other:?}"),
            };
            assert_eq!(step.step_id, 1);
            client.write_f64(torque, f64::NAN);
            client.commit().expect("commit");
            tx.send(()).expect("send");
            let _ = client.wait_step(Duration::from_millis(100));
        });

        host.wait_hello(TIMEOUT);
        host.set_ready();
        host.step(1, 1.0);
        rx.recv_timeout(TIMEOUT).expect("client wrote an effort");
        let (validity, value) = host.await_effort(1, TIMEOUT);
        assert_eq!(validity, Validity::Invalid);
        assert_eq!(value, 0.0);

        host.close();
    });
}
