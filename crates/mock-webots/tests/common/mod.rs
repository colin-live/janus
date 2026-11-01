//! Shared helpers for the B6 integration tests: a minimal fake host plus fake
//! model threads that speak protocol §7 directly through B2.

#![allow(dead_code)]

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use coupling_core::layout::{Layout, SlotLayout};
use coupling_core::manifest::{EffortPolicy, Entity, Limits, Manifest, SlotEntry};
use coupling_core::types::{Direction, HandshakeState, Validity, ValueType};
use coupling_shm::{names, JsonArea, Notifier, Segment, SegmentNames, WaitResult};

static COUNTER: AtomicU64 = AtomicU64::new(0);

pub fn unique_sim_id() -> u64 {
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64;
    let pid = std::process::id() as u64;
    nanos.rotate_left(17).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (pid << 32) ^ seq
}

#[allow(clippy::too_many_arguments)]
pub fn slot(
    slot_id: u32,
    signal_id: u32,
    name: &str,
    value_type: ValueType,
    direction: Direction,
    unit: &str,
    required: bool,
    limits: Option<Limits>,
) -> SlotEntry {
    SlotEntry {
        slot_id,
        entity_id: 1,
        signal_id,
        name: name.to_string(),
        value_type: value_type.name().to_string(),
        direction: match direction {
            Direction::ToModel => "to_model",
            Direction::ToWebots => "to_webots",
        }
        .to_string(),
        frame: match direction {
            Direction::ToModel => "none",
            Direction::ToWebots => "joint_axis",
        }
        .to_string(),
        unit: unit.to_string(),
        required,
        enabled: true,
        limits,
        point: None,
    }
}

pub fn limits(max_abs: f64, max_rate_per_s: Option<f64>) -> Limits {
    Limits {
        max_abs,
        max_rate_per_s,
    }
}

pub fn manifest(
    sim_id: u64,
    instance_id: u32,
    dt_ns: u64,
    model_timeout_ms: u32,
    max_consecutive_misses: u32,
    slots: Vec<SlotEntry>,
) -> Manifest {
    Manifest {
        manifest_version: 1,
        simulation_id: format!("0x{sim_id:016x}"),
        instance_id,
        instance_name: "test".to_string(),
        sync_mode: "lockstep".to_string(),
        dt_ns,
        model_timeout_ms,
        effort_policy: EffortPolicy {
            ttl_steps: 1,
            on_timeout: "zero".to_string(),
            max_consecutive_misses,
        },
        world_frame: "ENU".to_string(),
        entities: vec![Entity {
            entity_id: 1,
            name: "joint".to_string(),
            webots_def: "JOINT".to_string(),
            kind: "joint".to_string(),
            motor: None,
            sensor: None,
        }],
        slots,
    }
}

/// A fake host: owns the creating handles and exposes the shared header for
/// assertions. It stays alive for the whole test; the creator `Drop` cleans up the
/// kernel objects.
pub struct Host {
    pub sim_id: u64,
    pub instance_id: u32,
    pub names: SegmentNames,
    pub segment: Segment,
    pub layout: Layout,
    pub manifest: Manifest,
    pub s2m: Notifier,
    pub m2s: Notifier,
}

impl Host {
    pub fn new(manifest: Manifest) -> Host {
        let sim_id = manifest.simulation_id_u64().expect("sim id");
        let instance_id = manifest.instance_id;
        let specs = manifest.slot_specs().expect("slot specs");
        let layout = Layout::build(&specs, 64 * 1024, 16 * 1024, 16 * 1024).expect("layout");
        let names = names(sim_id, instance_id);
        let segment =
            Segment::create(&names.segment, layout.total_size as usize).expect("create segment");
        segment
            .init_from_layout(&layout, sim_id, instance_id, manifest.model_timeout_ms)
            .expect("init segment");
        let s2m = Notifier::create(&names.s2m).expect("s2m");
        let m2s = Notifier::create(&names.m2s).expect("m2s");
        let json = serde_json::to_vec(&manifest).expect("manifest json");
        segment
            .write_json(JsonArea::Manifest, &json)
            .expect("write manifest");
        segment
            .header()
            .hs_state
            .store(HandshakeState::Ready.as_raw(), Ordering::Release);
        Host {
            sim_id,
            instance_id,
            names,
            segment,
            layout,
            manifest,
            s2m,
            m2s,
        }
    }

