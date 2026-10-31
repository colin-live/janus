//! `ModelClient`: the model-side lockstep state machine (protocol §7 "model").
//!
//! Workflow: [`ModelClient::connect`] → loop { [`ModelClient::wait_step`] → read state →
//! write effort → [`ModelClient::commit`] }. A reset is delivered as a separate
//! [`WaitEvent::Reset`] event (it carries no step): the model reinitialises its internal
//! state and calls [`ModelClient::ack_reset`] before waiting for the first step of the new
//! epoch.
//!
//! Ownership (protocol §5): the model writes `model_epoch_ack`, `effort_step_id`,
//! `model_heartbeat`, the hello area and the ToWebots region; it never touches the
//! adapter-owned header fields. The client only acts as a consequence of a step
//! notification, so the adapter's `Release` publication is matched by `Acquire` loads.

use std::cell::Cell;
use std::marker::PhantomData;
use std::sync::atomic::Ordering;
use std::thread::sleep;
use std::time::{Duration, Instant};

use coupling_core::header::HEADER_SIZE;
use coupling_core::manifest::{Hello, Manifest};
use coupling_core::{CoreError, Direction, HandshakeState, Layout, SyncMode, Validity, ValueType};
use coupling_shm::{names, JsonArea, Notifier, Segment, SegmentNames, ShmError, WaitResult};

use crate::ClientError;

/// Poll interval while waiting for the host's `Ready` state.
const HANDSHAKE_POLL: Duration = Duration::from_millis(1);
/// Retry interval while the host is still creating / initialising the segment.
const OPEN_RETRY: Duration = Duration::from_millis(1);
/// Lock-free spin inside one `Notifier::wait` before the thread parks.
const WAIT_SPIN: Duration = Duration::from_micros(100);
/// `wait_step` re-checks the lifecycle state at least this often, so a closed or
/// failed segment is noticed even when no wakeup event arrives.
const STATE_POLL: Duration = Duration::from_millis(50);

/// Opaque handle for one slot, resolved to absolute offsets during `connect`.
///
/// Handles are only valid for the `ModelClient` that produced them; they are indices into
/// an internal table, so lookups at run time are O(1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SlotHandle(u32);

impl SlotHandle {
    pub(crate) fn raw(self) -> u32 {
        self.0
    }

    pub(crate) fn from_raw(raw: u32) -> Self {
        Self(raw)
    }
}

/// Metadata of one simulation step delivered by [`ModelClient::wait_step`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Step {
    /// Adapter step counter (`1..`, restarts at 1 after an epoch change).
    pub step_id: u64,
    /// Simulated time of the state sample, in nanoseconds.
    pub sim_time_ns: i64,
    /// Length of the step the model must advance, in nanoseconds.
    pub dt_ns: i64,
    /// Epoch this step belongs to.
    pub epoch: u64,
}

/// Result of [`ModelClient::wait_step`]: a step, a reset notification, or a timeout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitEvent {
    /// A new step is available; the model must write its efforts and call
    /// [`ModelClient::commit`] exactly once.
    Step(Step),
    /// The adapter changed the epoch (reset / world reload). No step is carried: the model
    /// must reinitialise all internal state and call [`ModelClient::ack_reset`].
    Reset { epoch: u64 },
    /// The timeout expired without a new step or reset event.
    Timeout,
}

struct ClientSlot {
    slot_id: u32,
    direction: Direction,
    value_type: ValueType,
    validity_off: u64,
    value_off: u64,
}

/// A connected model instance.
///
/// `ModelClient` is deliberately not `Sync`: handshake and step accounting are single
/// threaded (B4 requirement 6). `Segment`/`Notifier` handles stay alive until drop.
pub struct ModelClient {
    seg: Segment,
    s2m: Notifier,
    m2s: Notifier,
    manifest: Manifest,
    slots: Vec<ClientSlot>,
    epoch: u64,
    last_step: u64,
    pending_commit: Option<u64>,
    /// Epoch observed in a [`WaitEvent::Reset`] that has not been acknowledged yet.
    ack_pending: Option<u64>,
    _not_sync: PhantomData<Cell<()>>,
}

struct Opened {
    seg: Segment,
    s2m: Notifier,
    m2s: Notifier,
    manifest: Manifest,
    layout: Layout,
}

enum OpenError {
    /// Host has not finished creating the segment / writing the manifest yet (spec §6.2).
    Retry,
    Fatal(ClientError),
}

