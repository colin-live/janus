//! Parse, validate and resolve `project.yaml` into a `ResolvedProject`.
//!
//! The YAML schema is frozen in `docs/02-config-schema.md`. Validation errors
//! always identify the offending field with a dotted path. Unknown fields are
//! logged and ignored for forward compatibility.

use crate::error::ConfigError;
use crate::manifest::{EffortPolicy, Entity, Limits, Manifest, SlotEntry, MANIFEST_VERSION};
use crate::units::{self, Dim};
use crate::world::check_world_defs;
use coupling_core::layout::{DEFAULT_HELLO_CAP, DEFAULT_MANIFEST_CAP, DEFAULT_REPORT_CAP};
use coupling_core::registry::{self, SignalDef};
use coupling_core::types::{Direction, FrameId, SignalClass, SyncMode, ValueType};
use coupling_core::{Layout, SlotSpec};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Public resolved model
// ---------------------------------------------------------------------------

/// Simulation-level settings shared by every instance.
#[derive(Debug, Clone, PartialEq)]
pub struct SimSettings {
    pub world: Option<PathBuf>,
    pub basic_time_step_ms: f64,
    /// Step length in integer nanoseconds (protocol §8 `dt_ns`).
    pub dt_ns: u64,
    pub sync_mode: SyncMode,
    pub model_timeout_ms: u32,
    pub effort_policy: EffortPolicy,
    pub world_frame: String,
}

/// How a model process is obtained.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Launch {
    /// Host starts the executable. `exe` and `cwd` are resolved relative to the
    /// directory containing `project.yaml`.
    Spawn {
        exe: PathBuf,
        args: Vec<String>,
        cwd: PathBuf,
    },
    /// The process is started externally and connects to the segment.
    Attach,
}

/// Extra metadata that is not part of `coupling_core::SlotSpec` but is needed
/// for the manifest and for effort validation.
#[derive(Debug, Clone, PartialEq)]
pub struct SlotMeta {
    pub slot_id: u32,
    pub signal_id: u32,
    pub name: String,
    pub unit: String,
    pub required: bool,
    pub enabled: bool,
    pub limits: Option<Limits>,
    /// Canonical frame name (`world`, `body_local`, `joint_axis`, `model_local`);
    /// `None` serialises as `"none"`.
    pub frame: Option<String>,
    /// Application point in body-local coordinates, for body forces/torques.
    pub point: Option<[f64; 3]>,
}

/// One model instance after all names have been resolved to ids.
#[derive(Debug, Clone)]
pub struct ResolvedInstance {
    pub name: String,
    pub instance_id: u32,
    pub launch: Launch,
    pub entities: Vec<Entity>,
    pub slots: Vec<SlotSpec>,
    pub slot_meta: Vec<SlotMeta>,
}

impl ResolvedInstance {
    /// Build the protocol §8 manifest for this instance.
    pub fn to_manifest(&self, sim: &SimSettings, sim_id: u64) -> Manifest {
        let slots = self
            .slots
            .iter()
            .map(|spec| {
                let meta = self.slot_meta.iter().find(|m| m.slot_id == spec.slot_id);
                SlotEntry {
                    slot_id: spec.slot_id,
                    entity_id: spec.entity_id,
                    signal_id: spec.signal_id,
                    name: meta
                        .map(|m| m.name.clone())
                        .unwrap_or_else(|| format!("signal_{}", spec.signal_id)),
                    value_type: spec.value_type.name().to_string(),
                    direction: direction_name(spec.direction).to_string(),
                    frame: meta
                        .and_then(|m| m.frame.clone())
                        .unwrap_or_else(|| "none".to_string()),
                    unit: meta
                        .map(|m| m.unit.clone())
                        .unwrap_or_else(|| "1".to_string()),
                    required: meta.map(|m| m.required).unwrap_or(false),
                    enabled: meta.map(|m| m.enabled).unwrap_or(true),
                    limits: meta.and_then(|m| m.limits.clone()),
                    point: meta.and_then(|m| m.point),
                }
            })
            .collect();

        Manifest {
            manifest_version: MANIFEST_VERSION,
            simulation_id: format!("0x{sim_id:016x}"),
            instance_id: self.instance_id,
            instance_name: self.name.clone(),
            sync_mode: match sim.sync_mode {
                SyncMode::Lockstep => "lockstep".to_string(),
                SyncMode::Async => "async".to_string(),
            },
            dt_ns: sim.dt_ns,
            model_timeout_ms: sim.model_timeout_ms,
            effort_policy: sim.effort_policy.clone(),
            world_frame: sim.world_frame.clone(),
            entities: self.entities.clone(),
            slots,
        }
    }
}

/// Fully validated project, ready to be turned into per-instance segments.
#[derive(Debug, Clone)]
pub struct ResolvedProject {
    pub sim: SimSettings,
    pub instances: Vec<ResolvedInstance>,
    pub base_dir: PathBuf,
}

impl ResolvedProject {
    pub fn instance(&self, name: &str) -> Option<&ResolvedInstance> {
        self.instances.iter().find(|i| i.name == name)
    }
}

