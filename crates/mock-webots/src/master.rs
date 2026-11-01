//! Adapter-side reference implementation of protocol §7 (lockstep) and §10
//! (effort validation, limits, failure policy).
//!
//! [`MasterChannel`] owns one instance's segment and both wakeup events. It writes
//! the ToModel region, publishes `step_id` last, waits for the model, reads the
//! ToWebots region and applies the validation pipeline. The model is never trusted:
//! non-finite values, stale step ids and missing limits all become safe zeros.

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::{Duration, Instant};

use coupling_core::header::HEADER_SIZE;
use coupling_core::layout::{Layout, SlotLayout};
use coupling_core::manifest::{Limits, Manifest, Report, SlotEntry};
use coupling_core::types::{Direction, HandshakeState, Validity, ValueType};
use coupling_core::CoreError;
use coupling_shm::{names, JsonArea, Notifier, Segment, ShmError, WaitResult};

/// Spin phase passed to [`Notifier::wait`] on every exchange.
const SPIN: Duration = Duration::from_micros(50);

// ---------------------------------------------------------------- public result types

/// A validated effort value. Only the value types that can be efforts in v1 are
/// represented here; other types are rejected as an error.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EffortValue {
    Scalar(f64),
    Vec3([f64; 3]),
}

impl EffortValue {
    /// Zero value matching a slot's wire type (unknown types fall back to `Scalar`).
    pub fn zero_for(value_type: ValueType) -> Self {
        match value_type {
            ValueType::Vec3F64 => Self::Vec3([0.0; 3]),
            _ => Self::Scalar(0.0),
        }
    }

    pub fn as_scalar(self) -> Option<f64> {
        match self {
            Self::Scalar(v) => Some(v),
            Self::Vec3(_) => None,
        }
    }

    pub fn as_vec3(self) -> Option<[f64; 3]> {
        match self {
            Self::Scalar(_) => None,
            Self::Vec3(v) => Some(v),
        }
    }

    /// `true` when every component is finite.
    pub fn is_finite(self) -> bool {
        match self {
            Self::Scalar(v) => v.is_finite(),
            Self::Vec3(v) => v.iter().all(|x| x.is_finite()),
        }
    }

    fn zeroed(self) -> Self {
        match self {
            Self::Scalar(_) => Self::Scalar(0.0),
            Self::Vec3(_) => Self::Vec3([0.0; 3]),
        }
    }

    /// Clamp every component to `[-max_abs, max_abs]`. Returns the clamped value and
    /// whether anything changed.
    fn clamp(self, max_abs: f64) -> (Self, bool) {
        match self {
            Self::Scalar(v) => {
                let c = v.clamp(-max_abs, max_abs);
                (Self::Scalar(c), c != v)
            }
            Self::Vec3(v) => {
                let mut out = v;
                let mut changed = false;
                for x in &mut out {
                    let c = x.clamp(-max_abs, max_abs);
                    changed |= c != *x;
                    *x = c;
                }
                (Self::Vec3(out), changed)
            }
        }
    }

    /// Apply `max_delta` per component relative to `last` (missing/mismatched `last`
    /// is treated as zero).
    fn rate_limit(self, last: Self, max_delta: f64) -> Self {
        match (self, last) {
            (Self::Scalar(v), Self::Scalar(prev)) => Self::Scalar(limit_delta(v, prev, max_delta)),
            (Self::Vec3(v), Self::Vec3(prev)) => {
                let mut out = v;
                for (x, p) in out.iter_mut().zip(prev.iter()) {
                    *x = limit_delta(*x, *p, max_delta);
                }
                Self::Vec3(out)
            }
            (value, _) => value,
        }
    }
}

fn limit_delta(value: f64, previous: f64, max_delta: f64) -> f64 {
    let delta = value - previous;
    if delta > max_delta {
        previous + max_delta
    } else if delta < -max_delta {
        previous - max_delta
    } else {
        value
    }
}

/// One validated effort slot.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EffortOut {
    pub slot_id: u32,
    /// Value after validation, clamping and rate limiting. Always finite.
    pub value: EffortValue,
    /// `Valid` for a normal value, `OutOfRange` when it was clamped, or the reason
    /// the value was replaced by zero (`Invalid`, `Missing`, `Stale`, `Error`).
    pub status: Validity,
}