impl ModelClient {
    /// Connect to the segment of `(sim_id, instance_id)`, write the hello and wait until the
    /// adapter accepts the model (`Ready`, `Running` or `Paused`).
    ///
    /// `NotFound` and `BadMagic` are retried until `timeout` expires (protocol §6.2), as is an
    /// empty manifest (the host publishes `magic` before writing the manifest, see
    /// `docs/blocks/B4-client.md` status). The same deadline also covers the handshake.
    pub fn connect(
        sim_id: u64,
        instance_id: u32,
        hello: Hello,
        timeout: Duration,
    ) -> Result<ModelClient, ClientError> {
        let deadline = Instant::now() + timeout;
        let names = names(sim_id, instance_id);

        let Opened {
            seg,
            s2m,
            m2s,
            manifest,
            layout,
        } = loop {
            match Self::open_once(&names, sim_id, instance_id) {
                Ok(opened) => break opened,
                Err(OpenError::Fatal(e)) => return Err(e),
                Err(OpenError::Retry) => {
                    if Instant::now() >= deadline {
                        return Err(ClientError::Timeout("open segment / read manifest"));
                    }
                    sleep(OPEN_RETRY);
                }
            }
        };

        let header = seg.header();
        let hello_json =
            serde_json::to_vec(&hello).map_err(|e| ClientError::Json(format!("hello: {e}")))?;
        seg.write_json(JsonArea::Hello, &hello_json)?;

        match header.hs_state.compare_exchange(
            HandshakeState::Created.as_raw(),
            HandshakeState::Hello.as_raw(),
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => {}
            Err(current) => {
                let state = HandshakeState::from_raw(current)
                    .map(|s| format!("{s:?}"))
                    .unwrap_or_else(|| format!("unknown({current})"));
                if current == HandshakeState::Error.as_raw() {
                    return Err(remote_error(&seg));
                }
                return Err(ClientError::BadState(format!(
                    "expected Created before hello, found {state}"
                )));
            }
        }

        loop {
            match state_of(&seg)? {
                HandshakeState::Ready | HandshakeState::Running | HandshakeState::Paused => break,
                HandshakeState::Error => return Err(remote_error(&seg)),
                HandshakeState::Closed => return Err(ClientError::Closed),
                _ => {}
            }
            if Instant::now() >= deadline {
                return Err(ClientError::Timeout("handshake: waiting for Ready"));
            }
            sleep(HANDSHAKE_POLL);
        }

        match SyncMode::from_raw(header.sync_mode.load(Ordering::Acquire)) {
            Some(SyncMode::Lockstep) => {}
            other => {
                return Err(ClientError::BadState(format!(
                    "host finalized sync_mode {other:?}, only lockstep is supported"
                )))
            }
        }
        if manifest.sync_mode != "lockstep" {
            return Err(ClientError::BadState(format!(
                "manifest sync_mode {:?} is not lockstep",
                manifest.sync_mode
            )));
        }

        let slots = layout
            .slots
            .iter()
            .map(|s| ClientSlot {
                slot_id: s.spec.slot_id,
                direction: s.spec.direction,
                value_type: s.spec.value_type,
                validity_off: s.validity_offset,
                value_off: s.value_offset,
            })
            .collect();
        let initial_epoch = header.epoch.load(Ordering::Acquire);

        Ok(ModelClient {
            seg,
            s2m,
            m2s,
            manifest,
            slots,
            epoch: initial_epoch,
            last_step: 0,
            pending_commit: None,
            ack_pending: None,
            _not_sync: PhantomData,
        })
    }

    /// Segment manifest as written by the host (typed, protocol §8).
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// Resolve a manifest slot name to an O(1) handle; `None` for unknown or `enabled=false` slots.
    pub fn slot(&self, name: &str) -> Option<SlotHandle> {
        let entry = self.manifest.slot_by_name(name)?;
        if !entry.enabled {
            return None;
        }
        self.slots
            .iter()
            .position(|s| s.slot_id == entry.slot_id)
            .map(|i| SlotHandle(i as u32))
    }

    /// Current handshake state of the segment, for model-side logging/observability.
    ///
    /// Unknown raw bytes are reported as [`HandshakeState::Error`]. Only
    /// [`HandshakeState::Closed`] should terminate the model; [`HandshakeState::Error`] is
    /// recoverable when the adapter resets (protocol §10).
    pub fn hs_state(&self) -> HandshakeState {
        HandshakeState::from_raw(self.seg.header().hs_state.load(Ordering::Acquire))
            .unwrap_or(HandshakeState::Error)
    }

