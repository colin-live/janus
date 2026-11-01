//! Tests of the `coupling-host` binary itself: exit codes, messages, the `run` life cycle.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use coupling_core::HandshakeState;
use coupling_shm::{names, Segment};

const HOST: &str = env!("CARGO_BIN_EXE_coupling-host");
static COUNTER: AtomicU64 = AtomicU64::new(1);

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "janus-b5-cli-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }
    fn file(&self, name: &str, content: &str) -> PathBuf {
        let p = self.0.join(name);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&p, content).unwrap();
        p
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn echo_model_path() -> String {
    let mut p = std::env::current_exe().unwrap();
    p.pop();
    p.pop();
    p.push("examples");
    p.push(if cfg!(windows) {
        "echo_model.exe"
    } else {
        "echo_model"
    });
    assert!(p.is_file(), "example binary missing: {}", p.display());
    p.display().to_string().replace('\\', "/")
}

fn project_yaml(world: Option<&str>, model_block: &str, torque_limits: &str) -> String {
    let world = world.map(|w| format!("  world: {w}\n")).unwrap_or_default();
    format!(
        r#"coupling_version: 1
simulation:
{world}  basic_time_step_ms: 1.0
  sync_mode: lockstep
  model_timeout_ms: 50
models:
  mdl:
    {model_block}
instances:
  m1: {{ model: mdl, id: 1 }}
bindings:
  - instance: m1
    entities:
      joint: {{ webots_def: ELBOW_JOINT, kind: joint }}
    slots:
      - {{ signal: joint_position, entity: joint, required: true }}
      - {{ signal: joint_torque, entity: joint, required: true{torque_limits} }}
"#
    )
}

const LIMITS: &str = ", limits: { max_abs: 5.0 }";

fn run_host(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(HOST).args(args).output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn p(path: &Path) -> &str {
    path.to_str().unwrap()
}

#[test]
fn check_accepts_a_valid_project_even_when_world_and_model_do_not_exist_yet() {
    let dir = TempDir::new();
    let proj = dir.file(
        "project.yaml",
        &project_yaml(
            Some("worlds/none.wbt"),
            "launch: { exe: bin/model }",
            LIMITS,
        ),
    );
    let (code, out, err) = run_host(&["check", "--project", p(&proj)]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("OK: 1 instance(s), 2 slot(s)"), "{out}");
    assert!(
        err.contains("world file") && err.contains("model executable"),
        "warnings expected: {err}"
    );
}

#[test]
fn check_fails_with_exit_code_1_when_the_world_lacks_a_bound_def() {
    let dir = TempDir::new();
    dir.file("worlds/arm.wbt", "DEF SHOULDER_JOINT HingeJoint {}\n");
    let proj = dir.file(
        "project.yaml",
        &project_yaml(Some("worlds/arm.wbt"), "attach: true", LIMITS),
    );
    let (code, _, err) = run_host(&["check", "-p", p(&proj)]);
    assert_eq!(code, 1);
    assert!(
        err.contains("DEF ELBOW_JOINT not found") && err.contains("instance m1"),
        "{err}"
    );
}

#[test]
fn check_passes_when_the_world_has_the_def() {
    let dir = TempDir::new();
    dir.file("worlds/arm.wbt", "DEF ELBOW_JOINT HingeJoint {}\n");
    let proj = dir.file(
        "project.yaml",
        &project_yaml(Some("worlds/arm.wbt"), "attach: true", LIMITS),
    );
    assert_eq!(run_host(&["check", "-p", p(&proj)]).0, 0);
}

#[test]
fn check_reports_the_config_path_of_a_missing_effort_limit() {
    let dir = TempDir::new();
    let proj = dir.file("project.yaml", &project_yaml(None, "attach: true", ""));
    let (code, _, err) = run_host(&["check", "-p", p(&proj)]);
    assert_eq!(code, 1);
    assert!(
        err.contains("bindings[0].slots[1]") && err.contains("limits"),
        "{err}"
    );
}

#[test]
fn manifest_prints_parseable_json_and_is_deterministic() {
    let dir = TempDir::new();
    let proj = dir.file("project.yaml", &project_yaml(None, "attach: true", LIMITS));
    let (code, out, _) = run_host(&[
        "manifest",
        "-p",
        p(&proj),
        "--instance",
        "m1",
        "--sim-id",
        "ABC",
    ]);
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["simulation_id"], "0x0000000000000abc");
    assert_eq!(v["slots"].as_array().unwrap().len(), 2);
    let (_, again, _) = run_host(&[
        "manifest",
        "-p",
        p(&proj),
        "--instance",
        "m1",
        "--sim-id",
        "ABC",
    ]);
    assert_eq!(out, again);

    let (code, out, _) = run_host(&["manifest", "-p", p(&proj)]);
    assert_eq!(code, 0);
    assert!(serde_json::from_str::<serde_json::Value>(&out)
        .unwrap()
        .is_array());

    assert_eq!(
        run_host(&["manifest", "-p", p(&proj), "--instance", "nope"]).0,
        1
    );
}

#[test]
fn usage_errors_exit_with_1() {
    assert_eq!(run_host(&[]).0, 1);
    assert_eq!(run_host(&["check"]).0, 1);
    assert_eq!(run_host(&["--help"]).0, 0);
    assert_eq!(run_host(&["check", "-p", "no-such-file.yaml"]).0, 1);
}

#[test]
fn run_without_a_model_exits_with_2_and_says_why() {
    let dir = TempDir::new();
    let proj = dir.file("project.yaml", &project_yaml(None, "attach: true", LIMITS));
    let (code, _, err) = run_host(&["run", "-p", p(&proj), "--hello-timeout-ms", "200"]);
    assert_eq!(code, 2);
    assert!(err.contains("no hello within 200 ms"), "{err}");
}

#[test]
fn run_full_life_cycle_ends_with_exit_code_0_when_the_adapter_closes_everything() {
    let dir = TempDir::new();
    let block = format!("launch: {{ exe: \"{}\" }}", echo_model_path());
    let proj = dir.file("project.yaml", &project_yaml(None, &block, LIMITS));

    let mut child = Command::new(HOST)
        .args([
            "run",
            "-p",
            p(&proj),
            "--sim-id",
            "7a11",
            "--hello-timeout-ms",
            "10000",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let _ = tx.send(line);
        }
    });
    let ready = rx
        .recv_timeout(Duration::from_secs(20))
        .expect("READY line");
    assert_eq!(ready, "READY sim_id=0000000000007a11");

    let run_json = dir.0.join(".janus").join("run.json");
    assert!(
        run_json.is_file(),
        "run.json must exist once the host is ready"
    );
    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&run_json).unwrap()).unwrap();
    assert_eq!(doc["sim_id"], "0000000000007a11");

    // Act as the adapter: shut the simulation down.
    let seg = Segment::open(&names(0x7a11, 1).segment).unwrap();
    seg.header()
        .hs_state
        .store(HandshakeState::Closed.as_raw(), Ordering::Release);

    let status = {
        let end = std::time::Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(s) = child.try_wait().unwrap() {
                break s;
            }
            if std::time::Instant::now() > end {
                let _ = child.kill();
                panic!("host did not exit after every instance was closed");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    assert!(status.success(), "{status:?}");
    assert!(!run_json.exists(), "run.json must be removed on exit");
}