/// Result of one adapter step.
#[derive(Debug, Clone)]
pub struct ExchangeResult {
    /// Step id published to the model.
    pub step_id: u64,
    /// Enabled ToWebots slots, in manifest order.
    pub efforts: Vec<EffortOut>,
    /// The model did not produce a matching `effort_step_id` before the timeout.
    pub model_timed_out: bool,
    /// Human-readable reasons (also logged).
    pub errors: Vec<String>,
}

// ---------------------------------------------------------------- errors

#[derive(Debug)]
pub enum MasterError {
    Shm(ShmError),
    Core(CoreError),
    Json(serde_json::Error),
    Timeout(String),
    State {
        state: HandshakeState,
        detail: String,
    },
    Protocol(String),
    Slot(String),
}

impl fmt::Display for MasterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Shm(e) => write!(f, "shared memory: {e}"),
            Self::Core(e) => write!(f, "protocol: {e}"),
            Self::Json(e) => write!(f, "json: {e}"),
            Self::Timeout(what) => write!(f, "timeout: {what}"),
            Self::State { state, detail } => write!(f, "segment is {state:?}: {detail}"),
            Self::Protocol(why) => write!(f, "protocol error: {why}"),
            Self::Slot(why) => write!(f, "slot error: {why}"),
        }
    }
}

impl std::error::Error for MasterError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Shm(e) => Some(e),
            Self::Core(e) => Some(e),
            Self::Json(e) => Some(e),
            _ => None,
        }
    }
}

impl From<ShmError> for MasterError {
    fn from(e: ShmError) -> Self {
        Self::Shm(e)
    }
}

impl From<CoreError> for MasterError {
    fn from(e: CoreError) -> Self {
        Self::Core(e)
    }
}

impl From<serde_json::Error> for MasterError {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}

// ---------------------------------------------------------------- internal helpers

#[derive(Debug, Clone, Copy)]
enum SlotValue {
    F64(f64),
    Vec3([f64; 3]),
    Quat([f64; 4]),
}

impl SlotValue {
    fn zeroed(self) -> Self {
        match self {
            Self::F64(_) => Self::F64(0.0),
            Self::Vec3(_) => Self::Vec3([0.0; 3]),
            Self::Quat(_) => Self::Quat([0.0; 4]),
        }
    }
}

fn log_error(msg: &str) {
    eprintln!("[mock-webots] ERROR: {msg}");
}

pub fn validity_name(v: Validity) -> &'static str {
    match v {
        Validity::Missing => "missing",
        Validity::Valid => "valid",
        Validity::Invalid => "invalid",
        Validity::Stale => "stale",
        Validity::OutOfRange => "out_of_range",
        Validity::Error => "error",
    }
}

fn read_state(segment: &Segment) -> HandshakeState {
    HandshakeState::from_raw(segment.header().hs_state.load(Ordering::Acquire))
        .unwrap_or(HandshakeState::Error)
}

fn open_notifier(name: &str, deadline: Instant) -> Result<Notifier, MasterError> {
    loop {
        match Notifier::open(name) {
            Ok(n) => return Ok(n),
            Err(ShmError::NotFound) => {}
            Err(e) => return Err(e.into()),
        }
        if Instant::now() >= deadline {
            return Err(MasterError::Timeout(format!("notifier {name} not found")));
        }
        thread::sleep(Duration::from_millis(1));
    }
}

// ---------------------------------------------------------------- MasterChannel

/// Adapter-side handle for one model instance.
pub struct MasterChannel {
    segment: Segment,
    s2m: Notifier,
    m2s: Notifier,
    manifest: Manifest,
    layout: Layout,
    entries: HashMap<u32, SlotEntry>,
    instance_id: u32,
    model_timeout: Duration,
    next_step: u64,
    current_step: u64,
    consecutive_misses: u32,
    state_values: HashMap<u32, SlotValue>,
    state_validity: HashMap<u32, Validity>,
    last_efforts: HashMap<u32, EffortValue>,
}

