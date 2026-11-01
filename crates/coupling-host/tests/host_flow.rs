//! End-to-end tests of the host with the real B4 SDK as the model side (in-process threads for the
//! "attach" cases, the `echo_model` example process for the "spawn" cases). No Webots involved.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use coupling_client::{ClientError, ModelClient, WaitEvent};
use coupling_config::load_project;
use coupling_core::manifest::{Hello, Report};
use coupling_core::HandshakeState;
use coupling_host::{Host, HostError, HostOptions, TickOutcome};
use coupling_shm::{names, JsonArea, Segment};

static COUNTER: AtomicU64 = AtomicU64::new(1);

fn unique_sim_id() -> u64 {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .subsec_nanos() as u64;
    ((std::process::id() as u64) << 32) | ((nanos & 0xFFFF) << 16) | (n & 0xFFFF) | 1
}

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "janus-b5-it-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn echo_model_path() -> PathBuf {
    let mut p = std::env::current_exe().unwrap();
    p.pop(); // deps
    p.pop(); // debug
    p.push("examples");
    p.push(if cfg!(windows) {
        "echo_model.exe"
    } else {
        "echo_model"
    });
    assert!(
        p.is_file(),
        "example binary missing: {} (run `cargo build --examples -p coupling-host`)",
        p.display()
    );
    p
}

fn slash(p: &Path) -> String {
    p.display().to_string().replace('\\', "/")
}

/// `model_block` is the body of `models.mdl`, e.g. `attach: true` or a `launch:` mapping.
fn write_project(dir: &Path, model_block: &str) -> PathBuf {
    let yaml = format!(
        r#"coupling_version: 1
simulation:
  basic_time_step_ms: 1.0
  sync_mode: lockstep
  model_timeout_ms: 50
  effort_policy: {{ ttl_steps: 1, on_timeout: zero, max_consecutive_misses: 3 }}
models:
  mdl:
    {model_block}
instances:
  m1: {{ model: mdl, id: 1 }}
bindings:
  - instance: m1
    entities:
      joint: {{ webots_def: J, kind: joint }}
    slots:
      - {{ signal: joint_position, entity: joint, required: true }}
      - {{ signal: joint_velocity, entity: joint, required: false }}
      - {{ signal: joint_torque, entity: joint, required: true, limits: {{ max_abs: 5.0 }} }}
"#
    );
    let path = dir.join("project.yaml");
    std::fs::write(&path, yaml).unwrap();
    path
}

fn attach_project(dir: &TempDir) -> coupling_config::ResolvedProject {
    load_project(&write_project(&dir.0, "attach: true")).unwrap()
}