// ---------------------------------------------------------------------------
// Raw YAML shapes (unknown fields captured and warned about)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct RawProject {
    coupling_version: Option<u32>,
    simulation: Option<RawSimulation>,
    #[serde(default)]
    models: serde_yaml::Mapping,
    #[serde(default)]
    instances: serde_yaml::Mapping,
    #[serde(default)]
    bindings: Vec<RawBinding>,
    #[serde(flatten)]
    extra: BTreeMap<String, serde_yaml::Value>,
}

#[derive(Debug, Deserialize)]
struct RawSimulation {
    world: Option<String>,
    basic_time_step_ms: Option<f64>,
    sync_mode: Option<String>,
    model_timeout_ms: Option<u32>,
    effort_policy: Option<RawEffortPolicy>,
    world_frame: Option<String>,
    #[serde(flatten)]
    extra: BTreeMap<String, serde_yaml::Value>,
}

#[derive(Debug, Deserialize)]
struct RawEffortPolicy {
    ttl_steps: Option<u32>,
    on_timeout: Option<String>,
    max_consecutive_misses: Option<u32>,
    #[serde(flatten)]
    extra: BTreeMap<String, serde_yaml::Value>,
}

#[derive(Debug, Deserialize)]
struct RawModel {
    launch: Option<RawLaunch>,
    attach: Option<bool>,
    #[serde(flatten)]
    extra: BTreeMap<String, serde_yaml::Value>,
}

#[derive(Debug, Deserialize)]
struct RawLaunch {
    exe: Option<String>,
    #[serde(default)]
    args: Option<Vec<String>>,
    cwd: Option<String>,
    #[serde(flatten)]
    extra: BTreeMap<String, serde_yaml::Value>,
}

#[derive(Debug, Deserialize)]
struct RawInstance {
    model: Option<String>,
    id: Option<u32>,
    #[serde(default)]
    custom_signals: Vec<RawCustomSignal>,
    #[serde(flatten)]
    extra: BTreeMap<String, serde_yaml::Value>,
}

#[derive(Debug, Deserialize)]
struct RawCustomSignal {
    signal_id: Option<u32>,
    id: Option<u32>,
    name: Option<String>,
    value_type: Option<String>,
    direction: Option<String>,
    unit: Option<String>,
    /// Accepted for forward compatibility; the protocol keeps domain in hello,
    /// not in the manifest.
    #[allow(dead_code)]
    domain: Option<String>,
    #[serde(flatten)]
    extra: BTreeMap<String, serde_yaml::Value>,
}

#[derive(Debug, Deserialize)]
struct RawBinding {
    instance: Option<String>,
    #[serde(default)]
    entities: serde_yaml::Mapping,
    #[serde(default)]
    slots: Vec<RawSlot>,
    #[serde(flatten)]
    extra: BTreeMap<String, serde_yaml::Value>,
}

#[derive(Debug, Deserialize)]
struct RawEntity {
    webots_def: Option<String>,
    kind: Option<String>,
    /// Optional device-name overrides for joints (adapter auto-discovers when absent).
    motor: Option<String>,
    sensor: Option<String>,
    #[serde(flatten)]
    extra: BTreeMap<String, serde_yaml::Value>,
}

#[derive(Debug, Deserialize)]
struct RawSlot {
    signal: Option<String>,
    entity: Option<String>,
    required: Option<bool>,
    enabled: Option<bool>,
    limits: Option<RawLimits>,
    frame: Option<String>,
    point: Option<Vec<f64>>,
    /// Optional consistency checks; the registry remains the source of truth.
    direction: Option<String>,
    value_type: Option<String>,
    unit: Option<String>,
    #[serde(flatten)]
    extra: BTreeMap<String, serde_yaml::Value>,
}

#[derive(Debug, Deserialize)]
struct RawLimits {
    max_abs: Option<f64>,
    max_rate_per_s: Option<f64>,
    #[serde(flatten)]
    extra: BTreeMap<String, serde_yaml::Value>,
}

// ---------------------------------------------------------------------------
// Resolution
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct CustomDef {
    signal_id: u32,
    name: String,
    value_type: ValueType,
    direction: Direction,
    unit: String,
}

#[derive(Debug, Clone)]
struct InstanceSeed {
    id: u32,
    launch: Launch,
    customs: BTreeMap<String, CustomDef>,
}

/// How a joint moves, inferred from an explicit `unit` or from the kind of
/// effort signal bound to it. Used only to catch obviously contradictory
/// configurations early.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MotionKind {
    Rotational,
    Linear,
}

impl MotionKind {
    fn name(self) -> &'static str {
        match self {
            MotionKind::Rotational => "rotational",
            MotionKind::Linear => "linear",
        }
    }
}

struct Builder {
    name: String,
    id: u32,
    launch: Launch,
    entities: Vec<Entity>,
    entity_ids: BTreeMap<String, u32>,
    next_entity_id: u32,
    slots: Vec<SlotSpec>,
    slot_meta: Vec<SlotMeta>,
    signals: HashSet<(u32, u32, u8)>,
    next_slot_id: u32,
    entity_motion: BTreeMap<u32, MotionKind>,
    customs: BTreeMap<String, CustomDef>,
}