impl MasterChannel {
    /// Open an instance's segment and events, wait until the host published `Ready`
    /// (or the adapter is already `Running` / `Paused`), and validate every slot
    /// against the mapping size.
    ///
    /// `NotFound` and `BadMagic` are retried until `timeout` elapses (protocol §6).
    pub fn attach(sim_id: u64, instance_id: u32, timeout: Duration) -> Result<Self, MasterError> {
        let names = names(sim_id, instance_id);
        let deadline = Instant::now() + timeout;

        let segment = loop {
            match Segment::open(&names.segment) {
                Ok(segment) => {
                    if segment.len() >= HEADER_SIZE {
                        match segment.header().validate() {
                            Ok(()) => break segment,
                            Err(CoreError::BadMagic) => {}
                            Err(e) => return Err(e.into()),
                        }
                    }
                }
                Err(ShmError::NotFound) => {}
                Err(e) => return Err(e.into()),
            }
            if Instant::now() >= deadline {
                return Err(MasterError::Timeout(format!(
                    "segment {} not ready",
                    names.segment
                )));
            }
            thread::sleep(Duration::from_millis(1));
        };

        // The host writes the manifest after `init_from_layout`; an empty area is a
        // transient state, so retry it within the same deadline.
        let manifest: Manifest = loop {
            let bytes = segment.read_json(JsonArea::Manifest)?;
            if !bytes.is_empty() {
                break serde_json::from_slice(&bytes)?;
            }
            if Instant::now() >= deadline {
                return Err(MasterError::Timeout(format!(
                    "manifest for instance {instance_id} is empty"
                )));
            }
            thread::sleep(Duration::from_millis(1));
        };

        if manifest.simulation_id_u64()? != sim_id {
            return Err(MasterError::Protocol(format!(
                "manifest simulation_id {} does not match requested {sim_id:#x}",
                manifest.simulation_id
            )));
        }
        if manifest.instance_id != instance_id {
            return Err(MasterError::Protocol(format!(
                "manifest instance_id {} does not match requested {instance_id}",
                manifest.instance_id
            )));
        }

        let specs = manifest.slot_specs()?;
        let layout = Layout::from_header(&specs, segment.header())?;
        let segment_len = segment.len() as u64;
        for slot in &layout.slots {
            let end = slot
                .value_offset
                .checked_add(slot.spec.value_type.size() as u64)
                .ok_or_else(|| {
                    MasterError::Protocol(format!("slot {} offset overflow", slot.spec.slot_id))
                })?;
            if end > segment_len {
                return Err(MasterError::Protocol(format!(
                    "slot {} value ends at {end}, segment is {segment_len} bytes",
                    slot.spec.slot_id
                )));
            }
            if slot.validity_offset >= segment_len {
                return Err(MasterError::Protocol(format!(
                    "slot {} validity offset {} is outside the segment",
                    slot.spec.slot_id, slot.validity_offset
                )));
            }
        }

        let s2m = open_notifier(&names.s2m, deadline)?;
        let m2s = open_notifier(&names.m2s, deadline)?;

        loop {
            let state = read_state(&segment);
            match state {
                HandshakeState::Ready | HandshakeState::Running | HandshakeState::Paused => break,
                HandshakeState::Error | HandshakeState::Closed => {
                    return Err(MasterError::State {
                        state,
                        detail: state_detail(&segment),
                    });
                }
                _ => {}
            }
            if Instant::now() >= deadline {
                return Err(MasterError::Timeout(format!(
                    "instance {instance_id} did not reach Ready (state {state:?})"
                )));
            }
            thread::sleep(Duration::from_millis(1));
        }

        let entries = manifest
            .slots
            .iter()
            .map(|entry| (entry.slot_id, entry.clone()))
            .collect();
        let model_timeout_ms = manifest.model_timeout_ms.max(1);
        Ok(Self {
            segment,
            s2m,
            m2s,
            manifest,
            layout,
            entries,
            instance_id,
            model_timeout: Duration::from_millis(model_timeout_ms as u64),
            next_step: 1,
            current_step: 0,
            consecutive_misses: 0,
            state_values: HashMap::new(),
            state_validity: HashMap::new(),
            last_efforts: HashMap::new(),
        })
    }

    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// Current handshake state, read from the shared header.
    pub fn hs_state(&self) -> HandshakeState {
        read_state(&self.segment)
    }

