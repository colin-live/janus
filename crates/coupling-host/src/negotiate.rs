//! Capability negotiation (protocol §6 step 3) as a pure function, so it can be tested without any
//! shared memory.
//!
//! Rules: protocol major must match; the model must be `steppable` and support `lockstep`; every slot of
//! the manifest must be supported by the model. A *required* slot that is not supported is an error, an
//! optional one is disabled (`enabled=false`, the layout itself never changes). Custom signals (id ≥
//! 0x8000) must match the model's hello declaration field by field.

use std::collections::HashSet;

use coupling_core::manifest::{CustomSignal, Hello, Manifest, SlotEntry};
use coupling_core::registry::is_custom;
use coupling_core::PROTOCOL_MAJOR;

#[derive(Debug, Clone, PartialEq)]
pub struct Negotiated {
    /// The manifest with final `enabled` flags.
    pub manifest: Manifest,
    pub warnings: Vec<String>,
}

fn custom_mismatches(slot: &SlotEntry, declared: &CustomSignal) -> Vec<String> {
    let mut out = Vec::new();
    let mut cmp = |what: &str, cfg: &str, hello: &str| {
        if cfg != hello {
            out.push(format!(
                "{what}: config has {cfg:?}, model declares {hello:?}"
            ));
        }
    };
    cmp("name", &slot.name, &declared.name);
    cmp("value_type", &slot.value_type, &declared.value_type);
    cmp("direction", &slot.direction, &declared.direction);
    cmp("unit", &slot.unit, &declared.unit);
    out
}