/// Load, validate and resolve a `project.yaml`.
pub fn load_project(path: &Path) -> Result<ResolvedProject, ConfigError> {
    let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let raw: RawProject = serde_yaml::from_str(&text).map_err(|source| ConfigError::Yaml {
        path: path.to_path_buf(),
        source,
    })?;
    resolve(raw, path)
}

fn resolve(raw: RawProject, path: &Path) -> Result<ResolvedProject, ConfigError> {
    let base_dir = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    };

    warn_extra("", &raw.extra);
    let version = raw
        .coupling_version
        .ok_or_else(|| ConfigError::validation("coupling_version", "required (expected 1)"))?;
    if version != 1 {
        return Err(ConfigError::validation(
            "coupling_version",
            format!("unsupported version {version} (expected 1)"),
        ));
    }

    let sim = resolve_simulation(
        raw.simulation
            .ok_or_else(|| ConfigError::validation("simulation", "required"))?,
        &base_dir,
    )?;
    let launches = resolve_models(&raw.models, &base_dir)?;
    let seeds = resolve_instances(&raw.instances, &launches)?;
    let mut builders = seeds
        .into_iter()
        .map(|(name, seed)| {
            (
                name.clone(),
                Builder {
                    name,
                    id: seed.id,
                    launch: seed.launch,
                    entities: Vec::new(),
                    entity_ids: BTreeMap::new(),
                    next_entity_id: 1,
                    slots: Vec::new(),
                    slot_meta: Vec::new(),
                    signals: HashSet::new(),
                    next_slot_id: 0,
                    entity_motion: BTreeMap::new(),
                    customs: seed.customs,
                },
            )
        })
        .collect::<BTreeMap<_, _>>();

    resolve_bindings(&raw.bindings, &mut builders)?;

    let mut instances = Vec::with_capacity(builders.len());
    for builder in builders.into_values() {
        Layout::build(
            &builder.slots,
            DEFAULT_MANIFEST_CAP,
            DEFAULT_HELLO_CAP,
            DEFAULT_REPORT_CAP,
        )
        .map_err(|source| ConfigError::Layout {
            instance: builder.name.clone(),
            source,
        })?;
        instances.push(ResolvedInstance {
            name: builder.name,
            instance_id: builder.id,
            launch: builder.launch,
            entities: builder.entities,
            slots: builder.slots,
            slot_meta: builder.slot_meta,
        });
    }
    instances.sort_by_key(|i| i.instance_id);

    // Offline DEF pre-check: never fatal, the adapter performs the real check.
    if let Some(world) = &sim.world {
        if world.is_file() {
            let defs: Vec<&str> = instances
                .iter()
                .flat_map(|i| i.entities.iter().map(|e| e.webots_def.as_str()))
                .collect();
            for missing in check_world_defs(world, &defs) {
                log::warn!(
                    "world {}: DEF {missing} referenced by project.yaml was not found",
                    world.display()
                );
            }
        }
    }

    Ok(ResolvedProject {
        sim,
        instances,
        base_dir,
    })
}

fn resolve_simulation(raw: RawSimulation, base_dir: &Path) -> Result<SimSettings, ConfigError> {
    warn_extra("simulation", &raw.extra);
    let basic_time_step_ms = raw
        .basic_time_step_ms
        .ok_or_else(|| ConfigError::validation("simulation.basic_time_step_ms", "required"))?;
    let dt_ns = ms_to_ns(basic_time_step_ms)
        .map_err(|message| ConfigError::validation("simulation.basic_time_step_ms", message))?;

    let sync_mode = match raw.sync_mode.as_deref().unwrap_or("lockstep") {
        "lockstep" => SyncMode::Lockstep,
        other => {
            return Err(ConfigError::validation(
                "simulation.sync_mode",
                format!("v1 does not support sync_mode {other:?} (only \"lockstep\")"),
            ))
        }
    };

    let world_frame = match raw.world_frame.as_deref().unwrap_or("ENU") {
        "ENU" | "enu" => "ENU".to_string(),
        other => {
            return Err(ConfigError::validation(
                "simulation.world_frame",
                format!("v1 world frame is fixed to ENU, got {other:?}"),
            ))
        }
    };

    let effort_policy = match raw.effort_policy {
        Some(p) => {
            warn_extra("simulation.effort_policy", &p.extra);
            EffortPolicy {
                ttl_steps: p.ttl_steps.unwrap_or(1),
                on_timeout: p.on_timeout.unwrap_or_else(|| "zero".to_string()),
                max_consecutive_misses: p.max_consecutive_misses.unwrap_or(3),
            }
        }
        None => EffortPolicy::default(),
    };
    if effort_policy.on_timeout.is_empty() {
        return Err(ConfigError::validation(
            "simulation.effort_policy.on_timeout",
            "must not be empty",
        ));
    }

    Ok(SimSettings {
        world: raw.world.map(|w| resolve_path(base_dir, &w)),
        basic_time_step_ms,
        dt_ns,
        sync_mode,
        model_timeout_ms: raw.model_timeout_ms.unwrap_or(50),
        effort_policy,
        world_frame,
    })
}