    /// Wait for the next step or reset, dropping spurious wakeups (protocol §7).
    ///
    /// The epoch is checked *before* `step_id`: a reset wakeup carries no step
    /// (`step_id == 0`), so it is reported as [`WaitEvent::Reset`]. The model must
    /// reinitialise its internal state and call [`ModelClient::ack_reset`]; the adapter
    /// waits for that acknowledgement before it publishes the first step of the new epoch.
    /// Returns [`WaitEvent::Timeout`] when `timeout` expires.
    pub fn wait_step(&mut self, timeout: Duration) -> Result<WaitEvent, ClientError> {
        if self.pending_commit.is_some() {
            return Err(ClientError::StepNotCommitted);
        }
        let deadline = Instant::now() + timeout;
        let header = self.seg.header();

        loop {
            self.check_state()?;
            let now = Instant::now();
            if now >= deadline {
                return Ok(WaitEvent::Timeout);
            }
            let slice = (deadline - now).min(STATE_POLL);
            if self.s2m.wait(slice, WAIT_SPIN) == WaitResult::TimedOut {
                continue;
            }

            // Protocol §7: compare the epoch first; the reset wakeup does not carry a step.
            let epoch = header.epoch.load(Ordering::Acquire);
            if epoch != self.epoch {
                self.epoch = epoch;
                self.last_step = 0;
                self.ack_pending = Some(epoch);
                return Ok(WaitEvent::Reset { epoch });
            }

            let n = header.step_id.load(Ordering::Acquire);
            if n != 0 && n != self.last_step {
                self.last_step = n;
                self.pending_commit = Some(n);
                return Ok(WaitEvent::Step(Step {
                    step_id: n,
                    sim_time_ns: header.sim_time_ns.load(Ordering::Relaxed),
                    dt_ns: header.dt_ns.load(Ordering::Relaxed),
                    epoch: self.epoch,
                }));
            }
        }
    }

    /// Acknowledge a [`WaitEvent::Reset`] after the model has reinitialised its internal
    /// state: writes `model_epoch_ack = epoch` with release semantics.
    ///
    /// This is the infallible variant kept for the frozen signature: misuse (no pending reset)
    /// is reported through a debug assertion and is a no-op. Use [`ModelClient::try_ack_reset`]
    /// when the caller needs to detect misuse.
    pub fn ack_reset(&mut self) {
        if let Err(e) = self.try_ack_reset() {
            debug_assert!(false, "{e}");
        }
    }

    /// Fallible [`ModelClient::ack_reset`]: `NoPendingReset` when no reset was reported.
    pub fn try_ack_reset(&mut self) -> Result<(), ClientError> {
        let epoch = self.ack_pending.take().ok_or(ClientError::NoPendingReset)?;
        self.seg
            .header()
            .model_epoch_ack
            .store(epoch, Ordering::Release);
        Ok(())
    }

    /// Read a `f64` slot; `None` unless the slot is an F64 and its validity is `Valid`.
    pub fn read_f64(&self, h: SlotHandle) -> Option<f64> {
        let slot = self.slot_at(h)?;
        if slot.value_type != ValueType::F64 {
            return None;
        }
        if self.seg.read_validity(slot.validity_off) != Validity::Valid {
            return None;
        }
        Some(self.seg.read_f64(slot.value_off))
    }

    /// Read a `Vec3F64` slot; `None` unless the slot is a Vec3 and its validity is `Valid`.
    pub fn read_vec3(&self, h: SlotHandle) -> Option<[f64; 3]> {
        let slot = self.slot_at(h)?;
        if slot.value_type != ValueType::Vec3F64 {
            return None;
        }
        if self.seg.read_validity(slot.validity_off) != Validity::Valid {
            return None;
        }
        Some([
            self.seg.read_f64(slot.value_off),
            self.seg.read_f64(slot.value_off + 8),
            self.seg.read_f64(slot.value_off + 16),
        ])
    }

    /// Read a `QuatF64` slot; `None` unless the slot is a Quat and its validity is `Valid`.
    pub fn read_quat(&self, h: SlotHandle) -> Option<[f64; 4]> {
        let slot = self.slot_at(h)?;
        if slot.value_type != ValueType::QuatF64 {
            return None;
        }
        if self.seg.read_validity(slot.validity_off) != Validity::Valid {
            return None;
        }
        Some([
            self.seg.read_f64(slot.value_off),
            self.seg.read_f64(slot.value_off + 8),
            self.seg.read_f64(slot.value_off + 16),
            self.seg.read_f64(slot.value_off + 24),
        ])
    }

