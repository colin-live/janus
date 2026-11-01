//! The host proper: segment creation, process launching, handshake and supervision (protocol §6).

use std::path::{Path, PathBuf};
use std::process::{Child, ExitStatus};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use coupling_config::{Launch, ResolvedInstance, ResolvedProject, SimSettings};
use coupling_core::layout::{Layout, DEFAULT_HELLO_CAP, DEFAULT_MANIFEST_CAP, DEFAULT_REPORT_CAP};
use coupling_core::manifest::{Hello, Manifest, Report};
use coupling_core::{HandshakeState, SyncMode};
use coupling_shm::{names, JsonArea, Notifier, Segment, SegmentNames};

use crate::error::HostError;
use crate::negotiate::negotiate;
use crate::process::{spawn_model, JobGuard};
use crate::runfile;
use crate::watch::{HeartbeatWatch, WatchVerdict};
use crate::{log_error, log_info, log_warn};

const TICK_GRACE_FOR_CHILDREN: Duration = Duration::from_secs(2);
const ERROR_VISIBILITY_GRACE: Duration = Duration::from_millis(200);
const MIN_STALL_THRESHOLD: Duration = Duration::from_millis(100);

#[derive(Debug, Clone)]
pub struct HostOptions {
    pub sim_id: u64,
    /// `false` = every instance is treated as `Attach` (models are started by someone else).
    pub spawn: bool,
    pub hello_timeout: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TickOutcome {
    Continue,
    /// Every instance reached `Closed` (the adapter shut down): the run is over.
    AllClosed,
}

struct HostInstance {
    name: String,
    instance_id: u32,
    names: SegmentNames,
    seg: Segment,
    // Kept alive so the kernel objects exist for the whole run (the host is their creator).
    s2m: Notifier,
    _m2s: Notifier,
    manifest: Manifest,
    launch: Launch,
    child: Option<Child>,
    child_gone: bool,
    watch: HeartbeatWatch,
    timeout_ms: u32,
    error_logged: bool,
    last_state: HandshakeState,
}

fn describe(status: &ExitStatus) -> String {
    match status.code() {
        Some(c) => format!("exit code {c}"),
        None => "terminated by signal".to_string(),
    }
}

impl HostInstance {
    fn create(inst: &ResolvedInstance, sim: &SimSettings, sim_id: u64) -> Result<Self, HostError> {
        let n = names(sim_id, inst.instance_id);
        let layout = Layout::build(
            &inst.slots,
            DEFAULT_MANIFEST_CAP,
            DEFAULT_HELLO_CAP,
            DEFAULT_REPORT_CAP,
        )
        .map_err(|e| HostError::Runtime(format!("instance {}: layout: {e}", inst.name)))?;
        let seg = Segment::create(&n.segment, layout.total_size as usize).map_err(|e| {
            HostError::shm(
                format!("instance {}: create segment {}", inst.name, n.segment),
                e,
            )
        })?;
        seg.init_from_layout(&layout, sim_id, inst.instance_id, sim.model_timeout_ms)
            .map_err(|e| HostError::shm(format!("instance {}: init segment", inst.name), e))?;
        let s2m = Notifier::create(&n.s2m).map_err(|e| {
            HostError::shm(format!("instance {}: create event {}", inst.name, n.s2m), e)
        })?;
        let m2s = Notifier::create(&n.m2s).map_err(|e| {
            HostError::shm(format!("instance {}: create event {}", inst.name, n.m2s), e)
        })?;
        let manifest = inst.to_manifest(sim, sim_id);
        let json = serde_json::to_vec(&manifest).expect("manifest is plain data");
        seg.write_json(JsonArea::Manifest, &json)
            .map_err(|e| HostError::shm(format!("instance {}: write manifest", inst.name), e))?;
        log_info!(
            "instance {} (id {}): segment {} created, {} bytes, {} slots",
            inst.name,
            inst.instance_id,
            n.segment,
            layout.total_size,
            manifest.slots.len()
        );
        Ok(Self {
            name: inst.name.clone(),
            instance_id: inst.instance_id,
            names: n,
            seg,
            s2m,
            _m2s: m2s,
            manifest,
            launch: inst.launch.clone(),
            child: None,
            child_gone: false,
            watch: HeartbeatWatch::new(),
            timeout_ms: sim.model_timeout_ms,
            error_logged: false,
            last_state: HandshakeState::Created,
        })
    }