fn resolve_models(
    models: &serde_yaml::Mapping,
    base_dir: &Path,
) -> Result<BTreeMap<String, Launch>, ConfigError> {
    let mut out = BTreeMap::new();
    for (name, value) in mapping_entries(models, "models")? {
        if name.is_empty() {
            return Err(ConfigError::validation(
                "models",
                "model name must not be empty",
            ));
        }
        let path = format!("models.{name}");
        let raw: RawModel = from_value(value, &path)?;
        warn_extra(&path, &raw.extra);
        out.insert(
            name.to_string(),
            resolve_launch(name, &path, raw, base_dir)?,
        );
    }
    Ok(out)
}

fn resolve_launch(
    _name: &str,
    path: &str,
    raw: RawModel,
    base_dir: &Path,
) -> Result<Launch, ConfigError> {
    let attach = raw.attach.unwrap_or(false);
    match (attach, raw.launch) {
        (true, None) => Ok(Launch::Attach),
        (true, Some(_)) => Err(ConfigError::validation(
            path,
            "specify either 'launch' or 'attach', not both",
        )),
        (false, None) => Err(ConfigError::validation(
            path,
            "one of 'launch' or 'attach' is required",
        )),
        (false, Some(launch)) => {
            let launch_path = format!("{path}.launch");
            warn_extra(&launch_path, &launch.extra);
            let exe = launch.exe.ok_or_else(|| {
                ConfigError::validation(format!("{launch_path}.exe"), "required for launch")
            })?;
            if exe.trim().is_empty() {
                return Err(ConfigError::validation(
                    format!("{launch_path}.exe"),
                    "must not be empty",
                ));
            }
            let cwd = launch
                .cwd
                .map(|cwd| resolve_path(base_dir, &cwd))
                .unwrap_or_else(|| base_dir.to_path_buf());
            Ok(Launch::Spawn {
                exe: resolve_path(base_dir, &exe),
                args: launch.args.unwrap_or_default(),
                cwd,
            })
        }
    }
}

fn resolve_instances(
    instances: &serde_yaml::Mapping,
    launches: &BTreeMap<String, Launch>,
) -> Result<BTreeMap<String, InstanceSeed>, ConfigError> {
    let mut out = BTreeMap::new();
    let mut used_ids = BTreeSet::new();
    for (name, value) in mapping_entries(instances, "instances")? {
        if name.is_empty() {
            return Err(ConfigError::validation(
                "instances",
                "instance name must not be empty",
            ));
        }
        let path = format!("instances.{name}");
        let raw: RawInstance = from_value(value, &path)?;
        warn_extra(&path, &raw.extra);

        let id = raw
            .id
            .ok_or_else(|| ConfigError::validation(format!("{path}.id"), "required"))?;
        if id == 0 {
            return Err(ConfigError::validation(
                format!("{path}.id"),
                "instance id must be > 0",
            ));
        }
        if !used_ids.insert(id) {
            return Err(ConfigError::validation(
                format!("{path}.id"),
                format!("duplicate instance id {id}"),
            ));
        }
        let model = raw
            .model
            .ok_or_else(|| ConfigError::validation(format!("{path}.model"), "required"))?;
        let launch = launches.get(&model).cloned().ok_or_else(|| {
            ConfigError::validation(
                format!("{path}.model"),
                format!("unknown model {model:?} (not declared under models)"),
            )
        })?;
        let (customs, _custom_ids) = resolve_custom_signals(&path, &raw.custom_signals)?;
        out.insert(
            name.to_string(),
            InstanceSeed {
                id,
                launch,
                customs,
            },
        );
    }
    Ok(out)
}