    /// Write an effort value. Non-finite values mark the slot `Invalid` and are not written
    /// (protocol §10). Writing a ToModel slot or a non-F64 slot is a programming error; this
    /// infallible wrapper debug-asserts and ignores it. Use [`ModelClient::try_write_f64`] to
    /// detect misuse.
    pub fn write_f64(&mut self, h: SlotHandle, v: f64) {
        if let Err(e) = self.try_write_f64(h, v) {
            debug_assert!(false, "{e}");
        }
    }

    /// Fallible [`ModelClient::write_f64`]: `WrongSlot` when `h` is not a ToWebots F64 slot.
    pub fn try_write_f64(&mut self, h: SlotHandle, v: f64) -> Result<(), ClientError> {
        let slot = self
            .writable(h, ValueType::F64)
            .ok_or_else(|| ClientError::WrongSlot(format!("write_f64 on slot {}", h.raw())))?;
        if !v.is_finite() {
            self.seg
                .write_validity(slot.validity_off, Validity::Invalid);
            return Ok(());
        }
        self.seg.write_f64(slot.value_off, v);
        self.seg.write_validity(slot.validity_off, Validity::Valid);
        Ok(())
    }

    /// Write a `Vec3F64` effort. Any non-finite component marks the slot `Invalid`. Writing a
    /// ToModel slot or a non-Vec3 slot debug-asserts and is ignored; see
    /// [`ModelClient::try_write_vec3`].
    pub fn write_vec3(&mut self, h: SlotHandle, v: [f64; 3]) {
        if let Err(e) = self.try_write_vec3(h, v) {
            debug_assert!(false, "{e}");
        }
    }

    /// Fallible [`ModelClient::write_vec3`]: `WrongSlot` when `h` is not a ToWebots Vec3 slot.
    pub fn try_write_vec3(&mut self, h: SlotHandle, v: [f64; 3]) -> Result<(), ClientError> {
        let slot = self
            .writable(h, ValueType::Vec3F64)
            .ok_or_else(|| ClientError::WrongSlot(format!("write_vec3 on slot {}", h.raw())))?;
        if !v.iter().all(|x| x.is_finite()) {
            self.seg
                .write_validity(slot.validity_off, Validity::Invalid);
            return Ok(());
        }
        self.seg.write_f64(slot.value_off, v[0]);
        self.seg.write_f64(slot.value_off + 8, v[1]);
        self.seg.write_f64(slot.value_off + 16, v[2]);
        self.seg.write_validity(slot.validity_off, Validity::Valid);
        Ok(())
    }

    /// Mark an effort slot with the given validity (e.g. [`Validity::Error`]). Misuse
    /// debug-asserts and is ignored; see [`ModelClient::try_invalidate`].
    pub fn invalidate(&mut self, h: SlotHandle, why: Validity) {
        if let Err(e) = self.try_invalidate(h, why) {
            debug_assert!(false, "{e}");
        }
    }

    /// Fallible [`ModelClient::invalidate`]: `WrongSlot` when `h` is not a ToWebots slot.
    pub fn try_invalidate(&mut self, h: SlotHandle, why: Validity) -> Result<(), ClientError> {
        let slot = self
            .effort(h)
            .ok_or_else(|| ClientError::WrongSlot(format!("invalidate on slot {}", h.raw())))?;
        self.seg.write_validity(slot.validity_off, why);
        Ok(())
    }

    /// Publish the effort of the pending step: heartbeat, `effort_step_id` (release), then the
    /// m2s wakeup. Exactly one commit is allowed per [`WaitEvent::Step`]; a reset event is
    /// acknowledged with [`ModelClient::ack_reset`] instead.
    pub fn commit(&mut self) -> Result<(), ClientError> {
        let n = self.pending_commit.ok_or(ClientError::NoPendingStep)?;
        self.pending_commit = None;
        let header = self.seg.header();
        header.model_heartbeat.fetch_add(1, Ordering::Relaxed);
        header.effort_step_id.store(n, Ordering::Release);
        self.m2s.signal();
        Ok(())
    }