    /// Mark the adapter as actively stepping.
    ///
    /// Terminal states (`Error`/`Closed`) are never overwritten: `Error` may be
    /// written by either side (protocol §5) and must stay visible to the host, and
    /// `Closed` is final. `reset()` is the explicit recovery path from `Error`.
    pub fn set_running(&self) {
        if self.refuse_terminal_override("set_running") {
            return;
        }
        self.store_state(HandshakeState::Running);
    }

    /// Mark the adapter as paused (the host must not judge heartbeats then).
    ///
    /// Like [`MasterChannel::set_running`], this never overwrites `Error`/`Closed`.
    pub fn set_paused(&self) {
        if self.refuse_terminal_override("set_paused") {
            return;
        }
        self.store_state(HandshakeState::Paused);
    }

    /// Mark the adapter as cleanly shut down (model may exit).
    pub fn set_closed(&self) {
        self.store_state(HandshakeState::Closed);
    }

    /// Write the binding validation report (adapter-owned JSON area).
    pub fn write_report(&self, r: &Report) {
        if let Err(e) = self.try_write_report(r) {
            log_error(&format!("write_report failed: {e}"));
        }
    }

    pub fn try_write_report(&self, r: &Report) -> Result<(), MasterError> {
        let bytes = serde_json::to_vec(r)?;
        self.segment.write_json(JsonArea::Report, &bytes)?;
        Ok(())
    }

    // ------------------------------------------------------------ state setters

    pub fn set_state_f64(&mut self, slot_id: u32, v: f64) {
        if let Err(e) = self.try_set_state_f64(slot_id, v) {
            debug_assert!(false, "set_state_f64: {e}");
            log_error(&format!("set_state_f64: {e}"));
        }
    }

    pub fn try_set_state_f64(&mut self, slot_id: u32, v: f64) -> Result<(), MasterError> {
        let slot = self.state_slot(slot_id, ValueType::F64)?;
        self.store_state_value(slot, SlotValue::F64(v));
        Ok(())
    }

    pub fn set_state_vec3(&mut self, slot_id: u32, v: [f64; 3]) {
        if let Err(e) = self.try_set_state_vec3(slot_id, v) {
            debug_assert!(false, "set_state_vec3: {e}");
            log_error(&format!("set_state_vec3: {e}"));
        }
    }

    pub fn try_set_state_vec3(&mut self, slot_id: u32, v: [f64; 3]) -> Result<(), MasterError> {
        let slot = self.state_slot(slot_id, ValueType::Vec3F64)?;
        self.store_state_value(slot, SlotValue::Vec3(v));
        Ok(())
    }

    pub fn set_state_quat(&mut self, slot_id: u32, v: [f64; 4]) {
        if let Err(e) = self.try_set_state_quat(slot_id, v) {
            debug_assert!(false, "set_state_quat: {e}");
            log_error(&format!("set_state_quat: {e}"));
        }
    }

    pub fn try_set_state_quat(&mut self, slot_id: u32, v: [f64; 4]) -> Result<(), MasterError> {
        let slot = self.state_slot(slot_id, ValueType::QuatF64)?;
        self.store_state_value(slot, SlotValue::Quat(v));
        Ok(())
    }

    pub fn set_state_invalid(&mut self, slot_id: u32, why: Validity) {
        if let Err(e) = self.try_set_state_invalid(slot_id, why) {
            debug_assert!(false, "set_state_invalid: {e}");
            log_error(&format!("set_state_invalid: {e}"));
        }
    }

    pub fn try_set_state_invalid(
        &mut self,
        slot_id: u32,
        why: Validity,
    ) -> Result<(), MasterError> {
        let slot = self.state_slot_any(slot_id)?;
        let zero = self
            .state_values
            .get(&slot_id)
            .copied()
            .map(SlotValue::zeroed);
        match zero {
            Some(value) => self.write_value(&slot, value),
            None => self.write_zero_value(&slot),
        }
        self.segment.write_validity(slot.validity_offset, why);
        self.state_validity.insert(slot_id, why);
        Ok(())
    }