fn resolve_custom_signals(
    instance_path: &str,
    raw: &[RawCustomSignal],
) -> Result<(BTreeMap<String, CustomDef>, BTreeSet<u32>), ConfigError> {
    let mut by_name = BTreeMap::new();
    let mut ids = BTreeSet::new();
    for (index, signal) in raw.iter().enumerate() {
        let path = format!("{instance_path}.custom_signals[{index}]");
        warn_extra(&path, &signal.extra);
        let signal_id = signal
            .signal_id
            .or(signal.id)
            .ok_or_else(|| ConfigError::validation(format!("{path}.signal_id"), "required"))?;
        if !(0x8000..=0xFFFF).contains(&signal_id) {
            return Err(ConfigError::validation(
                format!("{path}.signal_id"),
                format!("custom signal id 0x{signal_id:04X} must be in 0x8000..=0xFFFF"),
            ));
        }
        if !ids.insert(signal_id) {
            return Err(ConfigError::validation(
                format!("{path}.signal_id"),
                format!("duplicate custom signal id 0x{signal_id:04X}"),
            ));
        }
        let name = signal
            .name
            .clone()
            .ok_or_else(|| ConfigError::validation(format!("{path}.name"), "required"))?;
        if name.is_empty() {
            return Err(ConfigError::validation(
                format!("{path}.name"),
                "must not be empty",
            ));
        }
        if registry::lookup_name(&name).is_some() {
            return Err(ConfigError::validation(
                format!("{path}.name"),
                format!("{name:?} is a reserved base signal name"),
            ));
        }
        let value_type =
            parse_value_type(signal.value_type.as_deref().ok_or_else(|| {
                ConfigError::validation(format!("{path}.value_type"), "required")
            })?)
            .map_err(|message| ConfigError::validation(format!("{path}.value_type"), message))?;
        let direction = parse_direction(
            signal
                .direction
                .as_deref()
                .ok_or_else(|| ConfigError::validation(format!("{path}.direction"), "required"))?,
        )
        .map_err(|message| ConfigError::validation(format!("{path}.direction"), message))?;
        let unit = signal
            .unit
            .as_deref()
            .ok_or_else(|| ConfigError::validation(format!("{path}.unit"), "required"))?
            .trim()
            .to_string();
        if unit.is_empty() {
            return Err(ConfigError::validation(
                format!("{path}.unit"),
                "must not be empty",
            ));
        }
        let def = CustomDef {
            signal_id,
            name: name.clone(),
            value_type,
            direction,
            unit,
        };
        if by_name.insert(name.clone(), def).is_some() {
            return Err(ConfigError::validation(
                format!("{path}.name"),
                format!("duplicate custom signal name {name:?} in this instance"),
            ));
        }
    }
    Ok((by_name, ids))
}

