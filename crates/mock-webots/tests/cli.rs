//! B6 acceptance: the `mock-webots` executable runs a closed loop against a fake
//! model and writes the CSV trace.

mod common;

use std::process::Command;
use std::time::Duration;

use common::{limits, manifest, slot, spawn_echo_model, unique_sim_id, Host};
use coupling_core::types::{Direction, HandshakeState, ValueType};

#[test]
fn executable_closes_the_loop_and_writes_csv() {
    let sim_id = unique_sim_id();
    let instance_id = 7;
    let dt_ns = 1_000_000u64;
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
    let host = Host::new(manifest(sim_id, instance_id, dt_ns, 50, 3, slots));
    let _model = spawn_echo_model(&host, "joint_torque", 0.5, Duration::ZERO);

    let csv = std::env::temp_dir().join(format!("mock-webots-cli-{sim_id:016x}.csv"));
    let _ = std::fs::remove_file(&csv);

    let output = Command::new(env!("CARGO_BIN_EXE_mock-webots"))
        .args([
            "--sim-id",
            &format!("{sim_id:016x}"),
            "--instance-id",
            &instance_id.to_string(),
            "--steps",
            "20",
            "--inertia",
            "0.01",
            "--damping",
            "0.001",
            "--attach-timeout-ms",
            "3000",
            "--csv",
            csv.to_str().expect("csv path"),
        ])
        .output()
        .expect("run mock-webots");

    assert!(
        output.status.success(),
        "mock-webots failed: status {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let text = std::fs::read_to_string(&csv).expect("read csv");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 21, "header + 20 steps");
    assert_eq!(lines[0], "step,t,theta,omega,torque,status");
    let first: Vec<&str> = lines[1].split(',').collect();
    assert_eq!(first.len(), 6);
    assert_eq!(first[0], "1");
    assert_eq!(first[1], "0");
    assert_eq!(first[4], "0.5");
    assert_eq!(first[5], "valid");
    let last: Vec<&str> = lines[20].split(',').collect();
    assert_eq!(last[0], "20");
    assert_eq!(last[5], "valid");

    assert_eq!(
        host.state(),
        HandshakeState::Closed,
        "the executable must close the segment when done"
    );

    let _ = std::fs::remove_file(&csv);
}
