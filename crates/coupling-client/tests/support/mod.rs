//! Shared helpers for the B4 integration tests: a minimal in-process "host + adapter".

#![allow(dead_code)]

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use coupling_core::header::SegmentHeader;
use coupling_core::manifest::{EffortPolicy, Entity, Hello, Manifest, SlotEntry};
use coupling_core::{Direction, FrameId, HandshakeState, Layout, SlotLayout, Validity, ValueType};
use coupling_shm::{names, JsonArea, Notifier, Segment};

pub const DT_NS: i64 = 1_000_000;
pub const TIMEOUT: Duration = Duration::from_secs(10);

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Fresh `sim_id` so parallel test binaries never share kernel objects.
pub fn unique_sim_id() -> u64 {
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos() as u64;
    let pid = u64::from(std::process::id());
    nanos.rotate_left(17).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (pid << 24) ^ seq
}

/// Hello used by the tests and the smoke examples.
pub fn hello() -> Hello {
    Hello {
        protocol: [1, 0],
        model_name: "b4-test".into(),
        model_version: "0.1.0".into(),
        steppable: true,
        sync_modes: vec!["lockstep".into()],
        internal_dt_ns: Some(100_000),
        supported_signals: vec![0x0001, 0x0101],
        custom_signals: vec![],
    }
}

pub fn slot(
    slot_id: u32,
    signal_id: u32,
    name: &str,
    value_type: ValueType,
    direction: Direction,
    frame: FrameId,
    unit: &str,
) -> SlotEntry {
    SlotEntry {
        slot_id,
        entity_id: 1,
        signal_id,
        name: name.into(),
        value_type: value_type.name().into(),
        direction: if direction == Direction::ToModel {
            "to_model".into()
        } else {
            "to_webots".into()
        },
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
    }
}

/// Usual motor binding: `joint_position` in, `joint_torque` out.
pub fn motor_slots() -> Vec<SlotEntry> {
    vec![
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
    ]
}

pub fn manifest(sim_id: u64, instance_id: u32, slots: Vec<SlotEntry>) -> Manifest {
    Manifest {
        manifest_version: 1,
        simulation_id: format!("0x{sim_id:016x}"),
        instance_id,
        instance_name: "test_instance".into(),
        sync_mode: "lockstep".into(),
        dt_ns: DT_NS as u64,
        model_timeout_ms: 2000,
        effort_policy: EffortPolicy::default(),
        world_frame: "ENU".into(),
        entities: vec![Entity {
            entity_id: 1,
            name: "joint".into(),
            webots_def: "TEST_JOINT".into(),
            kind: "joint".into(),
            motor: None,
            sensor: None,
        }],
        slots,
    }
}

/// Host + adapter side of one segment, used to drive the client from a test.
pub struct TestHost {
    pub sim_id: u64,
    pub instance_id: u32,
    pub seg: Segment,
    pub s2m: Notifier,
    pub m2s: Notifier,
    pub layout: Layout,
    pub manifest: Manifest,
    pub state: SlotLayout,
    pub effort: SlotLayout,
}

impl TestHost {
    pub fn new(sim_id: u64, instance_id: u32, slots: Vec<SlotEntry>) -> TestHost {
        let manifest = manifest(sim_id, instance_id, slots);
        let specs = manifest.slot_specs().expect("slot specs");
        let layout = Layout::build(&specs, 64 * 1024, 16 * 1024, 16 * 1024).expect("layout");
        let names = names(sim_id, instance_id);
        let seg = Segment::create(&names.segment, layout.total_size as usize).expect("create");
        seg.init_from_layout(&layout, sim_id, instance_id, 2000)
            .expect("init");
        let manifest_json = serde_json::to_vec(&manifest).expect("manifest json");
        seg.write_json(JsonArea::Manifest, &manifest_json)
            .expect("manifest");
        let s2m = Notifier::create(&names.s2m).expect("s2m");
        let m2s = Notifier::create(&names.m2s).expect("m2s");
        let state = layout.slot(0).expect("state slot").clone();
        let effort = layout.slot(1).expect("effort slot").clone();
        TestHost {
            sim_id,
            instance_id,
            seg,
            s2m,
            m2s,
            layout,
            manifest,
            state,
            effort,
        }
    }

    pub fn motor(sim_id: u64, instance_id: u32) -> TestHost {
        Self::new(sim_id, instance_id, motor_slots())
    }

    pub fn header(&self) -> &SegmentHeader {
        self.seg.header()
    }

    /// Wait until the client wrote its hello (polling; the host has no event for this).
    pub fn wait_hello(&self, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        loop {
            if self.header().hs_state.load(Ordering::Acquire) == HandshakeState::Hello.as_raw() {
                return;
            }
            assert!(Instant::now() < deadline, "client never wrote hello");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    pub fn set_ready(&self) {
        self.header()
            .hs_state
            .store(HandshakeState::Ready.as_raw(), Ordering::Release);
    }

    /// Adapter side of one step: state values + validity, timing, `step_id` last, wakeup.
    pub fn step(&self, n: u64, position: f64) {
        let header = self.header();
        self.seg.write_f64(self.state.value_offset, position);
        self.seg
            .write_validity(self.state.validity_offset, Validity::Valid);
        header
            .sim_time_ns
            .store(n as i64 * DT_NS, Ordering::Relaxed);
        header.dt_ns.store(DT_NS, Ordering::Relaxed);
        header.adapter_heartbeat.fetch_add(1, Ordering::Relaxed);
        header.step_id.store(n, Ordering::Release);
        self.s2m.signal();
    }

    /// Wait until the model published `effort_step_id == n`; returns `(validity, torque)`.
    pub fn await_effort(&self, n: u64, timeout: Duration) -> (Validity, f64) {
        let deadline = Instant::now() + timeout;
        loop {
            if self.header().effort_step_id.load(Ordering::Acquire) == n {
                return (
                    self.seg.read_validity(self.effort.validity_offset),
                    self.seg.read_f64(self.effort.value_offset),
                );
            }
            assert!(
                Instant::now() < deadline,
                "model did not publish effort for step {n}"
            );
            let _ = self
                .m2s
                .wait(Duration::from_millis(5), Duration::from_micros(50));
        }
    }

    /// Adapter reset: new epoch, cleared step ids and validities, wake the model (protocol §7).
    pub fn begin_reset(&self) -> u64 {
        let header = self.header();
        let epoch = header.epoch.fetch_add(1, Ordering::AcqRel) + 1;
        header.step_id.store(0, Ordering::Release);
        header.effort_step_id.store(0, Ordering::Release);
        header.sim_time_ns.store(0, Ordering::Relaxed);
        header.dt_ns.store(0, Ordering::Relaxed);
        self.seg
            .write_validity(self.state.validity_offset, Validity::Missing);
        self.seg
            .write_validity(self.effort.validity_offset, Validity::Missing);
        self.s2m.signal();
        epoch
    }

    pub fn wait_ack(&self, epoch: u64, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        loop {
            if self.header().model_epoch_ack.load(Ordering::Acquire) == epoch {
                return;
            }
            assert!(Instant::now() < deadline, "model never acked epoch {epoch}");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    pub fn close(&self) {
        self.header()
            .hs_state
            .store(HandshakeState::Closed.as_raw(), Ordering::Release);
        self.s2m.signal();
    }
}