fn resolve_bindings(
    bindings: &[RawBinding],
    builders: &mut BTreeMap<String, Builder>,
) -> Result<(), ConfigError> {
    for (binding_index, binding) in bindings.iter().enumerate() {
        let binding_path = format!("bindings[{binding_index}]");
        warn_extra(&binding_path, &binding.extra);
        let instance_name = binding.instance.as_deref().ok_or_else(|| {
            ConfigError::validation(format!("{binding_path}.instance"), "required")
        })?;
        let builder = builders.get_mut(instance_name).ok_or_else(|| {
            ConfigError::validation(
                format!("{binding_path}.instance"),
                format!("unknown instance {instance_name:?}"),
            )
        })?;

        for (entity_name, entity_value) in mapping_entries(&binding.entities, &binding_path)? {
            let entity_path = format!("{binding_path}.entities.{entity_name}");
            if entity_name.is_empty() {
                return Err(ConfigError::validation(
                    &entity_path,
                    "entity name must not be empty",
                ));
            }
            if builder.entity_ids.contains_key(entity_name) {
                return Err(ConfigError::validation(
                    &entity_path,
                    format!("duplicate entity {entity_name:?} in instance {instance_name:?}"),
                ));
            }
            let raw: RawEntity = from_value(entity_value, &entity_path)?;
            warn_extra(&entity_path, &raw.extra);
            let webots_def = raw
                .webots_def
                .ok_or_else(|| {
                    ConfigError::validation(format!("{entity_path}.webots_def"), "required")
                })?
                .trim()
                .to_string();
            if webots_def.is_empty() {
                return Err(ConfigError::validation(
                    format!("{entity_path}.webots_def"),
                    "must not be empty",
                ));
            }
            let kind = raw.kind.as_deref().ok_or_else(|| {
                ConfigError::validation(format!("{entity_path}.kind"), "required")
            })?;
            let kind = match kind.to_ascii_lowercase().as_str() {
                "joint" => "joint",
                "solid" => "solid",
                other => {
                    return Err(ConfigError::validation(
                        format!("{entity_path}.kind"),
                        format!("expected \"joint\" or \"solid\", got {other:?}"),
                    ))
                }
            }
            .to_string();
            let entity_id = builder.next_entity_id;
            builder.next_entity_id += 1;
            builder
                .entity_ids
                .insert(entity_name.to_string(), entity_id);
            builder.entities.push(Entity {
                entity_id,
                name: entity_name.to_string(),
                webots_def,
                kind,
                motor: raw.motor.clone(),
                sensor: raw.sensor.clone(),
            });
        }

        for (slot_index, slot) in binding.slots.iter().enumerate() {
            let slot_path = format!("{binding_path}.slots[{slot_index}]");
            warn_extra(&slot_path, &slot.extra);

            let entity_name = slot.entity.as_deref().ok_or_else(|| {
                ConfigError::validation(format!("{slot_path}.entity"), "required")
            })?;
            let entity_id = *builder.entity_ids.get(entity_name).ok_or_else(|| {
                ConfigError::validation(
                    format!("{slot_path}.entity"),
                    format!("unknown entity {entity_name:?} in this binding"),
                )
            })?;

            let signal_name = slot.signal.as_deref().ok_or_else(|| {
                ConfigError::validation(format!("{slot_path}.signal"), "required")
            })?;
            let info = if let Some(def) = registry::lookup_name(signal_name) {
                SignalInfo::from_base(def)
            } else if let Some(def) = builder.customs.get(signal_name) {
                SignalInfo::from_custom(def)
            } else {
                return Err(ConfigError::validation(
                    format!("{slot_path}.signal"),
                    format!("unknown signal {signal_name:?} (not in registry or custom_signals)"),
                ));
            };

            // Direction: registry wins, an explicit override must agree.
            if let Some(raw_direction) = &slot.direction {
                let direction = parse_direction(raw_direction).map_err(|message| {
                    ConfigError::validation(format!("{slot_path}.direction"), message)
                })?;
                if direction != info.direction {
                    return Err(ConfigError::validation(
                        format!("{slot_path}.direction"),
                        format!(
                            "conflicts with signal class (expected {})",
                            direction_name(info.direction)
                        ),
                    ));
                }
            }

            // Value type: same rule.
            if let Some(raw_type) = &slot.value_type {
                let value_type = parse_value_type(raw_type).map_err(|message| {
                    ConfigError::validation(format!("{slot_path}.value_type"), message)
                })?;
                if value_type != info.value_type {
                    return Err(ConfigError::validation(
                        format!("{slot_path}.value_type"),
                        format!("conflicts with registry ({})", info.value_type.name()),
                    ));
                }
            }

            // Unit: validate dimension, then normalise to the SI spelling.
            let unit = match slot.unit.as_deref() {
                Some(unit) => {
                    if unit.trim().is_empty() {
                        return Err(ConfigError::validation(
                            format!("{slot_path}.unit"),
                            "must not be empty",
                        ));
                    }
                    match info.allowed_dims {
                        Some(allowed) => {
                            let dim = units::dim_of(unit).map_err(|source| {
                                ConfigError::unit(format!("{slot_path}.unit"), source)
                            })?;
                            if !allowed.contains(&dim) {
                                return Err(ConfigError::validation(
                                    format!("{slot_path}.unit"),
                                    format!("unit {unit:?} does not match signal {:?}", info.name),
                                ));
                            }
                            dim.canonical_unit().to_string()
                        }
                        // Custom units may be vendor-specific; keep the spelling
                        // unless it is one we know.
                        None => units::dim_of(unit)
                            .map(|dim| dim.canonical_unit().to_string())
                            .unwrap_or_else(|_| unit.trim().to_string()),
                    }
                }
                None => info.unit.clone(),
            };

            // Conservative joint consistency check: an explicit motion unit and
            // `joint_torque`/`joint_force` describe the same joint, so mixing
            // rotational and linear signals cannot be resolved into one joint
            // type. Only explicit units are considered here; omitted units keep
            // their registry default and are left to the adapter's discovery.
            if let Some(motion) = motion_kind(&info.name, &unit, slot.unit.is_some()) {
                match builder.entity_motion.get(&entity_id) {
                    Some(existing) if *existing != motion => {
                        return Err(ConfigError::validation(
                            format!("{slot_path}.signal"),
                            format!(
                                "{} is {} but this entity's joint signals are already {}; \
                                 check `unit` and the intended joint type",
                                info.name,
                                motion.name(),
                                existing.name()
                            ),
                        ));
                    }
                    _ => {
                        builder.entity_motion.insert(entity_id, motion);
                    }
                }
            }

            // Frame: body forces/torques must choose a frame explicitly.
            let frame_id = if matches!(info.name.as_str(), "body_force" | "body_torque") {
                let frame = slot.frame.as_deref().ok_or_else(|| {
                    ConfigError::validation(
                        format!("{slot_path}.frame"),
                        "required for body_force/body_torque",
                    )
                })?;
                let frame_id = parse_frame(frame).map_err(|message| {
                    ConfigError::validation(format!("{slot_path}.frame"), message)
                })?;
                if frame_id != FrameId::World && frame_id != FrameId::BodyLocal {
                    return Err(ConfigError::validation(
                        format!("{slot_path}.frame"),
                        "body_force/body_torque frame must be 'world' or 'body_local'",
                    ));
                }
                frame_id
            } else if let Some(frame) = &slot.frame {
                parse_frame(frame).map_err(|message| {
                    ConfigError::validation(format!("{slot_path}.frame"), message)
                })?
            } else {
                info.frame
                    .as_deref()
                    .map(parse_frame)
                    .transpose()
                    .map_err(|message| {
                        ConfigError::validation(format!("{slot_path}.frame"), message)
                    })?
                    .unwrap_or(FrameId::None)
            };

            let limits = match &slot.limits {
                Some(raw) => {
                    let limits_path = format!("{slot_path}.limits");
                    warn_extra(&limits_path, &raw.extra);
                    let max_abs = raw.max_abs.ok_or_else(|| {
                        ConfigError::validation(
                            format!("{limits_path}.max_abs"),
                            "required for effort signal",
                        )
                    })?;
                    if !max_abs.is_finite() || max_abs < 0.0 {
                        return Err(ConfigError::validation(
                            format!("{limits_path}.max_abs"),
                            "must be a finite value >= 0",
                        ));
                    }
                    if let Some(rate) = raw.max_rate_per_s {
                        if !rate.is_finite() || rate < 0.0 {
                            return Err(ConfigError::validation(
                                format!("{limits_path}.max_rate_per_s"),
                                "must be a finite value >= 0",
                            ));
                        }
                    }
                    Some(Limits {
                        max_abs,
                        max_rate_per_s: raw.max_rate_per_s,
                    })
                }
                None => None,
            };
            if info.effort && limits.is_none() {
                return Err(ConfigError::validation(
                    format!("{slot_path}.limits"),
                    "required for effort signal",
                ));
            }

            let point = match &slot.point {
                Some(values) => {
                    if values.len() != 3 {
                        return Err(ConfigError::validation(
                            format!("{slot_path}.point"),
                            "expected [x, y, z]",
                        ));
                    }
                    if values.iter().any(|v| !v.is_finite()) {
                        return Err(ConfigError::validation(
                            format!("{slot_path}.point"),
                            "coordinates must be finite",
                        ));
                    }
                    Some([values[0], values[1], values[2]])
                }
                None => None,
            };

            let duplicate_key = (entity_id, info.signal_id, info.direction.as_raw());
            if !builder.signals.insert(duplicate_key) {
                return Err(ConfigError::validation(
                    format!("{slot_path}.signal"),
                    format!(
                        "duplicate signal (entity {entity_id}, signal 0x{:04X}, {})",
                        info.signal_id,
                        direction_name(info.direction)
                    ),
                ));
            }

            let slot_id = builder.next_slot_id;
            builder.next_slot_id += 1;
            builder.slots.push(SlotSpec {
                slot_id,
                entity_id,
                signal_id: info.signal_id,
                value_type: info.value_type,
                direction: info.direction,
                frame_id,
            });
            builder.slot_meta.push(SlotMeta {
                slot_id,
                signal_id: info.signal_id,
                name: info.name,
                unit,
                required: slot.required.unwrap_or(true),
                enabled: slot.enabled.unwrap_or(true),
                limits,
                frame: frame_name(frame_id),
                point,
            });
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct SignalInfo {
    signal_id: u32,
    name: String,
    value_type: ValueType,
    direction: Direction,
    unit: String,
    frame: Option<String>,
    effort: bool,
    /// `None` for custom signals (dimension is not constrained).
    allowed_dims: Option<&'static [Dim]>,
}

impl SignalInfo {
    fn from_base(def: &'static SignalDef) -> Self {
        let effort = def.class == SignalClass::Effort;
        Self {
            signal_id: def.id,
            name: def.name.to_string(),
            value_type: def.value_type,
            direction: if effort {
                Direction::ToWebots
            } else {
                Direction::ToModel
            },
            unit: default_unit(def),
            frame: default_frame(def.name).map(str::to_string),
            effort,
            allowed_dims: Some(base_allowed_dims(def.name)),
        }
    }

    fn from_custom(def: &CustomDef) -> Self {
        Self {
            signal_id: def.signal_id,
            name: def.name.clone(),
            value_type: def.value_type,
            direction: def.direction,
            unit: def.unit.clone(),
            frame: None,
            // A custom slot placed in the ToWebots region is an action and gets
            // the same safety requirement as a base Effort signal.
            effort: def.direction == Direction::ToWebots,
            allowed_dims: None,
        }
    }
}

fn default_unit(def: &SignalDef) -> String {
    let first = def.unit.split('|').next().unwrap_or(def.unit);
    units::dim_of(first)
        .map(|dim| dim.canonical_unit().to_string())
        .unwrap_or_else(|_| first.to_string())
}

fn default_frame(name: &str) -> Option<&'static str> {
    match name {
        "body_position" | "body_orientation" | "body_linear_velocity" | "body_angular_velocity" => {
            Some("world")
        }
        "joint_torque" | "joint_force" => Some("joint_axis"),
        _ => None,
    }
}

/// Motion implied by a joint signal. Position/velocity only count when the
/// configuration states a unit explicitly, so the registry's `rad` default is
/// never treated as evidence of a hinge.
fn motion_kind(signal: &str, unit: &str, explicit_unit: bool) -> Option<MotionKind> {
    match signal {
        "joint_torque" => Some(MotionKind::Rotational),
        "joint_force" => Some(MotionKind::Linear),
        "joint_position" | "joint_velocity" if explicit_unit => match unit {
            "rad" | "rad/s" => Some(MotionKind::Rotational),
            "m" | "m/s" => Some(MotionKind::Linear),
            _ => None,
        },
        _ => None,
    }
}

const ANGLE_OR_LENGTH: &[Dim] = &[Dim::Angle, Dim::Length];
const ANGVEL_OR_LINVEL: &[Dim] = &[Dim::AngularVelocity, Dim::LinearVelocity];
const TORQUE: &[Dim] = &[Dim::Torque];
const FORCE: &[Dim] = &[Dim::Force];
const LENGTH: &[Dim] = &[Dim::Length];
const LINVEL: &[Dim] = &[Dim::LinearVelocity];
const ANGVEL: &[Dim] = &[Dim::AngularVelocity];
const DIMENSIONLESS: &[Dim] = &[Dim::Dimensionless];

fn base_allowed_dims(name: &str) -> &'static [Dim] {
    match name {
        "joint_position" => ANGLE_OR_LENGTH,
        "joint_velocity" => ANGVEL_OR_LINVEL,
        "joint_torque_feedback" | "joint_torque" | "body_torque" => TORQUE,
        "body_position" => LENGTH,
        "body_orientation" => DIMENSIONLESS,
        "body_linear_velocity" => LINVEL,
        "body_angular_velocity" => ANGVEL,
        "joint_force" | "body_force" => FORCE,
        _ => &[],
    }
}

fn parse_frame(frame: &str) -> Result<FrameId, String> {
    match frame.trim().to_ascii_lowercase().as_str() {
        "none" => Ok(FrameId::None),
        "world" => Ok(FrameId::World),
        "body_local" | "body-local" | "body" => Ok(FrameId::BodyLocal),
        "joint_axis" | "joint-axis" | "joint" => Ok(FrameId::JointAxis),
        "model_local" | "model-local" | "model" => Ok(FrameId::ModelLocal),
        other => Err(format!(
            "unknown frame {other:?} (expected world|body_local|joint_axis|model_local|none)"
        )),
    }
}

fn frame_name(frame: FrameId) -> Option<String> {
    match frame {
        FrameId::None => None,
        FrameId::World => Some("world".to_string()),
        FrameId::BodyLocal => Some("body_local".to_string()),
        FrameId::JointAxis => Some("joint_axis".to_string()),
        FrameId::ModelLocal => Some("model_local".to_string()),
    }
}

fn direction_name(direction: Direction) -> &'static str {
    match direction {
        Direction::ToModel => "to_model",
        Direction::ToWebots => "to_webots",
    }
}

