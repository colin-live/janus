//! Minimal host + adapter used by the C ABI smoke test and the Python binding test.
//!
//! Usage: `mock_host <sim_id> <instance_id> <steps> [timeout_ms]`
//!
//! - `sim_id` accepts decimal or `0x` hex.
//! - Creates the segment with one `joint_position` (ToModel) and one `joint_torque`
//!   (ToWebots) slot, waits for the client's hello, sets `Ready`, then runs `steps`
//!   lockstep iterations and expects `torque == 2 * position`.
//! - Prints `HOST_READY sim_id=… instance=… steps=…` once the client may connect and
//!   `HOST_DONE` after the last step.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use coupling_core::manifest::{EffortPolicy, Entity, Manifest, SlotEntry};
use coupling_core::{Direction, FrameId, HandshakeState, Layout, Validity, ValueType};
use coupling_shm::{names, JsonArea, Notifier, Segment};

const DT_NS: i64 = 1_000_000;

fn main() {
    if let Err(message) = run() {
        eprintln!("mock_host: {message}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 3 {
        return Err("usage: mock_host <sim_id> <instance_id> <steps> [timeout_ms]".into());
    }
    let sim_id = parse_sim_id(&args[0])?;
    let instance_id: u32 = args[1].parse().map_err(|e| format!("instance_id: {e}"))?;
    let steps: u64 = args[2].parse().map_err(|e| format!("steps: {e}"))?;
    let timeout = Duration::from_millis(
        args.get(3)
            .map(|s| s.parse().map_err(|e| format!("timeout_ms: {e}")))
            .transpose()?
            .unwrap_or(10_000),
    );

    let manifest = manifest(sim_id, instance_id);
    let specs = manifest.slot_specs().map_err(|e| e.to_string())?;
    let layout =
        Layout::build(&specs, 64 * 1024, 16 * 1024, 16 * 1024).map_err(|e| e.to_string())?;
    let object_names = names(sim_id, instance_id);

    let seg = Segment::create(&object_names.segment, layout.total_size as usize)
        .map_err(|e| format!("create segment: {e}"))?;
    seg.init_from_layout(&layout, sim_id, instance_id, 2000)
        .map_err(|e| format!("init segment: {e}"))?;
    let manifest_json = serde_json::to_vec(&manifest).map_err(|e| e.to_string())?;
    seg.write_json(JsonArea::Manifest, &manifest_json)
        .map_err(|e| format!("write manifest: {e}"))?;
    let s2m = Notifier::create(&object_names.s2m).map_err(|e| format!("create s2m: {e}"))?;
    let m2s = Notifier::create(&object_names.m2s).map_err(|e| format!("create m2s: {e}"))?;

    // Handshake: wait for hello, then accept.
    let deadline = Instant::now() + timeout;
    loop {
        let state = seg.header().hs_state.load(Ordering::Acquire);
        if state == HandshakeState::Hello.as_raw() {
            seg.header()
                .hs_state
                .store(HandshakeState::Ready.as_raw(), Ordering::Release);
            break;
        }
        if state == HandshakeState::Error.as_raw() || state == HandshakeState::Closed.as_raw() {
            return Err("client aborted the handshake".into());
        }
        if Instant::now() >= deadline {
            return Err("timeout waiting for the client hello".into());
        }
        std::thread::sleep(Duration::from_millis(1));
    }

    println!("HOST_READY sim_id={sim_id} instance={instance_id} steps={steps}");

    let state_slot = layout.slot(0).ok_or("missing state slot")?;
    let effort_slot = layout.slot(1).ok_or("missing effort slot")?;

    for n in 1..=steps {
        let position = n as f64 * 0.5;
        seg.write_f64(state_slot.value_offset, position);
        seg.write_validity(state_slot.validity_offset, Validity::Valid);
        let header = seg.header();
        header
            .sim_time_ns
            .store(n as i64 * DT_NS, Ordering::Relaxed);
        header.dt_ns.store(DT_NS, Ordering::Relaxed);
        header.adapter_heartbeat.fetch_add(1, Ordering::Relaxed);
        header.step_id.store(n, Ordering::Release);
        s2m.signal();

        let deadline = Instant::now() + timeout;
        loop {
            if header.effort_step_id.load(Ordering::Acquire) == n {
                break;
            }
            if Instant::now() >= deadline {
                return Err(format!("timeout waiting for effort of step {n}"));
            }
            let _ = m2s.wait(Duration::from_millis(5), Duration::from_micros(50));
        }
        let validity = seg.read_validity(effort_slot.validity_offset);
        let torque = seg.read_f64(effort_slot.value_offset);
        if validity != Validity::Valid {
            return Err(format!("effort of step {n} has validity {validity:?}"));
        }
        let expected = position * 2.0;
        if (torque - expected).abs() > 1e-9 {
            return Err(format!(
                "step {n}: expected torque {expected}, got {torque}"
            ));
        }
    }

    seg.header()
        .hs_state
        .store(HandshakeState::Closed.as_raw(), Ordering::Release);
    s2m.signal();
    println!("HOST_DONE");
    Ok(())
}

fn parse_sim_id(s: &str) -> Result<u64, String> {
    match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(hex) => u64::from_str_radix(hex, 16).map_err(|e| format!("sim_id: {e}")),
        None => s.parse().map_err(|e| format!("sim_id: {e}")),
    }
}

fn manifest(sim_id: u64, instance_id: u32) -> Manifest {
    let slot = |slot_id: u32,
                signal_id: u32,
                name: &str,
                value_type: ValueType,
                direction: Direction,
                frame: FrameId,
                unit: &str| SlotEntry {
        slot_id,
        entity_id: 1,
        signal_id,
        name: name.into(),
        value_type: value_type.name().into(),
        direction: if direction == Direction::ToModel {
            "to_model"
        } else {
            "to_webots"
        }
        .into(),
        frame: match frame {
            FrameId::None => "none",
            FrameId::World => "world",
            FrameId::BodyLocal => "body_local",
            FrameId::JointAxis => "joint_axis",
            FrameId::ModelLocal => "model_local",
        }
        .into(),
        unit: unit.into(),
        required: true,
        enabled: true,
        limits: None,
        point: None,
    };

    Manifest {
        manifest_version: 1,
        simulation_id: format!("0x{sim_id:016x}"),
        instance_id,
        instance_name: "mock_instance".into(),
        sync_mode: "lockstep".into(),
        dt_ns: DT_NS as u64,
        model_timeout_ms: 2000,
        effort_policy: EffortPolicy::default(),
        world_frame: "ENU".into(),
        entities: vec![Entity {
            entity_id: 1,
            name: "joint".into(),
            webots_def: "MOCK_JOINT".into(),
            kind: "joint".into(),
            motor: None,
            sensor: None,
        }],
        slots: vec![
            slot(
                0,
                0x0001,
                "joint_position",
                ValueType::F64,
                Direction::ToModel,
                FrameId::None,
                "rad",
            ),
            slot(
                1,
                0x0101,
                "joint_torque",
                ValueType::F64,
                Direction::ToWebots,
                FrameId::JointAxis,
                "N*m",
            ),
        ],
    }
}
