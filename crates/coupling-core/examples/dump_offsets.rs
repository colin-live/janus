//! Prints the protocol constants needed by non-Rust implementations (Python adapter, C clients)
//! as JSON:  `cargo run -p coupling-core --example dump_offsets > protocol_offsets.json`

use coupling_core::header::offsets as o;
use coupling_core::registry::BASE_SIGNALS;
use coupling_core::{ValueType, HEADER_SIZE, MAGIC, PROTOCOL_MAJOR, PROTOCOL_MINOR};
use serde_json::json;

fn main() {
    let header: serde_json::Map<String, serde_json::Value> = [
        ("magic", o::MAGIC),
        ("protocol_major", o::PROTOCOL_MAJOR),
        ("header_size", o::HEADER_SIZE_FIELD),
        ("total_size", o::TOTAL_SIZE),
        ("simulation_id", o::SIMULATION_ID),
        ("instance_id", o::INSTANCE_ID),
        ("hs_state", o::HS_STATE),
        ("sync_mode", o::SYNC_MODE),
        ("epoch", o::EPOCH),
        ("model_epoch_ack", o::MODEL_EPOCH_ACK),
        ("step_id", o::STEP_ID),
        ("effort_step_id", o::EFFORT_STEP_ID),
        ("sim_time_ns", o::SIM_TIME_NS),
        ("dt_ns", o::DT_NS),
        ("adapter_heartbeat", o::ADAPTER_HEARTBEAT),
        ("model_heartbeat", o::MODEL_HEARTBEAT),
        ("manifest_off", o::MANIFEST_OFF),
        ("state_off", o::STATE_OFF),
        ("effort_off", o::EFFORT_OFF),
        ("state_slot_count", o::STATE_SLOT_COUNT),
        ("effort_slot_count", o::EFFORT_SLOT_COUNT),
        ("model_timeout_ms", o::MODEL_TIMEOUT_MS),
        ("report_off", o::REPORT_OFF),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), json!(v)))
    .collect();

    let value_types: Vec<_> = (1u8..=9)
        .filter_map(ValueType::from_raw)
        .map(|v| json!({ "id": v.as_raw(), "name": v.name(), "size": v.size() }))
        .collect();

    let signals: Vec<_> = BASE_SIGNALS
        .iter()
        .map(|s| {
            json!({
                "id": s.id, "name": s.name, "class": s.class.as_raw(),
                "value_type": s.value_type.name(), "unit": s.unit, "needs_frame": s.needs_frame
            })
        })
        .collect();

    let enum_table = |names: Vec<(String, i64)>| -> serde_json::Value {
        serde_json::Value::Object(names.into_iter().map(|(k, v)| (k, json!(v))).collect())
    };
    let enums = json!({
        "handshake_state": enum_table((0u32..=7).filter_map(HandshakeState::from_raw).map(|v| (format!("{v:?}"), v.as_raw() as i64)).collect()),
        "validity": enum_table((0u8..=5).filter_map(Validity::from_raw).map(|v| (format!("{v:?}"), v.as_raw() as i64)).collect()),
        "frame": enum_table((0u16..=4).filter_map(FrameId::from_raw).map(|v| (format!("{v:?}"), v.as_raw() as i64)).collect()),
        "sync_mode": enum_table((0u32..=1).filter_map(SyncMode::from_raw).map(|v| (format!("{v:?}"), v.as_raw() as i64)).collect()),
        "direction": enum_table((0u8..=1).filter_map(Direction::from_raw).map(|v| (format!("{v:?}"), v.as_raw() as i64)).collect()),
    });

    let out = json!({
        "magic": String::from_utf8_lossy(&MAGIC),
        "protocol": [PROTOCOL_MAJOR, PROTOCOL_MINOR],
        "header_size": HEADER_SIZE,
        "header_offsets": header,
        "enums": enums,
        "value_types": value_types,
        "signals": signals,
    });
    println!("{}", serde_json::to_string_pretty(&out).unwrap());
}