    fn store_state_value(&mut self, slot: SlotLayout, value: SlotValue) {
        // Value first, validity second (protocol §7 step 1).
        self.write_value(&slot, value);
        self.segment
            .write_validity(slot.validity_offset, Validity::Valid);
        self.state_values.insert(slot.spec.slot_id, value);
        self.state_validity
            .insert(slot.spec.slot_id, Validity::Valid);
    }

    // ------------------------------------------------------------ exchange

    /// Run one adapter step: write state, publish `step_id`, wait for the model,
    /// validate efforts. Never blocks when the segment is `Error` / `Closed` /
    /// `Paused` / `Resetting`.
    pub fn exchange(&mut self, sim_time_ns: i64, dt_ns: i64) -> ExchangeResult {
        let state = self.hs_state();
        match state {
            HandshakeState::Ready => self.store_state(HandshakeState::Running),
            HandshakeState::Running => {}
            HandshakeState::Error | HandshakeState::Closed => {
                let msg = format!("hs_state is {state:?}; exchange returns zeros");
                log_error(&msg);
                return self.zero_result(self.current_step, false, vec![msg]);
            }
            HandshakeState::Paused | HandshakeState::Resetting => {
                let msg = format!("hs_state is {state:?}; exchange skipped");
                log_error(&msg);
                return self.zero_result(self.current_step, false, vec![msg]);
            }
            other => {
                let msg = format!("exchange called in state {other:?}");
                log_error(&msg);
                return self.zero_result(self.current_step, false, vec![msg]);
            }
        }

        let n = self.next_step;

        // 1. All ToModel values, then their validity.
        self.write_state_slots();

        // 2. Time bookkeeping and heartbeat.
        {
            let h = self.segment.header();
            h.sim_time_ns.store(sim_time_ns, Ordering::Release);
            h.dt_ns.store(dt_ns, Ordering::Release);
            h.adapter_heartbeat.fetch_add(1, Ordering::Release);
        }

        // 3. Publish the step id last, then wake the model.
        self.segment.header().step_id.store(n, Ordering::Release);
        self.s2m.signal();

        // 4. Wait for the matching effort_step_id; ignore spurious wakeups.
        let deadline = Instant::now() + self.model_timeout;
        let mut timed_out = false;
        let mut errors = Vec::new();
        loop {
            let now = Instant::now();
            if now >= deadline {
                timed_out = true;
                break;
            }
            let remaining = deadline.saturating_duration_since(now);
            match self.m2s.wait(remaining, SPIN) {
                WaitResult::Signaled => {
                    let effort_step_id =
                        self.segment.header().effort_step_id.load(Ordering::Acquire);
                    if effort_step_id == n {
                        break;
                    }
                    errors.push(format!(
                        "spurious m2s wakeup: effort_step_id={effort_step_id}, expected {n}"
                    ));
                }
                WaitResult::TimedOut => {
                    timed_out = true;
                    break;
                }
            }
        }

        let effort_step_ok =
            !timed_out && self.segment.header().effort_step_id.load(Ordering::Acquire) == n;

        let effort_ids = self.effort_slot_ids();
        let mut efforts = Vec::with_capacity(effort_ids.len());
        let mut required_unavailable = false;

        if timed_out || !effort_step_ok {
            // Step-level miss: all efforts are zero and every slot reports Error.
            for slot_id in &effort_ids {
                let value_type = self
                    .layout
                    .slot(*slot_id)
                    .map(|s| s.spec.value_type)
                    .unwrap_or(ValueType::F64);
                efforts.push(EffortOut {
                    slot_id: *slot_id,
                    value: EffortValue::zero_for(value_type),
                    status: Validity::Error,
                });
            }
            self.last_efforts.clear();
            self.consecutive_misses += 1;
            errors.push(if timed_out {
                format!("model timeout at step {n}")
            } else {
                format!("effort_step_id does not match step {n}")
            });
        } else {
            for slot_id in &effort_ids {
                let Some(slot) = self.layout.slot(*slot_id).cloned() else {
                    continue;
                };
                let Some(entry) = self.entries.get(slot_id).cloned() else {
                    continue;
                };
                let value_type = slot.spec.value_type;
                let zero = EffortValue::zero_for(value_type);
                let validity = self.segment.read_validity(slot.validity_offset);
                let mut out = EffortOut {
                    slot_id: *slot_id,
                    value: zero,
                    status: Validity::Valid,
                };
                let mut unavailable = false;

                if validity != Validity::Valid {
                    out.status = validity;
                    errors.push(format!(
                        "slot {}: validity is {}",
                        slot_id,
                        validity_name(validity)
                    ));
                    unavailable = true;
                } else {
                    match self.read_effort(&slot) {
                        Ok(value) if value.is_finite() => out.value = value,
                        Ok(_) => {
                            out.status = Validity::Invalid;
                            errors.push(format!("slot {slot_id}: non-finite effort"));
                            unavailable = true;
                        }
                        Err(msg) => {
                            out.status = Validity::Error;
                            errors.push(msg);
                            unavailable = true;
                        }
                    }
                }

                if !unavailable {
                    match &entry.limits {
                        Some(limits) if valid_limits(limits) => {
                            let (clamped, was_clamped) = out.value.clamp(limits.max_abs);
                            out.value = clamped;
                            if was_clamped {
                                out.status = Validity::OutOfRange;
                            }
                            if let Some(rate) = limits.max_rate_per_s {
                                if dt_ns > 0 {
                                    let max_delta = rate * (dt_ns as f64 / 1e9);
                                    let last =
                                        self.last_efforts.get(slot_id).copied().unwrap_or(zero);
                                    out.value = out.value.rate_limit(last, max_delta);
                                }
                            }
                        }
                        _ => {
                            out.status = Validity::Error;
                            errors.push(format!("slot {slot_id}: missing or invalid limits"));
                            unavailable = true;
                        }
                    }
                }

                if unavailable {
                    out.value = zero;
                    self.last_efforts.remove(slot_id);
                    if entry.enabled && entry.required {
                        required_unavailable = true;
                    }
                } else {
                    self.last_efforts.insert(*slot_id, out.value);
                }
                efforts.push(out);
            }

            if required_unavailable {
                // A required effort slot was unusable: the whole step is a miss.
                for out in &mut efforts {
                    if matches!(out.status, Validity::Valid | Validity::OutOfRange) {
                        out.status = Validity::Error;
                    }
                    out.value = out.value.zeroed();
                }
                self.last_efforts.clear();
                self.consecutive_misses += 1;
                errors.push(format!("required effort slot unavailable at step {n}"));
            } else {
                self.consecutive_misses = 0;
            }
        }

        for msg in &errors {
            log_error(msg);
        }

        let max_misses = self.manifest.effort_policy.max_consecutive_misses;
        if max_misses > 0 && self.consecutive_misses >= max_misses {
            let msg = format!(
                "{} consecutive misses (limit {}); entering Error",
                self.consecutive_misses, max_misses
            );
            log_error(&msg);
            errors.push(msg);
            self.store_state(HandshakeState::Error);
        }

        self.current_step = n;
        self.next_step = n.saturating_add(1);

        ExchangeResult {
            step_id: n,
            efforts,
            model_timed_out: timed_out,
            errors,
        }
    }

