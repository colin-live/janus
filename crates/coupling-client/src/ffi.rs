//! C ABI (`include/wcp_client.h`) around [`ModelClient`].
//!
//! Return convention: `0` success, `1` timeout / value unavailable, `2` reset event
//! (`wcp_wait_step` only), `3` clean shutdown (`wcp_wait_step` only), `< 0` error. On any
//! `< 0` result the thread-local message from [`wcp_last_error`] describes the failure.
//!
//! Every exported function catches unwinding panics: a panic must never cross the C
//! boundary, so it is converted into `-1` plus a message.

use std::cell::RefCell;
use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::panic::catch_unwind;
use std::ptr;
use std::slice;
use std::time::Duration;

use coupling_core::manifest::Hello;
use coupling_core::Validity;

use crate::{ClientError, ModelClient, SlotHandle, Step, WaitEvent};

/// Success.
pub const WCP_OK: i32 = 0;
/// Timeout, or the requested value is unavailable (invalid validity, wrong type, missing slot).
pub const WCP_TIMEOUT: i32 = 1;
/// `wcp_wait_step`: an epoch reset was observed; call [`wcp_ack_reset`] and do not commit.
pub const WCP_RESET: i32 = 2;
/// `wcp_wait_step`: the adapter closed the segment cleanly; terminate the model.
pub const WCP_CLOSED: i32 = 3;
/// Error; read the message with [`wcp_last_error`].
pub const WCP_ERROR: i32 = -1;

/// Mirror of `wcp_step` in `include/wcp_client.h`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct WcpStep {
    pub step_id: u64,
    pub sim_time_ns: i64,
    pub dt_ns: i64,
    pub epoch: u64,
    pub epoch_changed: u8,
}

impl From<Step> for WcpStep {
    fn from(step: Step) -> Self {
        Self {
            step_id: step.step_id,
            sim_time_ns: step.sim_time_ns,
            dt_ns: step.dt_ns,
            epoch: step.epoch,
            epoch_changed: 0,
        }
    }
}

impl WcpStep {
    /// Transport for [`WaitEvent::Reset`]: no step, only the new epoch.
    fn reset(epoch: u64) -> Self {
        Self {
            step_id: 0,
            sim_time_ns: 0,
            dt_ns: 0,
            epoch,
            epoch_changed: 1,
        }
    }
}

/// Opaque handle owned by C (`typedef struct wcp_client wcp_client;`).
pub struct WcpClient {
    inner: ModelClient,
    manifest_json: CString,
}

thread_local! {
    static LAST_ERROR: RefCell<CString> = RefCell::new(CString::new("").expect("empty CString"));
}

fn set_last_error(message: impl AsRef<str>) {
    let sanitized = message.as_ref().replace('\0', " ");
    let cstring =
        CString::new(sanitized).unwrap_or_else(|_| CString::new("error").expect("static"));
    LAST_ERROR.with(|slot| *slot.borrow_mut() = cstring);
}

