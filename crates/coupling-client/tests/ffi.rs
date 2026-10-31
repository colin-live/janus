//! B4 FFI details: `WCP_CLOSED` and misuse reporting through the C ABI.

mod support;

use std::ffi::{CStr, CString};
use std::ptr;
use std::sync::mpsc;
use std::thread;

use coupling_client::ffi::{
    wcp_ack_reset, wcp_close, wcp_commit, wcp_connect, wcp_invalidate, wcp_last_error,
    wcp_read_f64, wcp_slot_by_name, wcp_wait_step, wcp_write_f64, wcp_write_vec3, WcpClient,
    WcpStep, WCP_CLOSED, WCP_ERROR, WCP_OK,
};
use coupling_core::Validity;

use support::{hello, unique_sim_id, TestHost, TIMEOUT};

fn last_error() -> String {
    // SAFETY: `wcp_last_error` returns a NUL-terminated thread-local string.
    unsafe { CStr::from_ptr(wcp_last_error()) }
        .to_string_lossy()
        .into_owned()
}

#[test]
fn ffi_reports_closed_and_misuse() {
    let sim_id = unique_sim_id();
    let host = TestHost::motor(sim_id, 10);
    let (tx, rx) = mpsc::channel();

    thread::scope(|scope| {
        scope.spawn(|| {
            let hello_json = CString::new(serde_json::to_string(&hello()).expect("hello json"))
                .expect("no NUL in hello");
            let mut client: *mut WcpClient = ptr::null_mut();
            // SAFETY: `hello_json` is NUL-terminated and `client` is writable.
            let rc = unsafe { wcp_connect(sim_id, 10, hello_json.as_ptr(), 10_000, &mut client) };
            assert_eq!(rc, WCP_OK, "wcp_connect failed: {}", last_error());

            let position_name = CString::new("joint_position").expect("name");
            let torque_name = CString::new("joint_torque").expect("name");
            let mut position = 0u32;
            let mut torque = 0u32;
            // SAFETY: `client` is valid and `out`/`name` satisfy the C contract.
            assert_eq!(
                unsafe { wcp_slot_by_name(client, position_name.as_ptr(), &mut position) },
                WCP_OK
            );
            // SAFETY: same contract as above.
            assert_eq!(
                unsafe { wcp_slot_by_name(client, torque_name.as_ptr(), &mut torque) },
                WCP_OK
            );

            // Misuse must be reported as an error instead of silently succeeding.
            // SAFETY: `client` is valid; these calls only need the client pointer.
            assert_eq!(
                unsafe { wcp_ack_reset(client) },
                WCP_ERROR,
                "ack_reset without a pending reset must fail"
            );
            assert!(!last_error().is_empty(), "misuse must set wcp_last_error");
            // SAFETY: `client` is valid and `position` is a contract-valid handle.
            assert_eq!(
                unsafe { wcp_write_f64(client, position, 1.0) },
                WCP_ERROR,
                "writing a ToModel slot must fail"
            );
            let vec3 = [1.0f64, 2.0, 3.0];
            // SAFETY: `client` is valid; `vec3` provides the three readable doubles.
            assert_eq!(
                unsafe { wcp_write_vec3(client, torque, vec3.as_ptr()) },
                WCP_ERROR,
                "writing Vec3 to an F64 slot must fail"
            );
            // SAFETY: `client` is valid; validity 2 = Invalid.
            assert_eq!(
                unsafe { wcp_invalidate(client, position, 2) },
                WCP_ERROR,
                "invalidating a ToModel slot must fail"
            );

            let mut step = WcpStep {
                step_id: 0,
                sim_time_ns: 0,
                dt_ns: 0,
                epoch: 0,
                epoch_changed: 0,
            };
            // SAFETY: `client` is valid and `step` is writable.
            assert_eq!(unsafe { wcp_wait_step(client, 5_000, &mut step) }, WCP_OK);
            assert_eq!(step.step_id, 1);
            assert_eq!(step.epoch, 0);

            let mut position_value = 0.0;
            // SAFETY: `client` is valid and `position_value` is writable.
            assert_eq!(
                unsafe { wcp_read_f64(client, position, &mut position_value) },
                WCP_OK
            );
            assert_eq!(position_value, 2.0);
            // SAFETY: `client` is valid and `torque` is a writable F64 slot.
            assert_eq!(
                unsafe { wcp_write_f64(client, torque, 2.0 * position_value) },
                WCP_OK
            );
            // SAFETY: `client` is valid; one step is pending.
            assert_eq!(unsafe { wcp_commit(client) }, WCP_OK);
            tx.send(()).expect("signal effort");

            // The adapter closes cleanly: the model must see `WCP_CLOSED`, not an error.
            // SAFETY: `client` is valid and `step` is writable.
            assert_eq!(
                unsafe { wcp_wait_step(client, 5_000, &mut step) },
                WCP_CLOSED,
                "clean shutdown must map to WCP_CLOSED"
            );

            // SAFETY: `client` came from `wcp_connect` and is not used afterwards.
            unsafe { wcp_close(client) };
        });

        host.wait_hello(TIMEOUT);
        host.set_ready();
        host.step(1, 2.0);
        rx.recv_timeout(TIMEOUT).expect("client published effort");
        assert_eq!(
            host.await_effort(1, TIMEOUT),
            (Validity::Valid, 4.0),
            "the FFI client must publish the effort through the same ABI"
        );
        host.close();
    });
}