    // ------------------------------------------------------------ reset

    /// Perform the epoch reset sequence from protocol §7. Returns after the model
    /// acknowledged the new epoch; the state is left at `Ready` (never `Running`).
    pub fn reset(&mut self, timeout: Duration) -> Result<(), MasterError> {
        let state = self.hs_state();
        if state == HandshakeState::Closed {
            let detail = "reset() called on a closed instance".to_string();
            log_error(&detail);
            return Err(MasterError::State { state, detail });
        }
        self.store_state(HandshakeState::Resetting);

        for slot in &self.layout.slots {
            self.segment
                .write_validity(slot.validity_offset, Validity::Missing);
        }
        {
            let h = self.segment.header();
            h.step_id.store(0, Ordering::Release);
            h.effort_step_id.store(0, Ordering::Release);
            h.epoch.fetch_add(1, Ordering::AcqRel);
        }
        let target_epoch = self.segment.header().epoch.load(Ordering::Acquire);
        self.s2m.signal();

        let deadline = Instant::now() + timeout;
        loop {
            if self
                .segment
                .header()
                .model_epoch_ack
                .load(Ordering::Acquire)
                == target_epoch
            {
                break;
            }
            if Instant::now() >= deadline {
                let msg =
                    format!("model did not acknowledge epoch {target_epoch} within {timeout:?}");
                log_error(&msg);
                self.store_state(HandshakeState::Error);
                return Err(MasterError::Timeout(msg));
            }
            thread::sleep(Duration::from_millis(1));
        }

        // Drop residual m2s wakeups (counting semaphores on Linux may have extras).
        for _ in 0..1024 {
            match self.m2s.wait(Duration::ZERO, Duration::ZERO) {
                WaitResult::Signaled => {}
                WaitResult::TimedOut => break,
            }
        }

        self.next_step = 1;
        self.current_step = 0;
        self.consecutive_misses = 0;
        self.state_values.clear();
        self.state_validity.clear();
        self.last_efforts.clear();
        self.store_state(HandshakeState::Ready);
        Ok(())
    }