/// Run `body`, converting errors and panics into `-1` with [`wcp_last_error`] set.
fn guard(body: impl FnOnce() -> Result<i32, ClientError>) -> i32 {
    match catch_unwind(std::panic::AssertUnwindSafe(body)) {
        Ok(Ok(code)) => code,
        Ok(Err(e)) => {
            set_last_error(e.to_string());
            WCP_ERROR
        }
        Err(payload) => {
            let message = payload
                .downcast_ref::<&str>()
                .map(|s| (*s).to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".to_string());
            set_last_error(format!("panic in wcp client: {message}"));
            WCP_ERROR
        }
    }
}

fn null_error(what: &str) -> ClientError {
    ClientError::BadState(format!("{what} is a null pointer"))
}

/// # Safety
/// `p` must be null or point to a NUL-terminated string valid for the duration of the call.
unsafe fn c_str<'a>(p: *const c_char, what: &str) -> Result<&'a str, ClientError> {
    if p.is_null() {
        return Err(null_error(what));
    }
    // SAFETY: the caller guarantees a valid NUL-terminated string.
    CStr::from_ptr(p)
        .to_str()
        .map_err(|_| ClientError::BadState(format!("{what} is not valid UTF-8")))
}

/// # Safety
/// `p` must be a non-null pointer returned by [`wcp_connect`] that has not been closed yet.
unsafe fn client_ref<'a>(p: *const WcpClient) -> Result<&'a WcpClient, ClientError> {
    if p.is_null() {
        return Err(null_error("client"));
    }
    // SAFETY: guaranteed by the caller; the pointer stays valid until `wcp_close`.
    Ok(&*p)
}

/// # Safety
/// `p` must be a non-null pointer returned by [`wcp_connect`] that has not been closed yet.
unsafe fn client_mut<'a>(p: *mut WcpClient) -> Result<&'a mut WcpClient, ClientError> {
    if p.is_null() {
        return Err(null_error("client"));
    }
    // SAFETY: guaranteed by the caller; unique access is the C caller's responsibility.
    Ok(&mut *p)
}

/// Connect, write the hello JSON and wait for `Ready`.
///
/// # Safety
/// `hello_json` must be a NUL-terminated UTF-8 string; `out` must point to writable storage
/// for one pointer. On success the returned pointer must be freed with [`wcp_close`].
#[no_mangle]
pub unsafe extern "C" fn wcp_connect(
    sim_id: u64,
    instance_id: u32,
    hello_json: *const c_char,
    timeout_ms: u32,
    out: *mut *mut WcpClient,
) -> i32 {
    guard(|| {
        if out.is_null() {
            return Err(null_error("out"));
        }
        // SAFETY: `out` is non-null and writable per the contract above.
        unsafe { *out = ptr::null_mut() };
        // SAFETY: `hello_json` is a NUL-terminated string per the contract above.
        let json = unsafe { c_str(hello_json, "hello_json")? };
        let hello: Hello =
            serde_json::from_str(json).map_err(|e| ClientError::Json(format!("hello: {e}")))?;
        let inner = ModelClient::connect(
            sim_id,
            instance_id,
            hello,
            Duration::from_millis(u64::from(timeout_ms)),
        )?;
        let manifest_json = serde_json::to_vec(inner.manifest())
            .map_err(|e| ClientError::Json(format!("manifest: {e}")))?;
        let manifest_json = CString::new(manifest_json)
            .map_err(|_| ClientError::BadState("manifest JSON contains a NUL byte".into()))?;
        let boxed = Box::new(WcpClient {
            inner,
            manifest_json,
        });
        // SAFETY: `out` is non-null and writable; ownership moves to the caller.
        unsafe { *out = Box::into_raw(boxed) };
        Ok(WCP_OK)
    })
}

/// Close a client returned by [`wcp_connect`]. Null is a no-op.
///
/// # Safety
/// `client` must be null or a pointer returned by [`wcp_connect`] that has not been closed.
#[no_mangle]
pub unsafe extern "C" fn wcp_close(client: *mut WcpClient) {
    if client.is_null() {
        return;
    }
    let _ = catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: the pointer came from `Box::into_raw` and is not used afterwards.
        drop(unsafe { Box::from_raw(client) });
    }));
}

/// Canonical manifest JSON of the connected segment.
///
/// # Safety
/// `client` must be a valid pointer from [`wcp_connect`]; the returned pointer is owned by
/// the client and stays valid until [`wcp_close`].
#[no_mangle]
pub unsafe extern "C" fn wcp_manifest_json(client: *const WcpClient) -> *const c_char {
    let result = catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: the caller guarantees a valid client pointer.
        let client = unsafe { client_ref(client) }?;
        Ok::<_, ClientError>(client.manifest_json.as_ptr())
    }));
    match result {
        Ok(Ok(p)) => p,
        Ok(Err(e)) => {
            set_last_error(e.to_string());
            ptr::null()
        }
        Err(payload) => {
            set_last_error(format!("panic in wcp_manifest_json: {payload:?}"));
            ptr::null()
        }
    }
}