fn spawn_project(dir: &TempDir, extra_args: &[&str]) -> coupling_config::ResolvedProject {
    let args = extra_args
        .iter()
        .map(|a| format!("\"{a}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let block = format!(
        "launch: {{ exe: \"{}\", args: [{}] }}",
        slash(&echo_model_path()),
        args
    );
    load_project(&write_project(&dir.0, &block)).unwrap()
}

fn opts(sim_id: u64, spawn: bool, hello_ms: u64) -> HostOptions {
    HostOptions {
        sim_id,
        spawn,
        hello_timeout: Duration::from_millis(hello_ms),
    }
}

fn hello(signals: &[u32], steppable: bool) -> Hello {
    Hello {
        protocol: [1, 0],
        model_name: "test".into(),
        model_version: "0".into(),
        steppable,
        sync_modes: vec!["lockstep".into()],
        internal_dt_ns: None,
        supported_signals: signals.to_vec(),
        custom_signals: vec![],
    }
}

/// A model thread: connect, then serve steps until stopped / closed.
fn model_thread(
    sim_id: u64,
    h: Hello,
    stop: Arc<AtomicBool>,
) -> JoinHandle<Result<(), ClientError>> {
    thread::spawn(move || {
        let mut c = ModelClient::connect(sim_id, 1, h, Duration::from_secs(5))?;
        while !stop.load(Ordering::Relaxed) {
            match c.wait_step(Duration::from_millis(20)) {
                Ok(WaitEvent::Step(_)) => c.commit()?,
                Ok(WaitEvent::Reset { .. }) => c.ack_reset(),
                Ok(WaitEvent::Timeout) => {}
                Err(ClientError::Closed) => return Ok(()),
                Err(e) => return Err(e),
            }
        }
        Ok(())
    })
}

fn wait_until(limit: Duration, mut f: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + limit;
    while Instant::now() < end {
        if f() {
            return true;
        }
        thread::sleep(Duration::from_millis(10));
    }
    f()
}

fn report_of(sim_id: u64) -> Report {
    let seg = Segment::open(&names(sim_id, 1).segment).unwrap();
    serde_json::from_slice(&seg.read_json(JsonArea::Report).unwrap()).unwrap()
}

// ---------------------------------------------------------------------------------------- handshake

#[test]
fn attach_handshake_reaches_ready_and_publishes_the_final_manifest() {
    let dir = TempDir::new();
    let sim_id = unique_sim_id();
    let stop = Arc::new(AtomicBool::new(false));
    let model = model_thread(sim_id, hello(&[1, 2, 257], true), stop.clone());

    let mut host = Host::start(&attach_project(&dir), &opts(sim_id, false, 5000)).unwrap();
    assert_eq!(
        host.states(),
        vec![("m1".to_string(), HandshakeState::Ready)]
    );
    let m = host.manifest("m1").unwrap();
    assert_eq!(m.slots.len(), 3);
    assert!(m.slots.iter().all(|s| s.enabled));
    assert_eq!(
        host.segment_names("m1").unwrap().segment,
        names(sim_id, 1).segment
    );

    stop.store(true, Ordering::Relaxed);
    host.shutdown();
    assert_eq!(host.states()[0].1, HandshakeState::Closed);
    let _ = model.join().unwrap();
}

#[test]
fn unsupported_optional_slot_is_disabled_in_the_final_manifest() {
    let dir = TempDir::new();
    let sim_id = unique_sim_id();
    let stop = Arc::new(AtomicBool::new(false));
    let model = model_thread(sim_id, hello(&[1, 257], true), stop.clone());

    let mut host = Host::start(&attach_project(&dir), &opts(sim_id, false, 5000)).unwrap();
    let m = host.manifest("m1").unwrap();
    assert!(!m.slot_by_name("joint_velocity").unwrap().enabled);
    assert!(m.slot_by_name("joint_torque").unwrap().enabled);
    stop.store(true, Ordering::Relaxed);
    host.shutdown();
    let _ = model.join().unwrap();
}

#[test]
fn missing_required_signal_fails_the_handshake_and_the_model_sees_the_reason() {
    let dir = TempDir::new();
    let sim_id = unique_sim_id();
    let stop = Arc::new(AtomicBool::new(false));
    let model = model_thread(sim_id, hello(&[1, 2], true), stop);

    let err = match Host::start(&attach_project(&dir), &opts(sim_id, false, 5000)) {
        Err(e) => e,
        Ok(_) => panic!("handshake must fail"),
    };
    assert_eq!(err.exit_code(), 2);
    assert!(err.to_string().contains("joint_torque"), "{err}");
    let model_err = model.join().unwrap().unwrap_err();
    assert!(
        model_err.to_string().contains("joint_torque"),
        "the model must see the reason: {model_err}"
    );
}

#[test]
fn non_steppable_model_is_refused() {
    let dir = TempDir::new();
    let sim_id = unique_sim_id();
    let model = model_thread(
        sim_id,
        hello(&[1, 2, 257], false),
        Arc::new(AtomicBool::new(false)),
    );
    let err = Host::start(&attach_project(&dir), &opts(sim_id, false, 5000))
        .err()
        .expect("must fail");
    assert!(err.to_string().contains("steppable"), "{err}");
    let _ = model.join();
}

#[test]
fn hello_timeout_is_a_handshake_error_with_exit_code_2() {
    let dir = TempDir::new();
    let err = Host::start(&attach_project(&dir), &opts(unique_sim_id(), false, 150))
        .err()
        .expect("must fail");
    assert!(matches!(err, HostError::Handshake { .. }));
    assert_eq!(err.exit_code(), 2);
    assert!(err.to_string().contains("no hello within 150 ms"), "{err}");
}

// ---------------------------------------------------------------------------------------- processes

#[test]
fn spawned_model_connects_and_is_cleaned_up_on_shutdown() {
    let dir = TempDir::new();
    let sim_id = unique_sim_id();
    let project = spawn_project(&dir, &[]);
    let mut host = Host::start(&project, &opts(sim_id, true, 10_000)).unwrap();
    assert_eq!(host.live_children(), 1);

    let run = host.publish_run_file(&project.base_dir).unwrap();
    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&run).unwrap()).unwrap();
    assert_eq!(doc["sim_id"], format!("{sim_id:016x}"));
    assert_eq!(doc["instances"][0]["name"], "m1");

    assert_eq!(host.tick(), TickOutcome::Continue);
    host.shutdown();
    assert_eq!(
        host.live_children(),
        0,
        "the model process must be gone after shutdown"
    );
    assert!(!run.exists(), "run.json must be removed");
}

#[test]
fn model_that_cannot_be_started_is_a_spawn_error() {
    let dir = TempDir::new();
    let block = format!(
        "launch: {{ exe: \"{}\" }}",
        slash(&dir.0.join("no-such-model"))
    );
    let project = load_project(&write_project(&dir.0, &block)).unwrap();
    let err = Host::start(&project, &opts(unique_sim_id(), true, 1000))
        .err()
        .expect("must fail");
    assert!(matches!(err, HostError::Spawn { .. }), "{err}");
    assert_eq!(err.exit_code(), 2);
}