    fn open_once(names: &SegmentNames, sim_id: u64, instance_id: u32) -> Result<Opened, OpenError> {
        let seg = match Segment::open(&names.segment) {
            Ok(seg) => seg,
            Err(ShmError::NotFound) => return Err(OpenError::Retry),
            Err(e) => return Err(OpenError::Fatal(e.into())),
        };
        if seg.len() < HEADER_SIZE {
            return Err(OpenError::Retry);
        }
        let header = seg.header();
        match header.validate() {
            Ok(()) => {}
            Err(CoreError::BadMagic) => return Err(OpenError::Retry),
            Err(e) => return Err(OpenError::Fatal(e.into())),
        }

        if header.simulation_id != sim_id || header.instance_id != instance_id {
            return Err(fatal(ClientError::BadState(format!(
                "segment belongs to sim_id {:#x} instance {}",
                header.simulation_id, header.instance_id
            ))));
        }

        let s2m = match Notifier::open(&names.s2m) {
            Ok(n) => n,
            Err(ShmError::NotFound) => return Err(OpenError::Retry),
            Err(e) => return Err(OpenError::Fatal(e.into())),
        };
        let m2s = match Notifier::open(&names.m2s) {
            Ok(n) => n,
            Err(ShmError::NotFound) => return Err(OpenError::Retry),
            Err(e) => return Err(OpenError::Fatal(e.into())),
        };

        let manifest_bytes = match seg.read_json(JsonArea::Manifest) {
            Ok(bytes) => bytes,
            Err(e) => return Err(OpenError::Fatal(e.into())),
        };
        if manifest_bytes.is_empty() {
            // The host publishes `magic` in `init_from_layout` before writing the manifest;
            // an empty area means it has not finished, so retry like `BadMagic`.
            return Err(OpenError::Retry);
        }
        let manifest: Manifest = match serde_json::from_slice(&manifest_bytes) {
            Ok(m) => m,
            Err(e) => {
                return Err(fatal(ClientError::Json(format!("manifest: {e}"))));
            }
        };

        let manifest_sim = match manifest.simulation_id_u64() {
            Ok(id) => id,
            Err(e) => return Err(fatal(ClientError::Core(e))),
        };
        if manifest_sim != sim_id
            || header.simulation_id != manifest_sim
            || manifest.instance_id != instance_id
            || header.instance_id != manifest.instance_id
        {
            return Err(fatal(ClientError::BadState(
                "manifest ids disagree with the request or the header".into(),
            )));
        }

        let specs = match manifest.slot_specs() {
            Ok(specs) => specs,
            Err(e) => return Err(fatal(ClientError::Core(e))),
        };
        let layout = match Layout::from_header(&specs, header) {
            Ok(layout) => layout,
            Err(e) => return Err(fatal(ClientError::Core(e))),
        };

        let len = seg.len() as u64;
        if header.total_size > len {
            return Err(fatal(ClientError::BadState(format!(
                "header total_size {} exceeds mapping size {len}",
                header.total_size
            ))));
        }
        for slot in &layout.slots {
            let end = slot
                .value_offset
                .saturating_add(slot.spec.value_type.size() as u64);
            if slot.validity_offset >= len || end > len {
                return Err(fatal(ClientError::BadState(format!(
                    "slot {} out of segment bounds (validity {}, value end {end}, len {len})",
                    slot.spec.slot_id, slot.validity_offset
                ))));
            }
        }

        Ok(Opened {
            seg,
            s2m,
            m2s,
            manifest,
            layout,
        })
    }

    fn slot_at(&self, h: SlotHandle) -> Option<&ClientSlot> {
        self.slots.get(h.0 as usize)
    }

    fn writable(&self, h: SlotHandle, want: ValueType) -> Option<&ClientSlot> {
        let slot = self.slot_at(h)?;
        (slot.direction == Direction::ToWebots && slot.value_type == want).then_some(slot)
    }

    fn effort(&self, h: SlotHandle) -> Option<&ClientSlot> {
        let slot = self.slot_at(h)?;
        (slot.direction == Direction::ToWebots).then_some(slot)
    }

    fn check_state(&self) -> Result<HandshakeState, ClientError> {
        match state_of(&self.seg)? {
            // Only a clean shutdown terminates the model. `Error` is recoverable: the adapter
            // may reset and the model must stay alive to acknowledge the new epoch
            // (protocol §10).
            HandshakeState::Closed => Err(ClientError::Closed),
            state => Ok(state),
        }
    }
}

fn fatal(e: ClientError) -> OpenError {
    OpenError::Fatal(e)
}

fn state_of(seg: &Segment) -> Result<HandshakeState, ClientError> {
    let raw = seg.header().hs_state.load(Ordering::Acquire);
    HandshakeState::from_raw(raw)
        .ok_or_else(|| ClientError::BadState(format!("unknown hs_state value {raw}")))
}

fn remote_error(seg: &Segment) -> ClientError {
    match seg.read_json(JsonArea::Report) {
        Ok(report) if !report.is_empty() => {
            ClientError::Remote(String::from_utf8_lossy(&report).into_owned())
        }
        _ => ClientError::Remote("hs_state = Error (no report written)".into()),
    }
}