/// Resolve a slot name; writes the handle to `out`. Returns `0`, `1` (unknown/disabled) or `< 0`.
///
/// # Safety
/// `client` must be valid, `name` a NUL-terminated string and `out` writable.
#[no_mangle]
pub unsafe extern "C" fn wcp_slot_by_name(
    client: *const WcpClient,
    name: *const c_char,
    out: *mut u32,
) -> i32 {
    guard(|| {
        // SAFETY: contract above.
        let client = unsafe { client_ref(client)? };
        if out.is_null() {
            return Err(null_error("out"));
        }
        // SAFETY: contract above.
        let name = unsafe { c_str(name, "name")? };
        match client.inner.slot(name) {
            Some(handle) => {
                // SAFETY: `out` is non-null and writable.
                unsafe { *out = handle.raw() };
                Ok(WCP_OK)
            }
            None => {
                set_last_error(format!("slot {name:?} not found or disabled"));
                Ok(WCP_TIMEOUT)
            }
        }
    })
}

/// Wait for the next event: `0` (step written to `out`), `2` (reset event written to `out`,
/// `step_id == 0`; call [`wcp_ack_reset`]), `1` (timeout), `3` (clean shutdown) or `< 0`
/// (error).
///
/// # Safety
/// `client` must be valid and `out` writable.
#[no_mangle]
pub unsafe extern "C" fn wcp_wait_step(
    client: *mut WcpClient,
    timeout_ms: u32,
    out: *mut WcpStep,
) -> i32 {
    guard(|| {
        // SAFETY: contract above.
        let client = unsafe { client_mut(client)? };
        if out.is_null() {
            return Err(null_error("out"));
        }
        let event = match client
            .inner
            .wait_step(Duration::from_millis(u64::from(timeout_ms)))
        {
            Ok(event) => event,
            Err(ClientError::Closed) => return Ok(WCP_CLOSED),
            Err(e) => return Err(e),
        };
        match event {
            WaitEvent::Step(step) => {
                // SAFETY: `out` is non-null and writable.
                unsafe { *out = step.into() };
                Ok(WCP_OK)
            }
            WaitEvent::Reset { epoch } => {
                // SAFETY: `out` is non-null and writable.
                unsafe { *out = WcpStep::reset(epoch) };
                Ok(WCP_RESET)
            }
            WaitEvent::Timeout => Ok(WCP_TIMEOUT),
        }
    })
}

/// Acknowledge the reset reported by [`wcp_wait_step`] after reinitialising internal state.
/// Returns `0`, or `< 0` when no reset is pending.
///
/// # Safety
/// `client` must be a valid pointer from [`wcp_connect`].
#[no_mangle]
pub unsafe extern "C" fn wcp_ack_reset(client: *mut WcpClient) -> i32 {
    guard(|| {
        // SAFETY: contract above.
        let client = unsafe { client_mut(client)? };
        client.inner.try_ack_reset()?;
        Ok(WCP_OK)
    })
}

/// Read an F64 slot. Returns `0` (value written), `1` (not valid / wrong type) or `< 0`.
///
/// # Safety
/// `client` must be valid and `out` writable.
#[no_mangle]
pub unsafe extern "C" fn wcp_read_f64(client: *const WcpClient, slot: u32, out: *mut f64) -> i32 {
    guard(|| {
        // SAFETY: contract above.
        let client = unsafe { client_ref(client)? };
        if out.is_null() {
            return Err(null_error("out"));
        }
        match client.inner.read_f64(SlotHandle::from_raw(slot)) {
            Some(v) => {
                // SAFETY: `out` is non-null and writable.
                unsafe { *out = v };
                Ok(WCP_OK)
            }
            None => Ok(WCP_TIMEOUT),
        }
    })
}

/// Read a Vec3 slot into `out[0..3]`. Returns `0`, `1` or `< 0`.
///
/// # Safety
/// `client` must be valid and `out` writable for three doubles.
#[no_mangle]
pub unsafe extern "C" fn wcp_read_vec3(client: *const WcpClient, slot: u32, out: *mut f64) -> i32 {
    guard(|| {
        // SAFETY: contract above.
        let client = unsafe { client_ref(client)? };
        if out.is_null() {
            return Err(null_error("out"));
        }
        match client.inner.read_vec3(SlotHandle::from_raw(slot)) {
            Some(v) => {
                // SAFETY: `out` is non-null and writable for three doubles.
                unsafe { ptr::copy_nonoverlapping(v.as_ptr(), out, 3) };
                Ok(WCP_OK)
            }
            None => Ok(WCP_TIMEOUT),
        }
    })
}