fn parse_direction(value: &str) -> Result<Direction, String> {
    match value.trim().to_ascii_lowercase().replace('-', "_").as_str() {
        "to_model" | "tomodel" | "model" => Ok(Direction::ToModel),
        "to_webots" | "towebots" | "webots" | "adapter" => Ok(Direction::ToWebots),
        other => Err(format!(
            "unknown direction {other:?} (expected to_model|to_webots)"
        )),
    }
}

fn parse_value_type(value: &str) -> Result<ValueType, String> {
    match value.trim().to_ascii_lowercase().replace('-', "").as_str() {
        "bool" | "boolean" => Ok(ValueType::Bool),
        "i32" => Ok(ValueType::I32),
        "u32" => Ok(ValueType::U32),
        "i64" => Ok(ValueType::I64),
        "f32" => Ok(ValueType::F32),
        "f64" => Ok(ValueType::F64),
        "vec3f64" | "vec3" | "vector3" => Ok(ValueType::Vec3F64),
        "quatf64" | "quat" | "quaternion" => Ok(ValueType::QuatF64),
        "mat3f64" | "mat3" | "matrix3" => Ok(ValueType::Mat3F64),
        other => Err(format!(
            "unknown value_type {other:?} (expected bool|i32|u32|i64|f32|f64|vec3f64|quatf64|mat3f64)"
        )),
    }
}

