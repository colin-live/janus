//! B6 acceptance: consume the shared `tests/vectors/effort_validation.json`
//! (also used by B8) and assert every case against the Rust reference adapter.

mod common;

use std::time::Duration;

use common::{
    limits, manifest, slot, spawn_scripted_model, unique_sim_id, Host, ModelEffort, ScriptStep,
};
use coupling_core::types::{Direction, HandshakeState, Validity, ValueType};
use mock_webots::master::{EffortValue, MasterChannel};
use serde_json::Value;

const VECTORS: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/vectors/effort_validation.json"
));

fn parse_special(text: &str) -> Option<f64> {
    match text {
        "nan" | "NaN" => Some(f64::NAN),
        "inf" | "+inf" | "Infinity" => Some(f64::INFINITY),
        "-inf" | "-Infinity" => Some(f64::NEG_INFINITY),
        _ => None,
    }
}

fn parse_number(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => parse_special(s),
        _ => None,
    }
}

fn parse_effort(value: &Value) -> Option<ModelEffort> {
    match value {
        Value::Null => None,
        Value::Array(items) => {
            assert_eq!(items.len(), 3, "vec3 effort needs three components");
            Some(ModelEffort::Vec3([
                parse_number(&items[0]).expect("vec3 x"),
                parse_number(&items[1]).expect("vec3 y"),
                parse_number(&items[2]).expect("vec3 z"),
            ]))
        }
        other => Some(ModelEffort::Scalar(
            parse_number(other).expect("scalar effort"),
        )),
    }
}

fn validity_from_name(name: &str) -> Validity {
    match name {
        "missing" => Validity::Missing,
        "valid" => Validity::Valid,
        "invalid" => Validity::Invalid,
        "stale" => Validity::Stale,
        "out_of_range" => Validity::OutOfRange,
        "error" => Validity::Error,
        other => panic!("unknown validity {other:?}"),
    }
}

fn hs_state_from_name(name: &str) -> HandshakeState {
    match name {
        "created" => HandshakeState::Created,
        "hello" => HandshakeState::Hello,
        "ready" => HandshakeState::Ready,
        "running" => HandshakeState::Running,
        "paused" => HandshakeState::Paused,
        "resetting" => HandshakeState::Resetting,
        "error" => HandshakeState::Error,
        "closed" => HandshakeState::Closed,
        other => panic!("unknown hs_state {other:?}"),
    }
}

fn assert_value(actual: EffortValue, expected: &Value, context: &str) {
    match expected {
        Value::Array(items) => {
            let actual = actual
                .as_vec3()
                .unwrap_or_else(|| panic!("{context}: expected vec3, got {actual:?}"));
            assert_eq!(items.len(), 3, "{context}: expected vec3");
            for (i, item) in items.iter().enumerate() {
                let expected = parse_number(item).expect("expected component");
                assert!(
                    (actual[i] - expected).abs() < 1e-12,
                    "{context}: component {i}: expected {expected}, got {}",
                    actual[i]
                );
            }
        }
        other => {
            let expected = parse_number(other).expect("expected scalar");
            let actual = actual
                .as_scalar()
                .unwrap_or_else(|| panic!("{context}: expected scalar, got {actual:?}"));
            assert!(
                (actual - expected).abs() < 1e-12,
                "{context}: expected {expected}, got {actual}"
            );
        }
    }
}

#[test]
fn shared_effort_validation_vectors_pass() {
    let root: Value = serde_json::from_str(VECTORS).expect("vectors json");
    let cases = root["cases"].as_array().expect("cases array");
    assert!(!cases.is_empty(), "vectors file has no cases");

    for case in cases {
        let name = case["name"].as_str().expect("case name");
        let value_type = match case
            .get("value_type")
            .and_then(Value::as_str)
            .unwrap_or("f64")
        {
            "f64" => ValueType::F64,
            "vec3f64" => ValueType::Vec3F64,
            other => panic!("{name}: unknown value_type {other:?}"),
        };
        let (signal_id, slot_name, unit) = match value_type {
            ValueType::F64 => (0x0101, "joint_torque", "N*m"),
            ValueType::Vec3F64 => (0x0110, "body_force", "N"),
            _ => unreachable!(),
        };
        let slot_limits = match case.get("limits") {
            Some(Value::Null) | None => None,
            Some(value) => Some(limits(
                value["max_abs"].as_f64().expect("max_abs"),
                value.get("max_rate_per_s").and_then(Value::as_f64),
            )),
        };
        let required = case
            .get("required")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let dt_ns = case
            .get("dt_ns")
            .and_then(Value::as_i64)
            .unwrap_or(1_000_000);
        let model_timeout_ms = case
            .get("model_timeout_ms")
            .and_then(Value::as_u64)
            .unwrap_or(10) as u32;
        let max_misses = case
            .get("max_consecutive_misses")
            .and_then(Value::as_u64)
            .unwrap_or(3) as u32;

        let sim_id = unique_sim_id();
        let instance_id = 1;
        let host = Host::new(manifest(
            sim_id,
            instance_id,
            dt_ns as u64,
            model_timeout_ms,
            max_misses,
            vec![slot(
                0,
                signal_id,
                slot_name,
                value_type,
                Direction::ToWebots,
                unit,
                required,
                slot_limits,
            )],
        ));

        let steps = case["steps"].as_array().expect("steps array");
        let scripts: Vec<ScriptStep> = steps
            .iter()
            .map(|step| {
                let effort = step
                    .get("effort")
                    .map(parse_effort)
                    .unwrap_or(Some(ModelEffort::Scalar(0.0)));
                ScriptStep {
                    effort,
                    validity: validity_from_name(
                        step.get("validity")
                            .and_then(Value::as_str)
                            .unwrap_or("valid"),
                    ),
                    effort_step_id_ok: step
                        .get("effort_step_id_ok")
                        .and_then(Value::as_bool)
                        .unwrap_or(true),
                    respond: step.get("respond").and_then(Value::as_bool).unwrap_or(true),
                }
            })
            .collect();

        let _model = spawn_scripted_model(&host, slot_name, scripts);
        let mut channel =
            MasterChannel::attach(sim_id, instance_id, Duration::from_secs(2)).expect("attach");
        let slot_id = host.slot_id(slot_name);

        let mut sim_time_ns = 0i64;
        for (index, step) in steps.iter().enumerate() {
            let expected_step = index as u64 + 1;
            let result = channel.exchange(sim_time_ns, dt_ns);
            let context = format!("{name} step {expected_step}");
            assert_eq!(result.step_id, expected_step, "{context}: step id");
            let effort = result
                .efforts
                .iter()
                .find(|effort| effort.slot_id == slot_id)
                .unwrap_or_else(|| panic!("{context}: missing effort"));
            let expect = &step["expect"];
            assert_value(effort.value, &expect["value"], &context);
            assert_eq!(
                effort.status,
                validity_from_name(expect["status"].as_str().expect("expect.status")),
                "{context}: status"
            );
            let expected_timeout = expect
                .get("model_timed_out")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            assert_eq!(
                result.model_timed_out, expected_timeout,
                "{context}: model_timed_out"
            );
            if let Some(state) = expect.get("hs_state").and_then(Value::as_str) {
                assert_eq!(
                    channel.hs_state(),
                    hs_state_from_name(state),
                    "{context}: hs_state"
                );
            }
            sim_time_ns += dt_ns;
        }
    }
}
