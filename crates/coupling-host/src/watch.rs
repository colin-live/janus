//! Heartbeat watchdog for the supervision loop (B5 requirement 7).
//!
//! The document asks for "model heartbeat unchanged for > 3 × model_timeout_ms while Running". Taken
//! literally this would also fire whenever Webots is paused from its GUI (the adapter is not told, the
//! state stays `Running`, nobody steps, no heartbeat moves). The watchdog therefore only counts time
//! during which the *adapter* heartbeat advanced but the model heartbeat did not: that is a model that
//! is demonstrably being asked for work and does not answer.

use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchVerdict {
    Healthy,
    Stalled { for_ms: u128 },
}

#[derive(Debug, Default)]
pub struct HeartbeatWatch {
    last: Option<(u64, u64)>, // (adapter, model)
    stalled_since: Option<Instant>,
}

impl HeartbeatWatch {
    pub fn new() -> Self {
        Self::default()
    }

    /// Forget everything (call whenever the instance is not `Running`).
    pub fn reset(&mut self) {
        self.last = None;
        self.stalled_since = None;
    }

    pub fn observe(
        &mut self,
        adapter_hb: u64,
        model_hb: u64,
        now: Instant,
        threshold: Duration,
    ) -> WatchVerdict {
        let Some((last_adapter, last_model)) = self.last.replace((adapter_hb, model_hb)) else {
            return WatchVerdict::Healthy; // first sample is only a baseline
        };
        if model_hb != last_model {
            self.stalled_since = None;
            return WatchVerdict::Healthy;
        }
        if adapter_hb == last_adapter {
            // Nobody asked the model for anything: not a stall.
            self.stalled_since = None;
            return WatchVerdict::Healthy;
        }
        let since = *self.stalled_since.get_or_insert(now);
        let elapsed = now.saturating_duration_since(since);
        if elapsed >= threshold {
            WatchVerdict::Stalled {
                for_ms: elapsed.as_millis(),
            }
        } else {
            WatchVerdict::Healthy
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: Duration = Duration::from_millis(150);

    #[test]
    fn first_sample_is_a_baseline() {
        let mut w = HeartbeatWatch::new();
        assert_eq!(w.observe(5, 5, Instant::now(), T), WatchVerdict::Healthy);
    }

    #[test]
    fn healthy_pair_never_stalls() {
        let mut w = HeartbeatWatch::new();
        let t0 = Instant::now();
        for i in 0..20u64 {
            assert_eq!(
                w.observe(i * 10, i * 10, t0 + Duration::from_millis(i * 50), T),
                WatchVerdict::Healthy
            );
        }
    }

    #[test]
    fn paused_adapter_is_not_a_stall() {
        let mut w = HeartbeatWatch::new();
        let t0 = Instant::now();
        w.observe(10, 10, t0, T);
        for i in 1..20u64 {
            assert_eq!(
                w.observe(10, 10, t0 + Duration::from_millis(i * 100), T),
                WatchVerdict::Healthy
            );
        }
    }

    #[test]
    fn model_silent_while_adapter_advances_stalls_after_threshold() {
        let mut w = HeartbeatWatch::new();
        let t0 = Instant::now();
        w.observe(10, 10, t0, T);
        assert_eq!(
            w.observe(20, 10, t0 + Duration::from_millis(50), T),
            WatchVerdict::Healthy
        );
        assert_eq!(
            w.observe(30, 10, t0 + Duration::from_millis(100), T),
            WatchVerdict::Healthy
        );
        assert!(matches!(
            w.observe(40, 10, t0 + Duration::from_millis(210), T),
            WatchVerdict::Stalled { .. }
        ));
    }

    #[test]
    fn a_model_heartbeat_clears_the_stall_timer() {
        let mut w = HeartbeatWatch::new();
        let t0 = Instant::now();
        w.observe(10, 10, t0, T);
        w.observe(20, 10, t0 + Duration::from_millis(100), T);
        assert_eq!(
            w.observe(30, 11, t0 + Duration::from_millis(200), T),
            WatchVerdict::Healthy
        );
        assert_eq!(
            w.observe(40, 11, t0 + Duration::from_millis(250), T),
            WatchVerdict::Healthy
        );
    }

    #[test]
    fn reset_forgets_the_baseline() {
        let mut w = HeartbeatWatch::new();
        let t0 = Instant::now();
        w.observe(10, 10, t0, T);
        w.reset();
        assert_eq!(
            w.observe(20, 10, t0 + Duration::from_secs(5), T),
            WatchVerdict::Healthy
        );
    }
}