/// Returns the finalised manifest, or every reason the model is refused (joined with `; `).
pub fn negotiate(manifest: &Manifest, hello: &Hello) -> Result<Negotiated, String> {
    let mut errors: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    if hello.protocol[0] != PROTOCOL_MAJOR {
        errors.push(format!(
            "protocol major {} is not supported (host speaks {PROTOCOL_MAJOR})",
            hello.protocol[0]
        ));
    }
    if !hello.steppable {
        errors.push(
            "model is not steppable (a lockstep co-simulation needs a steppable model)".to_string(),
        );
    }
    if !hello.sync_modes.iter().any(|m| m == "lockstep") {
        errors.push(format!(
            "model does not support lockstep (supports {:?})",
            hello.sync_modes
        ));
    }
    if manifest.sync_mode != "lockstep" {
        errors.push(format!(
            "sync_mode {:?} is not supported in v1",
            manifest.sync_mode
        ));
    }

    let mut supported: HashSet<u32> = hello.supported_signals.iter().copied().collect();
    supported.extend(hello.custom_signals.iter().map(|c| c.signal_id));

    let mut out = manifest.clone();
    let mut used_custom: HashSet<u32> = HashSet::new();
    for slot in &mut out.slots {
        if is_custom(slot.signal_id) {
            used_custom.insert(slot.signal_id);
            if let Some(declared) = hello
                .custom_signals
                .iter()
                .find(|c| c.signal_id == slot.signal_id)
            {
                for m in custom_mismatches(slot, declared) {
                    errors.push(format!(
                        "custom signal 0x{:04X} ({}) {m}",
                        slot.signal_id, slot.name
                    ));
                }
            }
        }
        if !slot.enabled || supported.contains(&slot.signal_id) {
            continue;
        }
        if slot.required {
            errors.push(format!(
                "required slot {} ({}, signal 0x{:04X}) is not supported by the model",
                slot.slot_id, slot.name, slot.signal_id
            ));
        } else {
            slot.enabled = false;
            warnings.push(format!(
                "optional slot {} ({}, signal 0x{:04X}) is not supported by the model and was disabled",
                slot.slot_id, slot.name, slot.signal_id
            ));
        }
    }
    for c in &hello.custom_signals {
        if !used_custom.contains(&c.signal_id) {
            warnings.push(format!(
                "model declares custom signal 0x{:04X} ({}) that no slot of the project uses",
                c.signal_id, c.name
            ));
        }
    }

    if errors.is_empty() {
        Ok(Negotiated {
            manifest: out,
            warnings,
        })
    } else {
        Err(errors.join("; "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use coupling_core::manifest::{EffortPolicy, Entity, Limits, MANIFEST_VERSION};

    fn slot(id: u32, signal: u32, name: &str, dir: &str, required: bool) -> SlotEntry {
        SlotEntry {
            slot_id: id,
            entity_id: 1,
            signal_id: signal,
            name: name.into(),
            value_type: "f64".into(),
            direction: dir.into(),
            frame: "none".into(),
            unit: "A".into(),
            required,
            enabled: true,
            limits: if dir == "to_webots" {
                Some(Limits {
                    max_abs: 5.0,
                    max_rate_per_s: None,
                })
            } else {
                None
            },
            point: None,
        }
    }

    fn manifest(slots: Vec<SlotEntry>) -> Manifest {
        Manifest {
            manifest_version: MANIFEST_VERSION,
            simulation_id: "0x1".into(),
            instance_id: 1,
            instance_name: "m".into(),
            sync_mode: "lockstep".into(),
            dt_ns: 1_000_000,
            model_timeout_ms: 50,
            effort_policy: EffortPolicy::default(),
            world_frame: "ENU".into(),
            entities: vec![Entity {
                entity_id: 1,
                name: "joint".into(),
                webots_def: "J".into(),
                kind: "joint".into(),
                motor: None,
                sensor: None,
            }],
            slots,
        }
    }

    fn hello(signals: &[u32]) -> Hello {
        Hello {
            protocol: [1, 0],
            model_name: "m".into(),
            model_version: "0".into(),
            steppable: true,
            sync_modes: vec!["lockstep".into()],
            internal_dt_ns: None,
            supported_signals: signals.to_vec(),
            custom_signals: vec![],
        }
    }

    fn base() -> Manifest {
        manifest(vec![
            slot(0, 0x0001, "joint_position", "to_model", true),
            slot(1, 0x0002, "joint_velocity", "to_model", false),
            slot(2, 0x0101, "joint_torque", "to_webots", true),
        ])
    }

    #[test]
    fn full_support_keeps_everything_enabled() {
        let n = negotiate(&base(), &hello(&[1, 2, 0x101])).unwrap();
        assert!(n.manifest.slots.iter().all(|s| s.enabled));
        assert!(n.warnings.is_empty());
    }

    #[test]
    fn missing_required_signal_is_an_error_naming_the_slot() {
        let err = negotiate(&base(), &hello(&[1, 2])).unwrap_err();
        assert!(
            err.contains("joint_torque") && err.contains("required"),
            "{err}"
        );
    }

    #[test]
    fn missing_optional_signal_disables_the_slot_only() {
        let n = negotiate(&base(), &hello(&[1, 0x101])).unwrap();
        let vel = n.manifest.slot_by_name("joint_velocity").unwrap();
        assert!(!vel.enabled);
        assert!(n.manifest.slot_by_name("joint_position").unwrap().enabled);
        assert_eq!(n.warnings.len(), 1);
        assert_eq!(n.manifest.slots.len(), 3, "the layout must not change");
    }

    #[test]
    fn non_steppable_model_is_refused() {
        let mut h = hello(&[1, 2, 0x101]);
        h.steppable = false;
        assert!(negotiate(&base(), &h).unwrap_err().contains("steppable"));
    }

    #[test]
    fn model_without_lockstep_or_with_other_major_is_refused() {
        let mut h = hello(&[1, 2, 0x101]);
        h.sync_modes = vec!["async".into()];
        h.protocol = [2, 0];
        let err = negotiate(&base(), &h).unwrap_err();
        assert!(
            err.contains("lockstep") && err.contains("protocol major 2"),
            "{err}"
        );
    }

    fn with_custom() -> Manifest {
        let mut m = base();
        let mut s = slot(3, 0x8001, "motor_current", "to_webots", false);
        s.limits = Some(Limits {
            max_abs: 100.0,
            max_rate_per_s: None,
        });
        m.slots.push(s);
        m
    }

    fn custom(unit: &str, direction: &str) -> CustomSignal {
        CustomSignal {
            signal_id: 0x8001,
            name: "motor_current".into(),
            value_type: "f64".into(),
            direction: direction.into(),
            unit: unit.into(),
            domain: "electrical".into(),
        }
    }

    #[test]
    fn matching_custom_signal_is_accepted() {
        let mut h = hello(&[1, 2, 0x101]);
        h.custom_signals.push(custom("A", "to_webots"));
        let n = negotiate(&with_custom(), &h).unwrap();
        assert!(n.manifest.slot_by_name("motor_current").unwrap().enabled);
    }

    #[test]
    fn custom_signal_mismatch_is_an_error() {
        let mut h = hello(&[1, 2, 0x101]);
        h.custom_signals.push(custom("V", "to_model"));
        let err = negotiate(&with_custom(), &h).unwrap_err();
        assert!(err.contains("unit") && err.contains("direction"), "{err}");
    }

    #[test]
    fn undeclared_optional_custom_slot_is_disabled_and_unused_declaration_warned() {
        let n = negotiate(&with_custom(), &hello(&[1, 2, 0x101])).unwrap();
        assert!(!n.manifest.slot_by_name("motor_current").unwrap().enabled);

        let mut h = hello(&[1, 2, 0x101]);
        h.custom_signals.push(custom("A", "to_webots"));
        let n = negotiate(&base(), &h).unwrap();
        assert!(n
            .warnings
            .iter()
            .any(|w| w.contains("no slot of the project uses")));
    }
}