    // ------------------------------------------------------------ internals

    fn store_state(&self, new: HandshakeState) {
        let old = self.hs_state();
        if old != new {
            eprintln!(
                "[mock-webots] instance {}: hs_state {:?} -> {:?}",
                self.instance_id, old, new
            );
        }
        self.segment
            .header()
            .hs_state
            .store(new.as_raw(), Ordering::Release);
    }

    /// Returns `true` when the current state is terminal and must not be replaced
    /// by a regular lifecycle transition (`Running`/`Paused`).
    fn refuse_terminal_override(&self, what: &str) -> bool {
        let state = self.hs_state();
        if matches!(state, HandshakeState::Error | HandshakeState::Closed) {
            log_error(&format!("{what} ignored: hs_state is {state:?}"));
            true
        } else {
            false
        }
    }

    fn state_slot_any(&self, slot_id: u32) -> Result<SlotLayout, MasterError> {
        let slot = self
            .layout
            .slot(slot_id)
            .ok_or_else(|| MasterError::Slot(format!("slot {slot_id} is not in the layout")))?;
        if slot.spec.direction != Direction::ToModel {
            return Err(MasterError::Slot(format!(
                "slot {slot_id} is not a to_model slot"
            )));
        }
        let enabled = self
            .entries
            .get(&slot_id)
            .map(|entry| entry.enabled)
            .unwrap_or(false);
        if !enabled {
            return Err(MasterError::Slot(format!("slot {slot_id} is disabled")));
        }
        Ok(slot.clone())
    }

    fn state_slot(&self, slot_id: u32, expected: ValueType) -> Result<SlotLayout, MasterError> {
        let slot = self.state_slot_any(slot_id)?;
        if slot.spec.value_type != expected {
            return Err(MasterError::Slot(format!(
                "slot {slot_id} is {}, not {}",
                slot.spec.value_type.name(),
                expected.name()
            )));
        }
        Ok(slot)
    }

    fn effort_slot_ids(&self) -> Vec<u32> {
        self.layout
            .slots
            .iter()
            .filter(|slot| slot.spec.direction == Direction::ToWebots)
            .filter(|slot| {
                self.entries
                    .get(&slot.spec.slot_id)
                    .map(|entry| entry.enabled)
                    .unwrap_or(false)
            })
            .map(|slot| slot.spec.slot_id)
            .collect()
    }

    fn write_state_slots(&self) {
        for slot in self
            .layout
            .slots
            .iter()
            .filter(|slot| slot.spec.direction == Direction::ToModel)
        {
            let enabled = self
                .entries
                .get(&slot.spec.slot_id)
                .map(|entry| entry.enabled)
                .unwrap_or(false);
            if !enabled {
                continue;
            }
            match self.state_values.get(&slot.spec.slot_id) {
                Some(value) => self.write_value(slot, *value),
                None => self.write_zero_value(slot),
            }
            let validity = self
                .state_validity
                .get(&slot.spec.slot_id)
                .copied()
                .unwrap_or(Validity::Missing);
            self.segment.write_validity(slot.validity_offset, validity);
        }
    }