    // ------------------------------------------------------------------ state helpers
    fn state(&self) -> HandshakeState {
        HandshakeState::from_raw(self.seg.header().hs_state.load(Ordering::Acquire))
            .unwrap_or(HandshakeState::Error)
    }

    fn set_state(&self, s: HandshakeState) {
        self.seg
            .header()
            .hs_state
            .store(s.as_raw(), Ordering::Release);
    }

    fn read_report(&self) -> Option<Report> {
        let raw = self.seg.read_json(JsonArea::Report).ok()?;
        if raw.is_empty() {
            return None;
        }
        serde_json::from_slice(&raw).ok()
    }

    fn write_host_report(&self, reason: &str) {
        let report = Report {
            ok: false,
            adapter: "coupling-host".to_string(),
            world_coordinate_system: String::new(),
            entities: Vec::new(),
            errors: vec![reason.to_string()],
            warnings: Vec::new(),
        };
        let json = serde_json::to_vec(&report).expect("report is plain data");
        if let Err(e) = self.seg.write_json(JsonArea::Report, &json) {
            log_warn!(
                "instance {}: could not write the error report: {e}",
                self.name
            );
        }
    }

    /// Flip to `Error` (never from `Closed`). `reason` goes to the report area unless the adapter already
    /// left a failing report there.
    fn mark_error(&self, reason: &str) {
        if self.state() == HandshakeState::Closed {
            return;
        }
        let keep = self
            .read_report()
            .map(|r| !r.ok && !r.errors.is_empty())
            .unwrap_or(false);
        if !keep {
            self.write_host_report(reason);
        }
        self.set_state(HandshakeState::Error);
    }

    fn fail(&self, reason: String) -> HostError {
        self.write_host_report(&reason);
        self.set_state(HandshakeState::Error);
        log_error!("instance {}: {reason}", self.name);
        HostError::Handshake {
            instance: self.name.clone(),
            reason,
        }
    }

    // ------------------------------------------------------------------ process
    fn spawn(&mut self, sim_id: u64, job: &JobGuard) -> Result<(), HostError> {
        let Launch::Spawn { exe, args, cwd } = &self.launch else {
            return Ok(());
        };
        let child =
            spawn_model(exe, args, cwd, sim_id, self.instance_id, job).map_err(|message| {
                HostError::Spawn {
                    instance: self.name.clone(),
                    message,
                }
            })?;
        log_info!(
            "instance {}: started model process {} (pid {})",
            self.name,
            exe.display(),
            child.id()
        );
        self.child = Some(child);
        Ok(())
    }

    /// `Some(status)` exactly once, when the child has exited.
    fn poll_child(&mut self) -> Option<ExitStatus> {
        if self.child_gone {
            return None;
        }
        let status = self.child.as_mut()?.try_wait().ok().flatten()?;
        self.child_gone = true;
        Some(status)
    }