/// Read a Quat slot into `out[0..4]`. Returns `0`, `1` or `< 0`.
///
/// # Safety
/// `client` must be valid and `out` writable for four doubles.
#[no_mangle]
pub unsafe extern "C" fn wcp_read_quat(client: *const WcpClient, slot: u32, out: *mut f64) -> i32 {
    guard(|| {
        // SAFETY: contract above.
        let client = unsafe { client_ref(client)? };
        if out.is_null() {
            return Err(null_error("out"));
        }
        match client.inner.read_quat(SlotHandle::from_raw(slot)) {
            Some(v) => {
                // SAFETY: `out` is non-null and writable for four doubles.
                unsafe { ptr::copy_nonoverlapping(v.as_ptr(), out, 4) };
                Ok(WCP_OK)
            }
            None => Ok(WCP_TIMEOUT),
        }
    })
}

/// Write an F64 effort value. Returns `0`, or `< 0` when the slot is not a ToWebots F64 slot.
///
/// # Safety
/// `client` must be a valid pointer from [`wcp_connect`].
#[no_mangle]
pub unsafe extern "C" fn wcp_write_f64(client: *mut WcpClient, slot: u32, value: f64) -> i32 {
    guard(|| {
        // SAFETY: contract above.
        let client = unsafe { client_mut(client)? };
        client
            .inner
            .try_write_f64(SlotHandle::from_raw(slot), value)?;
        Ok(WCP_OK)
    })
}

/// Write a Vec3 effort value. Returns `0`, or `< 0` when the slot is not a ToWebots Vec3 slot.
///
/// # Safety
/// `client` must be valid and `value` must point to three readable doubles.
#[no_mangle]
pub unsafe extern "C" fn wcp_write_vec3(
    client: *mut WcpClient,
    slot: u32,
    value: *const f64,
) -> i32 {
    guard(|| {
        // SAFETY: contract above.
        let client = unsafe { client_mut(client)? };
        if value.is_null() {
            return Err(null_error("value"));
        }
        // SAFETY: `value` points to three readable doubles per the contract above.
        let v = unsafe { slice::from_raw_parts(value, 3) };
        client
            .inner
            .try_write_vec3(SlotHandle::from_raw(slot), [v[0], v[1], v[2]])?;
        Ok(WCP_OK)
    })
}

/// Mark an effort slot invalid. `validity` uses the protocol byte values (`2` = Invalid,
/// `5` = Error, …). Returns `0`, or `< 0` for an unknown validity or a non-effort slot.
///
/// # Safety
/// `client` must be a valid pointer from [`wcp_connect`].
#[no_mangle]
pub unsafe extern "C" fn wcp_invalidate(client: *mut WcpClient, slot: u32, validity: u8) -> i32 {
    guard(|| {
        // SAFETY: contract above.
        let client = unsafe { client_mut(client)? };
        let why = Validity::from_raw(validity)
            .ok_or_else(|| ClientError::BadState(format!("unknown validity value {validity}")))?;
        client
            .inner
            .try_invalidate(SlotHandle::from_raw(slot), why)?;
        Ok(WCP_OK)
    })
}

/// Publish the effort of the pending step. Returns `0` or `< 0` (e.g. no pending step).
///
/// # Safety
/// `client` must be a valid pointer from [`wcp_connect`].
#[no_mangle]
pub unsafe extern "C" fn wcp_commit(client: *mut WcpClient) -> i32 {
    guard(|| {
        // SAFETY: contract above.
        let client = unsafe { client_mut(client)? };
        client.inner.commit()?;
        Ok(WCP_OK)
    })
}

/// Message of the last failed call on this thread (empty when none).
///
/// The pointer is thread-local and remains valid until the next failed call on the same
/// thread.
#[no_mangle]
pub extern "C" fn wcp_last_error() -> *const c_char {
    LAST_ERROR.with(|slot| slot.borrow().as_ptr())
}
