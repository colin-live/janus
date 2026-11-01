//! `mock-webots` — a fake Webots master (adapter side) for closed-loop tests.
//!
//! See `docs/blocks/B6-mock-webots.md`.

use std::env;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use coupling_core::manifest::Manifest;
use coupling_core::types::Direction;
use mock_webots::master::{validity_name, MasterChannel};
use mock_webots::plant::RotaryJoint;

const USAGE: &str = "\
usage: mock-webots --sim-id <hex> --instance-id <n> [options]

options:
  --steps N              number of steps (default 5000)
  --inertia J            rotary inertia (default 0.01)
  --damping B            viscous damping (default 0.001)
  --csv PATH             write step,t,theta,omega,torque,status
  --reset-at K           perform one epoch reset before step K (1-based)
  --stop-after-error     stop as soon as the adapter enters Error
  --attach-timeout-ms N  how long to wait for the host (default 10000)
  -h, --help             show this help
";

#[derive(Debug)]
struct Args {
    sim_id: u64,
    instance_id: u32,
    steps: u64,
    inertia: f64,
    damping: f64,
    csv: Option<PathBuf>,
    reset_at: Option<u64>,
    stop_after_error: bool,
    attach_timeout: Duration,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("mock-webots: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let args = parse_args(env::args().skip(1))?;
    let mut channel = MasterChannel::attach(args.sim_id, args.instance_id, args.attach_timeout)
        .map_err(|e| e.to_string())?;

    let manifest = channel.manifest().clone();
    let dt_ns = manifest.dt_ns as i64;
    if dt_ns <= 0 {
        return Err(format!("manifest dt_ns must be positive, got {dt_ns}"));
    }
    let dt = dt_ns as f64 / 1e9;

    let position_slot = find_slot(&manifest, "joint_position", Direction::ToModel, "f64")?;
    let velocity_slot = find_slot(&manifest, "joint_velocity", Direction::ToModel, "f64")?;
    let torque_slot = find_slot(&manifest, "joint_torque", Direction::ToWebots, "f64")?;

    let mut plant = RotaryJoint::new(args.inertia, args.damping);
    let mut csv = match &args.csv {
        Some(path) => {
            let file =
                File::create(path).map_err(|e| format!("cannot create {}: {e}", path.display()))?;
            let mut writer = BufWriter::new(file);
            writeln!(writer, "step,t,theta,omega,torque,status")
                .map_err(|e| format!("cannot write CSV header: {e}"))?;
            Some(writer)
        }
        None => None,
    };

    let mut did_reset = false;
    for step in 1..=args.steps {
        if !did_reset && args.reset_at == Some(step) {
            channel
                .reset(Duration::from_secs(10))
                .map_err(|e| format!("reset at step {step} failed: {e}"))?;
            plant = RotaryJoint::new(args.inertia, args.damping);
            did_reset = true;
            eprintln!("[mock-webots] reset before step {step}: plant back to initial state");
        }

        let sim_time_ns = (step as i128 - 1)
            .saturating_mul(dt_ns as i128)
            .clamp(i64::MIN as i128, i64::MAX as i128) as i64;

        channel.set_state_f64(position_slot, plant.theta);
        channel.set_state_f64(velocity_slot, plant.omega);

        let result = channel.exchange(sim_time_ns, dt_ns);

        let torque_out = result
            .efforts
            .iter()
            .find(|effort| effort.slot_id == torque_slot);
        let (torque, status) = match torque_out {
            Some(out) => (
                out.value.as_scalar().unwrap_or(0.0),
                validity_name(out.status),
            ),
            None => (0.0, "error"),
        };
        let applied = if torque.is_finite() { torque } else { 0.0 };
        plant.step(applied, dt);

        if let Some(writer) = csv.as_mut() {
            let t = sim_time_ns as f64 / 1e9;
            writeln!(
                writer,
                "{step},{t},{},{},{applied},{status}",
                plant.theta, plant.omega
            )
            .map_err(|e| format!("cannot write CSV row: {e}"))?;
        }

        if channel.hs_state() == coupling_core::HandshakeState::Error && args.stop_after_error {
            eprintln!("[mock-webots] stopping after Error at step {step}");
            break;
        }
    }

    if let Some(mut writer) = csv {
        writer
            .flush()
            .map_err(|e| format!("cannot flush CSV: {e}"))?;
    }
    channel.set_closed();
    eprintln!(
        "[mock-webots] finished {} step(s), instance {} closed",
        args.steps, args.instance_id
    );
    Ok(())
}

fn parse_args<I>(mut args: I) -> Result<Args, String>
where
    I: Iterator<Item = String>,
{
    let mut sim_id = None;
    let mut instance_id = None;
    let mut steps = 5000u64;
    let mut inertia = 0.01f64;
    let mut damping = 0.001f64;
    let mut csv = None;
    let mut reset_at = None;
    let mut stop_after_error = false;
    let mut attach_timeout_ms = 10_000u64;

    while let Some(arg) = args.next() {
        let mut value = |name: &str| -> Result<String, String> {
            args.next()
                .ok_or_else(|| format!("{name} requires a value"))
        };
        match arg.as_str() {
            "--sim-id" => sim_id = Some(parse_hex(&value("--sim-id")?)?),
            "--instance-id" => {
                instance_id = Some(
                    value("--instance-id")?
                        .parse::<u32>()
                        .map_err(|e| format!("invalid --instance-id: {e}"))?,
                )
            }
            "--steps" => {
                steps = value("--steps")?
                    .parse::<u64>()
                    .map_err(|e| format!("invalid --steps: {e}"))?
            }
            "--inertia" => {
                inertia = value("--inertia")?
                    .parse::<f64>()
                    .map_err(|e| format!("invalid --inertia: {e}"))?
            }
            "--damping" => {
                damping = value("--damping")?
                    .parse::<f64>()
                    .map_err(|e| format!("invalid --damping: {e}"))?
            }
            "--csv" => csv = Some(PathBuf::from(value("--csv")?)),
            "--reset-at" => {
                reset_at = Some(
                    value("--reset-at")?
                        .parse::<u64>()
                        .map_err(|e| format!("invalid --reset-at: {e}"))?,
                )
            }
            "--stop-after-error" => stop_after_error = true,
            "--attach-timeout-ms" => {
                attach_timeout_ms = value("--attach-timeout-ms")?
                    .parse::<u64>()
                    .map_err(|e| format!("invalid --attach-timeout-ms: {e}"))?
            }
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument {other:?}\n\n{USAGE}")),
        }
    }

    if steps == 0 {
        return Err("--steps must be positive".to_string());
    }
    if !inertia.is_finite() || inertia <= 0.0 {
        return Err("--inertia must be a positive finite number".to_string());
    }
    if !damping.is_finite() || damping < 0.0 {
        return Err("--damping must be a non-negative finite number".to_string());
    }

    Ok(Args {
        sim_id: sim_id.ok_or("missing --sim-id")?,
        instance_id: instance_id.ok_or("missing --instance-id")?,
        steps,
        inertia,
        damping,
        csv,
        reset_at,
        stop_after_error,
        attach_timeout: Duration::from_millis(attach_timeout_ms.max(1)),
    })
}

fn parse_hex(text: &str) -> Result<u64, String> {
    let trimmed = text.trim_start_matches("0x").trim_start_matches("0X");
    u64::from_str_radix(trimmed, 16).map_err(|e| format!("invalid --sim-id {text:?}: {e}"))
}

fn find_slot(
    manifest: &Manifest,
    name: &str,
    direction: Direction,
    value_type: &str,
) -> Result<u32, String> {
    let entry = manifest
        .slots
        .iter()
        .find(|slot| slot.name == name && slot.enabled)
        .ok_or_else(|| format!("manifest has no enabled slot named {name:?}"))?;
    let expected_direction = match direction {
        Direction::ToModel => "to_model",
        Direction::ToWebots => "to_webots",
    };
    if entry.direction != expected_direction {
        return Err(format!(
            "slot {name:?} has direction {}, expected {expected_direction}",
            entry.direction
        ));
    }
    if entry.value_type != value_type {
        return Err(format!(
            "slot {name:?} has value_type {}, expected {value_type}",
            entry.value_type
        ));
    }
    Ok(entry.slot_id)
}