    // ------------------------------------------------------------------ handshake
    fn wait_hello(&mut self, deadline: Instant, timeout: Duration) -> Result<(), HostError> {
        loop {
            match self.state() {
                HandshakeState::Hello => return Ok(()),
                HandshakeState::Error => {
                    let why = self
                        .read_report()
                        .and_then(|r| r.errors.first().cloned())
                        .unwrap_or_else(|| "segment is in Error state".to_string());
                    return Err(self.fail(why));
                }
                HandshakeState::Closed => {
                    return Err(self.fail("segment was closed during the handshake".into()))
                }
                _ => {}
            }
            if let Some(status) = self.poll_child() {
                return Err(self.fail(format!(
                    "model process exited before sending hello ({})",
                    describe(&status)
                )));
            }
            if Instant::now() >= deadline {
                let hint = if self.child.is_some() {
                    "the process was started but did not connect"
                } else {
                    "no model attached (start it with WCP_SIM_ID/WCP_INSTANCE_ID set)"
                };
                return Err(self.fail(format!(
                    "no hello within {} ms: {hint}",
                    timeout.as_millis()
                )));
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn negotiate_and_ready(&mut self) -> Result<(), HostError> {
        let raw = self
            .seg
            .read_json(JsonArea::Hello)
            .map_err(|e| self.fail(format!("cannot read hello: {e}")))?;
        let hello: Hello = serde_json::from_slice(&raw)
            .map_err(|e| self.fail(format!("invalid hello JSON: {e}")))?;
        log_info!(
            "instance {}: hello from {} {} (steppable={}, {} signals, {} custom)",
            self.name,
            hello.model_name,
            hello.model_version,
            hello.steppable,
            hello.supported_signals.len(),
            hello.custom_signals.len()
        );
        let negotiated = negotiate(&self.manifest, &hello).map_err(|reason| self.fail(reason))?;
        for w in &negotiated.warnings {
            log_warn!("instance {}: {w}", self.name);
        }
        self.manifest = negotiated.manifest;
        let json = serde_json::to_vec(&self.manifest).expect("manifest is plain data");
        self.seg
            .write_json(JsonArea::Manifest, &json)
            .map_err(|e| self.fail(format!("cannot write the final manifest: {e}")))?;
        self.seg
            .header()
            .sync_mode
            .store(SyncMode::Lockstep.as_raw(), Ordering::Release);
        self.set_state(HandshakeState::Ready);
        self.last_state = HandshakeState::Ready;
        let enabled = self.manifest.slots.iter().filter(|s| s.enabled).count();
        log_info!(
            "instance {}: Ready ({} of {} slots enabled)",
            self.name,
            enabled,
            self.manifest.slots.len()
        );
        Ok(())
    }

    // ------------------------------------------------------------------ supervision
    fn tick(&mut self, now: Instant) {
        let mut st = self.state();
        if st != self.last_state {
            log_info!(
                "instance {}: state {:?} -> {:?}",
                self.name,
                self.last_state,
                st
            );
            self.last_state = st;
        }
        if let Some(status) = self.poll_child() {
            if matches!(st, HandshakeState::Closed | HandshakeState::Error) {
                log_info!(
                    "instance {}: model process ended ({})",
                    self.name,
                    describe(&status)
                );
            } else {
                let why = format!("model process exited unexpectedly ({})", describe(&status));
                log_error!("instance {}: {why}", self.name);
                self.mark_error(&why);
            }
            st = self.state();
        }
        if st == HandshakeState::Error {
            if !self.error_logged {
                let reasons = self
                    .read_report()
                    .map(|r| r.errors.join("; "))
                    .unwrap_or_default();
                log_error!(
                    "instance {} is in Error state: {}",
                    self.name,
                    if reasons.is_empty() {
                        "(no report)"
                    } else {
                        reasons.as_str()
                    }
                );
                self.error_logged = true;
            }
        } else {
            self.error_logged = false;
        }
        if st == HandshakeState::Running {
            let h = self.seg.header();
            let threshold =
                Duration::from_millis(3 * u64::from(self.timeout_ms)).max(MIN_STALL_THRESHOLD);
            let verdict = self.watch.observe(
                h.adapter_heartbeat.load(Ordering::Relaxed),
                h.model_heartbeat.load(Ordering::Relaxed),
                now,
                threshold,
            );
            if let WatchVerdict::Stalled { for_ms } = verdict {
                let why = format!(
                    "model heartbeat stalled for {for_ms} ms while the adapter kept stepping"
                );
                log_error!("instance {}: {why}", self.name);
                self.mark_error(&why);
                self.watch.reset();
            }
        } else {
            self.watch.reset();
        }
    }
}

/// All instances of one project run.
pub struct Host {
    sim_id: u64,
    instances: Vec<HostInstance>,
    job: JobGuard,
    run_file: Option<PathBuf>,
    ready: bool,
    shut_down: bool,
}

impl Host {
    /// Create all segments, start the model processes, run the handshake of every instance.
    pub fn start(project: &ResolvedProject, opts: &HostOptions) -> Result<Host, HostError> {
        let mut host = Host::create(project, opts)?;
        host.handshake(opts.hello_timeout)?;
        Ok(host)
    }

    /// Protocol §6 steps 1–3: segments, events, manifests, then launch the models.
    pub fn create(project: &ResolvedProject, opts: &HostOptions) -> Result<Host, HostError> {
        let mut host = Host {
            sim_id: opts.sim_id,
            instances: Vec::new(),
            job: JobGuard::new(),
            run_file: None,
            ready: false,
            shut_down: false,
        };
        for inst in &project.instances {
            host.instances
                .push(HostInstance::create(inst, &project.sim, opts.sim_id)?);
        }
        if opts.spawn {
            for inst in host.instances.iter_mut() {
                inst.spawn(opts.sim_id, &host.job)?;
            }
        }
        Ok(host)
    }

    /// Protocol §6 steps 4–5: wait for each hello, negotiate, publish `Ready`.
    pub fn handshake(&mut self, timeout: Duration) -> Result<(), HostError> {
        let deadline = Instant::now() + timeout;
        for inst in &mut self.instances {
            inst.wait_hello(deadline, timeout)?;
            inst.negotiate_and_ready()?;
        }
        self.ready = true;
        log_info!(
            "all {} instance(s) ready (sim_id {:016x})",
            self.instances.len(),
            self.sim_id
        );
        Ok(())
    }

    pub fn sim_id(&self) -> u64 {
        self.sim_id
    }

    /// Write `<project dir>/.janus/run.json` (removed again on shutdown).
    pub fn publish_run_file(&mut self, project_dir: &Path) -> Result<PathBuf, HostError> {
        let list: Vec<(String, u32)> = self
            .instances
            .iter()
            .map(|i| (i.name.clone(), i.instance_id))
            .collect();
        let path = runfile::write(project_dir, self.sim_id, &list).map_err(|e| {
            HostError::Runtime(format!(
                "cannot write run.json in {}: {e}",
                project_dir.display()
            ))
        })?;
        self.run_file = Some(path.clone());
        Ok(path)
    }

    /// One supervision pass (call at >= 10 Hz).
    pub fn tick(&mut self) -> TickOutcome {
        let now = Instant::now();
        for inst in &mut self.instances {
            inst.tick(now);
        }
        if !self.instances.is_empty()
            && self
                .instances
                .iter()
                .all(|i| i.state() == HandshakeState::Closed)
        {
            TickOutcome::AllClosed
        } else {
            TickOutcome::Continue
        }
    }

    pub fn states(&self) -> Vec<(String, HandshakeState)> {
        self.instances
            .iter()
            .map(|i| (i.name.clone(), i.state()))
            .collect()
    }

    /// Number of model processes that are still running (diagnostics / tests).
    pub fn live_children(&mut self) -> usize {
        self.instances
            .iter_mut()
            .filter_map(|i| i.child.as_mut())
            .filter_map(|c| c.try_wait().ok())
            .filter(|status| status.is_none())
            .count()
    }

    pub fn segment_names(&self, instance: &str) -> Option<&SegmentNames> {
        self.instances
            .iter()
            .find(|i| i.name == instance)
            .map(|i| &i.names)
    }

    /// The final (post-negotiation) manifest of an instance.
    pub fn manifest(&self, instance: &str) -> Option<&Manifest> {
        self.instances
            .iter()
            .find(|i| i.name == instance)
            .map(|i| &i.manifest)
    }

    /// Orderly shutdown: `Closed` everywhere, grace period for the children, then kill; idempotent.
    pub fn shutdown(&mut self) {
        if self.shut_down {
            return;
        }
        self.shut_down = true;
        if !self.ready
            && self
                .instances
                .iter()
                .any(|i| i.state() == HandshakeState::Error)
        {
            // Let a model that failed its handshake read the reason before `Closed` replaces `Error`.
            std::thread::sleep(ERROR_VISIBILITY_GRACE);
        }
        for inst in &self.instances {
            inst.set_state(HandshakeState::Closed);
            inst.s2m.signal();
        }
        let deadline = Instant::now() + TICK_GRACE_FOR_CHILDREN;
        for inst in &mut self.instances {
            let Some(child) = inst.child.as_mut() else {
                continue;
            };
            loop {
                match child.try_wait() {
                    Ok(Some(_)) | Err(_) => break,
                    Ok(None) if Instant::now() >= deadline => {
                        log_warn!(
                            "instance {}: model did not exit in time, killing it",
                            inst.name
                        );
                        let _ = child.kill();
                        let _ = child.wait();
                        break;
                    }
                    Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                }
            }
        }
        if let Some(p) = self.run_file.take() {
            runfile::remove(&p);
        }
        log_info!("shutdown complete");
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        self.shutdown();
    }
}
