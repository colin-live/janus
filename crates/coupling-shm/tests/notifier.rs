//! Notifier tests: event semantics, wakeup latency and timeout accuracy.

mod support;

use std::time::{Duration, Instant};

use coupling_shm::{Notifier, ShmError, WaitResult};

use support::unique_names;

fn pair(tag: &str) -> (Notifier, Notifier) {
    let n = unique_names(tag);
    let a = Notifier::create(&n.s2m).unwrap();
    let b = Notifier::open(&n.s2m).unwrap();
    (a, b)
}

#[test]
fn signal_before_wait_returns_immediately() {
    let (host, model) = pair("ntf-immediate");
    host.signal();
    let t = Instant::now();
    assert_eq!(
        model.wait(Duration::from_millis(1000), Duration::ZERO),
        WaitResult::Signaled
    );
    assert!(
        t.elapsed() < Duration::from_millis(1),
        "wait for an already signalled event took {:?}",
        t.elapsed()
    );
}

#[test]
fn wait_times_out_with_sub_5ms_error() {
    let (_host, model) = pair("ntf-timeout");
    let t = Instant::now();
    let r = model.wait(Duration::from_millis(10), Duration::from_millis(1));
    let elapsed = t.elapsed();
    assert_eq!(r, WaitResult::TimedOut);
    assert!(
        elapsed >= Duration::from_millis(5) && elapsed <= Duration::from_millis(15),
        "wait(10ms) returned after {elapsed:?}, expected 10ms +/- 5ms"
    );
}

#[test]
fn wait_zero_timeout_is_a_poll() {
    let (_host, model) = pair("ntf-zero");
    let t = Instant::now();
    assert_eq!(
        model.wait(Duration::ZERO, Duration::ZERO),
        WaitResult::TimedOut
    );
    assert!(t.elapsed() < Duration::from_millis(5));

    // A pending signal is still consumed by a zero timeout.
    let (host2, model2) = pair("ntf-zero-signalled");
    host2.signal();
    assert_eq!(
        model2.wait(Duration::ZERO, Duration::ZERO),
        WaitResult::Signaled
    );
}

#[test]
fn signal_wakes_another_thread_within_1ms() {
    let (host, model) = pair("ntf-thread");
    let (tx, rx) = std::sync::mpsc::channel();
    let waiter = std::thread::spawn(move || {
        let r = model.wait(Duration::from_secs(5), Duration::ZERO);
        tx.send((r, Instant::now())).unwrap();
    });
    std::thread::sleep(Duration::from_millis(20));
    let signalled_at = Instant::now();
    host.signal();
    let (r, woke_at) = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("waiter wakeup");
    waiter.join().unwrap();
    assert_eq!(r, WaitResult::Signaled);
    let latency = woke_at.duration_since(signalled_at);
    assert!(
        latency < Duration::from_millis(1),
        "signal->wait wakeup took {latency:?}"
    );
}

#[test]
fn multiple_signals_wake_at_least_once() {
    let (host, model) = pair("ntf-multi");
    host.signal();
    host.signal();
    assert_eq!(
        model.wait(Duration::from_millis(100), Duration::ZERO),
        WaitResult::Signaled
    );

    // Windows auto-reset events are binary: both signals collapse into one wakeup.
    // POSIX named semaphores are counting, so the second wait may consume the
    // duplicate; the protocol tolerates this via `step_id` (see B2 doc status).
    #[cfg(windows)]
    assert_eq!(
        model.wait(Duration::from_millis(5), Duration::ZERO),
        WaitResult::TimedOut
    );
}

#[test]
fn duplicate_create_is_rejected() {
    let n = unique_names("ntf-dup");
    let a = Notifier::create(&n.s2m).unwrap();
    assert_eq!(
        Notifier::create(&n.s2m).unwrap_err(),
        ShmError::AlreadyExists
    );
    drop(a);
}

#[test]
fn open_missing_notifier_is_not_found() {
    let n = unique_names("ntf-missing");
    assert_eq!(Notifier::open(&n.s2m).unwrap_err(), ShmError::NotFound);
}

#[test]
fn creator_drop_destroys_the_event() {
    let n = unique_names("ntf-drop");
    let a = Notifier::create(&n.s2m).unwrap();
    let b = Notifier::open(&n.s2m).unwrap();
    drop(a);
    // POSIX unlinks the name immediately, Windows keeps the kernel object alive as
    // long as a handle (here: the opener) is open.
    #[cfg(unix)]
    assert!(Notifier::open(&n.s2m).is_err());
    // An opener still holding the handle keeps working.
    b.signal();
    assert_eq!(
        b.wait(Duration::from_millis(10), Duration::ZERO),
        WaitResult::Signaled
    );
    drop(b);
    assert_eq!(Notifier::open(&n.s2m).unwrap_err(), ShmError::NotFound);
}

#[test]
fn signalled_event_is_consumed_by_one_waiter() {
    let (host, model) = pair("ntf-consume");
    host.signal();
    assert_eq!(
        model.wait(Duration::from_millis(50), Duration::ZERO),
        WaitResult::Signaled
    );
    // Nothing left pending, regardless of the platform's underlying primitive.
    assert_eq!(
        model.wait(Duration::from_millis(5), Duration::ZERO),
        WaitResult::TimedOut
    );
}