#[test]
fn spawned_model_with_missing_signal_fails_fast() {
    let dir = TempDir::new();
    let project = spawn_project(&dir, &["--signals", "1,2"]);
    let err = Host::start(&project, &opts(unique_sim_id(), true, 10_000))
        .err()
        .expect("must fail");
    assert!(err.to_string().contains("joint_torque"), "{err}");
}

#[test]
fn crashing_model_process_flips_the_instance_to_error() {
    let dir = TempDir::new();
    let sim_id = unique_sim_id();
    let project = spawn_project(&dir, &["--crash-after-ms", "300"]);
    let mut host = Host::start(&project, &opts(sim_id, true, 10_000)).unwrap();

    let went_error = wait_until(Duration::from_secs(3), || {
        host.tick();
        host.states()[0].1 == HandshakeState::Error
    });
    assert!(went_error, "the crash must be noticed within 3 s");
    let report = report_of(sim_id);
    assert!(!report.ok);
    assert!(
        report.errors[0].contains("exited unexpectedly"),
        "{:?}",
        report.errors
    );
    host.shutdown();
}

// ---------------------------------------------------------------------------------------- supervision

/// Connected-but-idle model + an adapter stand-in that drives the header directly.
struct Idle {
    host: Host,
    adapter: Segment,
    stop: Arc<AtomicBool>,
    model: JoinHandle<Result<(), ClientError>>,
    sim_id: u64,
    _dir: TempDir,
}

fn idle_running_instance() -> Idle {
    let dir = TempDir::new();
    let sim_id = unique_sim_id();
    let stop = Arc::new(AtomicBool::new(false));
    let model = model_thread(sim_id, hello(&[1, 2, 257], true), stop.clone());
    let host = Host::start(&attach_project(&dir), &opts(sim_id, false, 5000)).unwrap();
    let adapter = Segment::open(&names(sim_id, 1).segment).unwrap();
    adapter
        .header()
        .hs_state
        .store(HandshakeState::Running.as_raw(), Ordering::Release);
    Idle {
        host,
        adapter,
        stop,
        model,
        sim_id,
        _dir: dir,
    }
}

#[test]
fn model_silent_while_the_adapter_steps_is_marked_stalled() {
    let mut t = idle_running_instance();
    // The adapter "steps" (heartbeat moves) but never wakes the model: the model heartbeat stays put.
    let went_error = wait_until(Duration::from_secs(3), || {
        t.adapter
            .header()
            .adapter_heartbeat
            .fetch_add(1, Ordering::Release);
        t.host.tick();
        t.host.states()[0].1 == HandshakeState::Error
    });
    assert!(went_error, "stall must be detected");
    assert!(report_of(t.sim_id).errors[0].contains("stalled"));
    t.stop.store(true, Ordering::Relaxed);
    t.host.shutdown();
    let _ = t.model.join();
}

#[test]
fn paused_simulation_is_not_mistaken_for_a_stall() {
    let mut t = idle_running_instance();
    // State says Running but nobody steps (e.g. paused from the Webots GUI): heartbeats do not move.
    let end = Instant::now() + Duration::from_millis(600);
    while Instant::now() < end {
        t.host.tick();
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(t.host.states()[0].1, HandshakeState::Running);
    t.stop.store(true, Ordering::Relaxed);
    t.host.shutdown();
    let _ = t.model.join();
}

#[test]
fn adapter_closing_every_instance_ends_the_run() {
    let mut t = idle_running_instance();
    assert_eq!(t.host.tick(), TickOutcome::Continue);
    t.adapter
        .header()
        .hs_state
        .store(HandshakeState::Closed.as_raw(), Ordering::Release);
    assert_eq!(t.host.tick(), TickOutcome::AllClosed);
    t.stop.store(true, Ordering::Relaxed);
    t.host.shutdown();
    let _ = t.model.join();
}

#[test]
fn an_error_written_by_the_adapter_is_kept_and_not_overwritten() {
    let mut t = idle_running_instance();
    let report = serde_json::to_vec(&Report {
        ok: false,
        adapter: "adapter".into(),
        world_coordinate_system: String::new(),
        entities: vec![],
        errors: vec!["DEF J not found".into()],
        warnings: vec![],
    })
    .unwrap();
    t.adapter.write_json(JsonArea::Report, &report).unwrap();
    t.adapter
        .header()
        .hs_state
        .store(HandshakeState::Error.as_raw(), Ordering::Release);
    t.host.tick();
    assert_eq!(
        report_of(t.sim_id).errors,
        vec!["DEF J not found".to_string()]
    );
    t.stop.store(true, Ordering::Relaxed);
    t.host.shutdown();
    let _ = t.model.join();
}
