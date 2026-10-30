//! Latency micro-benchmark (B2 acceptance criteria: report p50/p99 only).
//!
//! ```text
//! cargo test -p coupling-shm --test bench_notify -- --ignored --nocapture
//! ```

mod support;

use std::time::{Duration, Instant};

use coupling_shm::{Notifier, WaitResult};

use support::unique_names;

const SAMPLES: usize = 20_000;

#[test]
#[ignore = "measurement only, run explicitly with --ignored --nocapture"]
fn bench_signal_wait_roundtrip() {
    let n = unique_names("bench");
    let host = Notifier::create(&n.s2m).unwrap();
    let model = Notifier::open(&n.s2m).unwrap();

    // Warm up caches / kernel objects.
    for _ in 0..100 {
        host.signal();
        assert_eq!(
            model.wait(Duration::from_millis(10), Duration::ZERO),
            WaitResult::Signaled
        );
    }

    let mut samples = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let t = Instant::now();
        host.signal();
        assert_eq!(
            model.wait(Duration::from_millis(100), Duration::ZERO),
            WaitResult::Signaled
        );
        samples.push(t.elapsed());
    }
    samples.sort_unstable();

    let at = |q: f64| samples[((samples.len() - 1) as f64 * q) as usize];
    println!(
        "signal->wait same-thread roundtrip: p50={:?} p90={:?} p99={:?} max={:?} ({} samples)",
        at(0.50),
        at(0.90),
        at(0.99),
        samples[samples.len() - 1],
        SAMPLES
    );
}
