//! Cross-process test: the parent is the "host", the child process is the "model".
//!
//! The child is the same test binary, re-executed with `--exact child_process_worker
//! --ignored`, so no extra binary is needed (B2 acceptance criteria: write/read
//! consistency across processes through `std::process::Command`).

mod support;

use std::process::{Child, Command, ExitStatus};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use coupling_core::{HandshakeState, Validity};
use coupling_shm::{JsonArea, Notifier, Segment, WaitResult};

use support::{motor_layout, unique_names};

const CHILD_ENV: &str = "JANUS_B2_CHILD";
const MANIFEST: &[u8] = br#"{"manifest_version":1,"instance_id":9,"sync_mode":"lockstep"}"#;
const SIM_ID: u64 = 0x00c0_ffee_1234_5678;

fn wait_child(mut child: Child, timeout: Duration) -> ExitStatus {
    let start = Instant::now();
    loop {
        match child.try_wait().expect("try_wait") {
            Some(status) => return status,
            None if start.elapsed() > timeout => {
                let _ = child.kill();
                panic!("child process did not finish within {timeout:?}");
            }
            None => std::thread::sleep(Duration::from_millis(5)),
        }
    }
}

#[test]
fn cross_process_handshake_and_step() {
    if std::env::var_os(CHILD_ENV).is_some() {
        return;
    }

    let n = unique_names("xproc");
    let layout = motor_layout();
    let host = Segment::create(&n.segment, layout.total_size as usize).unwrap();
    host.init_from_layout(&layout, SIM_ID, 9, 50).unwrap();
    host.write_json(JsonArea::Manifest, MANIFEST).unwrap();
    let s2m = Notifier::create(&n.s2m).unwrap();
    let m2s = Notifier::create(&n.m2s).unwrap();

    let state = layout.slot(0).unwrap().clone();
    let effort = layout.slot(2).unwrap().clone();

    let exe = std::env::current_exe().expect("current_exe");
    let mut cmd = Command::new(exe);
    cmd.args([
        "--exact",
        "child_process_worker",
        "--ignored",
        "--nocapture",
    ])
    .env(CHILD_ENV, "1")
    .env("B2_SEGMENT", &n.segment)
    .env("B2_S2M", &n.s2m)
    .env("B2_M2S", &n.m2s)
    .env("B2_SIM_ID", SIM_ID.to_string())
    .env("B2_STATE_VAL", state.value_offset.to_string())
    .env("B2_STATE_VALIDITY", state.validity_offset.to_string())
    .env("B2_EFFORT_VAL", effort.value_offset.to_string())
    .env("B2_EFFORT_VALIDITY", effort.validity_offset.to_string());
    let child = cmd.spawn().expect("spawn child");

    // Adapter: step 1, state written, step_id last (release), then wake the model.
    host.write_f64(state.value_offset, 1.5);
    host.write_validity(state.validity_offset, Validity::Valid);
    host.header()
        .sim_time_ns
        .store(1_000_000, Ordering::Relaxed);
    host.header().dt_ns.store(1_000_000, Ordering::Relaxed);
    host.header()
        .adapter_heartbeat
        .fetch_add(1, Ordering::Relaxed);
    host.header().step_id.store(1, Ordering::Release);
    s2m.signal();

    // Adapter waits for the effort of step 1.
    assert_eq!(
        m2s.wait(Duration::from_secs(10), Duration::from_micros(200)),
        WaitResult::Signaled,
        "model process did not answer"
    );
    assert_eq!(host.header().effort_step_id.load(Ordering::Acquire), 1);
    assert_eq!(host.read_validity(effort.validity_offset), Validity::Valid);
    assert_eq!(host.read_f64(effort.value_offset), 0.25);
    // The model must not touch adapter-owned fields.
    assert_eq!(host.header().step_id.load(Ordering::Acquire), 1);
    assert_eq!(host.header().epoch.load(Ordering::Acquire), 0);
    assert_eq!(host.header().model_epoch_ack.load(Ordering::Acquire), 0);
    assert_eq!(host.header().model_heartbeat.load(Ordering::Acquire), 1);

    let status = wait_child(child, Duration::from_secs(10));
    assert!(status.success(), "child failed: {status:?}");
}

/// Child role of `cross_process_handshake_and_step`; ignored so that a normal test
/// run never executes it.
#[test]
#[ignore = "spawned by cross_process_handshake_and_step"]
fn child_process_worker() {
    if std::env::var_os(CHILD_ENV).is_none() {
        return;
    }
    let env = |k: &str| std::env::var(k).unwrap_or_else(|_| panic!("missing env {k}"));
    let off = |k: &str| {
        env(k)
            .parse::<u64>()
            .unwrap_or_else(|_| panic!("bad offset {k}"))
    };

    let segment_name = env("B2_SEGMENT");
    let sim_id: u64 = env("B2_SIM_ID").parse().expect("sim id");
    let state_val = off("B2_STATE_VAL");
    let state_validity = off("B2_STATE_VALIDITY");
    let effort_val = off("B2_EFFORT_VAL");
    let effort_validity = off("B2_EFFORT_VALIDITY");

    let seg = Segment::open(&segment_name).expect("child: open segment");
    let header = seg.header();
    header.validate().expect("child: header validate");
    assert_eq!(header.simulation_id, sim_id);
    assert_eq!(header.instance_id, 9);
    assert_eq!(
        header.hs_state.load(Ordering::Acquire),
        HandshakeState::Created.as_raw()
    );
    assert_eq!(
        seg.read_json(JsonArea::Manifest).expect("child: manifest"),
        MANIFEST.to_vec()
    );

    let s2m = Notifier::open(&env("B2_S2M")).expect("child: open s2m");
    let m2s = Notifier::open(&env("B2_M2S")).expect("child: open m2s");
    assert_eq!(
        s2m.wait(Duration::from_secs(10), Duration::from_micros(200)),
        WaitResult::Signaled,
        "child: no state event"
    );
    assert_eq!(header.step_id.load(Ordering::Acquire), 1);
    assert_eq!(header.sim_time_ns.load(Ordering::Acquire), 1_000_000);
    assert_eq!(header.dt_ns.load(Ordering::Acquire), 1_000_000);
    assert_eq!(seg.read_validity(state_validity), Validity::Valid);
    assert_eq!(seg.read_f64(state_val), 1.5);

    // Effort for step 1: values, then validity, then effort_step_id (release).
    seg.write_f64(effort_val, 0.25);
    seg.write_validity(effort_validity, Validity::Valid);
    header.model_heartbeat.fetch_add(1, Ordering::Relaxed);
    header.effort_step_id.store(1, Ordering::Release);
    m2s.signal();
}