    pub fn slot(&self, name: &str) -> &SlotLayout {
        let id = self
            .manifest
            .slot_by_name(name)
            .unwrap_or_else(|| panic!("manifest has no slot {name}"))
            .slot_id;
        self.layout.slot(id).expect("layout slot")
    }

    pub fn slot_id(&self, name: &str) -> u32 {
        self.manifest
            .slot_by_name(name)
            .unwrap_or_else(|| panic!("manifest has no slot {name}"))
            .slot_id
    }

    pub fn state(&self) -> HandshakeState {
        HandshakeState::from_raw(self.segment.header().hs_state.load(Ordering::Acquire))
            .unwrap_or(HandshakeState::Error)
    }

    pub fn epoch(&self) -> u64 {
        self.segment.header().epoch.load(Ordering::Acquire)
    }

    pub fn step_id(&self) -> u64 {
        self.segment.header().step_id.load(Ordering::Acquire)
    }

    pub fn effort_step_id(&self) -> u64 {
        self.segment.header().effort_step_id.load(Ordering::Acquire)
    }

    pub fn model_epoch_ack(&self) -> u64 {
        self.segment
            .header()
            .model_epoch_ack
            .load(Ordering::Acquire)
    }

    pub fn validity(&self, name: &str) -> Validity {
        self.segment.read_validity(self.slot(name).validity_offset)
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        self.segment
            .header()
            .hs_state
            .store(HandshakeState::Closed.as_raw(), Ordering::Release);
    }
}

// ---------------------------------------------------------------- fake models

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ModelEffort {
    Scalar(f64),
    Vec3([f64; 3]),
}

#[derive(Debug, Clone, Copy)]
pub struct ScriptStep {
    pub effort: Option<ModelEffort>,
    pub validity: Validity,
    pub effort_step_id_ok: bool,
    pub respond: bool,
}

impl Default for ScriptStep {
    fn default() -> Self {
        Self {
            effort: Some(ModelEffort::Scalar(0.0)),
            validity: Validity::Valid,
            effort_step_id_ok: true,
            respond: true,
        }
    }
}

impl ScriptStep {
    pub fn scalar(v: f64) -> Self {
        Self {
            effort: Some(ModelEffort::Scalar(v)),
            ..Self::default()
        }
    }

    pub fn vec3(v: [f64; 3]) -> Self {
        Self {
            effort: Some(ModelEffort::Vec3(v)),
            ..Self::default()
        }
    }
}

pub struct ModelHandle {
    stop: Arc<AtomicBool>,
    seen: Arc<Mutex<Vec<u64>>>,
    s2m_name: String,
    join: Option<JoinHandle<()>>,
}

impl ModelHandle {
    pub fn seen_steps(&self) -> Vec<u64> {
        self.seen.lock().unwrap().clone()
    }
}

impl Drop for ModelHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Ok(notifier) = Notifier::open(&self.s2m_name) {
            notifier.signal();
        }
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn write_effort(segment: &Segment, off: u64, value_type: ValueType, effort: ModelEffort) {
    match (value_type, effort) {
        (ValueType::F64, ModelEffort::Scalar(v)) => segment.write_f64(off, v),
        (ValueType::Vec3F64, ModelEffort::Vec3(v)) => {
            for (i, x) in v.iter().enumerate() {
                segment.write_f64(off + 8 * i as u64, *x);
            }
        }
        (ValueType::Vec3F64, ModelEffort::Scalar(v)) => {
            for i in 0..3 {
                segment.write_f64(off + 8 * i, v);
            }
        }
        (ValueType::F64, ModelEffort::Vec3(v)) => segment.write_f64(off, v[0]),
        (other, _) => panic!("unsupported test effort type {}", other.name()),
    }
}