    fn write_value(&self, slot: &SlotLayout, value: SlotValue) {
        let off = slot.value_offset;
        match value {
            SlotValue::F64(v) => self.segment.write_f64(off, v),
            SlotValue::Vec3(v) => {
                for (i, x) in v.iter().enumerate() {
                    self.segment.write_f64(off + 8 * i as u64, *x);
                }
            }
            SlotValue::Quat(v) => {
                for (i, x) in v.iter().enumerate() {
                    self.segment.write_f64(off + 8 * i as u64, *x);
                }
            }
        }
    }

    fn write_zero_value(&self, slot: &SlotLayout) {
        let zeros = [0u8; 72];
        let size = slot.spec.value_type.size();
        self.segment.write_bytes(slot.value_offset, &zeros[..size]);
    }

    fn read_effort(&self, slot: &SlotLayout) -> Result<EffortValue, String> {
        let off = slot.value_offset;
        match slot.spec.value_type {
            ValueType::F64 => Ok(EffortValue::Scalar(self.segment.read_f64(off))),
            ValueType::Vec3F64 => Ok(EffortValue::Vec3([
                self.segment.read_f64(off),
                self.segment.read_f64(off + 8),
                self.segment.read_f64(off + 16),
            ])),
            other => Err(format!(
                "slot {}: unsupported effort value type {}",
                slot.spec.slot_id,
                other.name()
            )),
        }
    }

    fn zero_result(
        &self,
        step_id: u64,
        model_timed_out: bool,
        errors: Vec<String>,
    ) -> ExchangeResult {
        let efforts = self
            .effort_slot_ids()
            .into_iter()
            .map(|slot_id| {
                let value_type = self
                    .layout
                    .slot(slot_id)
                    .map(|slot| slot.spec.value_type)
                    .unwrap_or(ValueType::F64);
                EffortOut {
                    slot_id,
                    value: EffortValue::zero_for(value_type),
                    status: Validity::Error,
                }
            })
            .collect();
        ExchangeResult {
            step_id,
            efforts,
            model_timed_out,
            errors,
        }
    }
}

fn valid_limits(limits: &Limits) -> bool {
    if !limits.max_abs.is_finite() || limits.max_abs < 0.0 {
        return false;
    }
    match limits.max_rate_per_s {
        Some(rate) => rate.is_finite() && rate >= 0.0,
        None => true,
    }
}

fn state_detail(segment: &Segment) -> String {
    for area in [JsonArea::Report, JsonArea::Hello] {
        if let Ok(bytes) = segment.read_json(area) {
            if !bytes.is_empty() {
                if let Ok(text) = String::from_utf8(bytes) {
                    return text;
                }
            }
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalar_clamp_and_rate_limit() {
        let (v, clamped) = EffortValue::Scalar(7.0).clamp(5.0);
        assert_eq!(v, EffortValue::Scalar(5.0));
        assert!(clamped);

        let v = EffortValue::Scalar(5.0).rate_limit(EffortValue::Scalar(0.0), 1.0);
        assert_eq!(v, EffortValue::Scalar(1.0));
        let v = EffortValue::Scalar(-5.0).rate_limit(EffortValue::Scalar(0.0), 1.0);
        assert_eq!(v, EffortValue::Scalar(-1.0));
    }

    #[test]
    fn vec3_clamp_and_rate_limit_are_per_component() {
        let (v, clamped) = EffortValue::Vec3([10.0, -10.0, 2.0]).clamp(5.0);
        assert_eq!(v, EffortValue::Vec3([5.0, -5.0, 2.0]));
        assert!(clamped);

        let v = v.rate_limit(EffortValue::Vec3([0.0, 0.0, 0.0]), 1.0);
        assert_eq!(v, EffortValue::Vec3([1.0, -1.0, 1.0]));
    }

    #[test]
    fn non_finite_detection() {
        assert!(!EffortValue::Scalar(f64::NAN).is_finite());
        assert!(!EffortValue::Scalar(f64::INFINITY).is_finite());
        assert!(EffortValue::Vec3([1.0, 2.0, 3.0]).is_finite());
        assert!(!EffortValue::Vec3([1.0, f64::NAN, 3.0]).is_finite());
    }
}