fn ms_to_ns(ms: f64) -> Result<u64, String> {
    if !ms.is_finite() || ms <= 0.0 {
        return Err("must be a finite value > 0".to_string());
    }
    let ns = ms * 1_000_000.0;
    let rounded = ns.round();
    if (ns - rounded).abs() > 1e-6 {
        return Err(format!(
            "{ms} ms is not an integer number of nanoseconds ({ns} ns)"
        ));
    }
    if rounded < 1.0 || rounded > u64::MAX as f64 {
        return Err(format!("{ms} ms is outside the representable dt_ns range"));
    }
    Ok(rounded as u64)
}

fn resolve_path(base_dir: &Path, value: &str) -> PathBuf {
    let path = Path::new(value);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base_dir.join(path)
    }
}

fn mapping_entries<'a>(
    mapping: &'a serde_yaml::Mapping,
    path: &str,
) -> Result<Vec<(&'a str, &'a serde_yaml::Value)>, ConfigError> {
    let mut entries = Vec::with_capacity(mapping.len());
    for (key, value) in mapping {
        let name = key
            .as_str()
            .ok_or_else(|| ConfigError::validation(path, "mapping keys must be strings"))?;
        entries.push((name, value));
    }
    Ok(entries)
}

fn from_value<T: DeserializeOwned>(
    value: &serde_yaml::Value,
    path: &str,
) -> Result<T, ConfigError> {
    serde_yaml::from_value(value.clone())
        .map_err(|source| ConfigError::validation(path, source.to_string()))
}

fn warn_extra(path: &str, extra: &BTreeMap<String, serde_yaml::Value>) {
    for key in extra.keys() {
        if path.is_empty() {
            log::warn!("ignoring unknown field {key:?}");
        } else {
            log::warn!("{path}: ignoring unknown field {key:?}");
        }
    }
}