/// Spawn a model thread that follows a scripted response for each `s2m` step.
pub fn spawn_scripted_model(
    host: &Host,
    effort_slot: &str,
    scripts: Vec<ScriptStep>,
) -> ModelHandle {
    let names = host.names.clone();
    let slot = host.slot(effort_slot).clone();
    let stop = Arc::new(AtomicBool::new(false));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let stop_thread = Arc::clone(&stop);
    let seen_thread = Arc::clone(&seen);
    let s2m_name = names.s2m.clone();

    let join = thread::spawn(move || {
        let segment = Segment::open(&names.segment).expect("model opens segment");
        let s2m = Notifier::open(&names.s2m).expect("model opens s2m");
        let m2s = Notifier::open(&names.m2s).expect("model opens m2s");
        let mut last = 0u64;

        for script in scripts {
            let n = loop {
                if stop_thread.load(Ordering::Acquire) {
                    return;
                }
                match s2m.wait(Duration::from_millis(20), Duration::from_micros(50)) {
                    WaitResult::Signaled => {}
                    WaitResult::TimedOut => continue,
                }
                let n = segment.header().step_id.load(Ordering::Acquire);
                if n != 0 && n != last {
                    break n;
                }
            };
            last = n;
            seen_thread.lock().unwrap().push(n);

            if !script.respond {
                continue;
            }
            if let Some(effort) = script.effort {
                write_effort(&segment, slot.value_offset, slot.spec.value_type, effort);
            }
            segment.write_validity(slot.validity_offset, script.validity);
            let effort_step_id = if script.effort_step_id_ok {
                n
            } else {
                n.saturating_sub(1)
            };
            segment
                .header()
                .effort_step_id
                .store(effort_step_id, Ordering::Release);
            segment
                .header()
                .model_heartbeat
                .fetch_add(1, Ordering::Release);
            m2s.signal();
        }
    });

    ModelHandle {
        stop,
        seen,
        s2m_name,
        join: Some(join),
    }
}

/// Spawn a model that responds with a constant torque on every step and handles
/// epoch resets (optionally delaying the ack so a test can observe `Resetting`).
pub fn spawn_echo_model(
    host: &Host,
    effort_slot: &str,
    torque: f64,
    ack_delay: Duration,
) -> ModelHandle {
    let names = host.names.clone();
    let slot = host.slot(effort_slot).clone();
    let stop = Arc::new(AtomicBool::new(false));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let stop_thread = Arc::clone(&stop);
    let seen_thread = Arc::clone(&seen);
    let s2m_name = names.s2m.clone();

    let join = thread::spawn(move || {
        let segment = Segment::open(&names.segment).expect("model opens segment");
        let s2m = Notifier::open(&names.s2m).expect("model opens s2m");
        let m2s = Notifier::open(&names.m2s).expect("model opens m2s");
        let mut my_epoch = segment.header().epoch.load(Ordering::Acquire);
        let mut last = 0u64;

        loop {
            if stop_thread.load(Ordering::Acquire) {
                return;
            }
            match s2m.wait(Duration::from_millis(20), Duration::from_micros(50)) {
                WaitResult::Signaled => {}
                WaitResult::TimedOut => continue,
            }

            let epoch = segment.header().epoch.load(Ordering::Acquire);
            if epoch != my_epoch {
                if !ack_delay.is_zero() {
                    thread::sleep(ack_delay);
                }
                segment
                    .header()
                    .model_epoch_ack
                    .store(epoch, Ordering::Release);
                my_epoch = epoch;
                last = 0;
                continue;
            }

            let n = segment.header().step_id.load(Ordering::Acquire);
            if n == 0 || n == last {
                continue;
            }
            last = n;
            seen_thread.lock().unwrap().push(n);

            match slot.spec.value_type {
                ValueType::F64 => segment.write_f64(slot.value_offset, torque),
                ValueType::Vec3F64 => {
                    for i in 0..3 {
                        segment.write_f64(slot.value_offset + 8 * i, torque);
                    }
                }
                other => panic!("unsupported test effort type {}", other.name()),
            }
            segment.write_validity(slot.validity_offset, Validity::Valid);
            segment.header().effort_step_id.store(n, Ordering::Release);
            segment
                .header()
                .model_heartbeat
                .fetch_add(1, Ordering::Release);
            m2s.signal();
        }
    });

    ModelHandle {
        stop,
        seen,
        s2m_name,
        join: Some(join),
    }
}
