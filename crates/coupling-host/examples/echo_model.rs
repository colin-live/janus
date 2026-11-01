//! Test model process for the host's integration tests (built by `cargo test`).
//!
//! Environment: `WCP_SIM_ID` (hex), `WCP_INSTANCE_ID`. Options:
//!   --no-steppable            hello says steppable=false
//!   --signals 1,2,257         supported signal ids (default 1,2,257)
//!   --crash-after-ms <n>      exit(3) that long after connecting (simulates a crashing model)
//! The model writes a constant torque (1.0) every step and acknowledges resets.

use std::time::{Duration, Instant};

use coupling_client::{ClientError, ModelClient, WaitEvent};
use coupling_core::manifest::Hello;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut steppable = true;
    let mut signals: Vec<u32> = vec![1, 2, 257];
    let mut crash_after: Option<Duration> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--no-steppable" => steppable = false,
            "--signals" => {
                i += 1;
                signals = args[i]
                    .split(',')
                    .filter(|s| !s.is_empty())
                    .map(|s| s.parse().expect("signal id"))
                    .collect();
            }
            "--crash-after-ms" => {
                i += 1;
                crash_after = Some(Duration::from_millis(args[i].parse().expect("ms")));
            }
            other => panic!("unknown argument {other}"),
        }
        i += 1;
    }
    let sim_id =
        u64::from_str_radix(&std::env::var("WCP_SIM_ID").expect("WCP_SIM_ID"), 16).expect("hex");
    let instance_id: u32 = std::env::var("WCP_INSTANCE_ID")
        .expect("WCP_INSTANCE_ID")
        .parse()
        .expect("id");

    let hello = Hello {
        protocol: [1, 0],
        model_name: "echo_model".into(),
        model_version: "0.1.0".into(),
        steppable,
        sync_modes: vec!["lockstep".into()],
        internal_dt_ns: None,
        supported_signals: signals,
        custom_signals: vec![],
    };
    let mut client = match ModelClient::connect(sim_id, instance_id, hello, Duration::from_secs(10))
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("echo_model: connect failed: {e}");
            std::process::exit(1);
        }
    };
    let torque = client.slot("joint_torque");
    let started = Instant::now();
    loop {
        if let Some(limit) = crash_after {
            if started.elapsed() >= limit {
                std::process::exit(3);
            }
        }
        match client.wait_step(Duration::from_millis(20)) {
            Ok(WaitEvent::Step(_)) => {
                if let Some(t) = torque {
                    client.write_f64(t, 1.0);
                }
                if client.commit().is_err() {
                    std::process::exit(1);
                }
            }
            Ok(WaitEvent::Reset { .. }) => client.ack_reset(),
            Ok(WaitEvent::Timeout) => {}
            Err(ClientError::Closed) => std::process::exit(0),
            Err(e) => {
                eprintln!("echo_model: {e}");
                std::process::exit(1);
            }
        }
    }
}
