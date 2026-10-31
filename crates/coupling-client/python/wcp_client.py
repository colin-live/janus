"""Python binding of Block B4 (`coupling-client`) over the C ABI.

Requirements: `docs/blocks/B4-client.md`. No third-party dependencies (only `ctypes`).

The native library is loaded from ``WCP_CLIENT_LIB`` when set, otherwise it is
looked up next to the crate (``target/{debug,release}``).

Usage::

    from wcp_client import ModelClient

    hello = {
        "protocol": [1, 0],
        "model_name": "demo",
        "model_version": "0.1.0",
        "steppable": True,
        "sync_modes": ["lockstep"],
        "supported_signals": [1, 257],
    }
    with ModelClient.connect(0x1234, 1, hello) as client:
        pos = client.slot("joint_position")
        torque = client.slot("joint_torque")
        while (event := client.wait_step(1.0)) is not None:
            if isinstance(event, Reset):
                client.ack_reset()  # reinitialise internal state first
                continue
            client.write_f64(torque, -10.0 * (client.read_f64(pos) or 0.0))
            client.commit()
"""

from __future__ import annotations

import ctypes
import json
import os
from pathlib import Path
from typing import Any, Mapping, NamedTuple, Optional, Sequence, Tuple, Union

__all__ = [
    "ModelClient",
    "Step",
    "Reset",
    "WcpError",
    "Closed",
    "load_library",
    "WCP_MISSING",
    "WCP_VALID",
    "WCP_INVALID",
    "WCP_STALE",
    "WCP_OUT_OF_RANGE",
    "WCP_VALIDITY_ERROR",
    "WCP_ERROR",
    "WCP_RESET",
    "WCP_CLOSED",
]

#: Success (mirrors ``WCP_OK`` in ``include/wcp_client.h``).
WCP_OK = 0
#: Timeout, or the requested value is unavailable (invalid validity / wrong type).
WCP_TIMEOUT = 1
#: Epoch reset (``wait_step`` only); call :meth:`ModelClient.ack_reset`.
WCP_RESET = 2
#: Clean adapter shutdown (``wait_step`` only); the model should terminate.
WCP_CLOSED = 3
#: Error (message from ``wcp_last_error``).
WCP_ERROR = -1

#: Validity byte values (protocol §3), for :meth:`ModelClient.invalidate`.
WCP_MISSING = 0
WCP_VALID = 1
WCP_INVALID = 2
WCP_STALE = 3
WCP_OUT_OF_RANGE = 4
#: The model could not compute an effort (protocol §10).
WCP_VALIDITY_ERROR = 5

_LIB_NAMES = ("coupling_client.dll", "libcoupling_client.so", "libcoupling_client.dylib")


class WcpError(RuntimeError):
    """A call into the native client failed (return code ``< 0``)."""


class Closed(WcpError):
    """The adapter closed the segment cleanly; the model should terminate."""


class Step(NamedTuple):
    """Metadata of one simulation step (mirrors ``wcp_step``)."""

    step_id: int
    sim_time_ns: int
    dt_ns: int
    epoch: int
    epoch_changed: bool


class Reset(NamedTuple):
    """Epoch reset event: no step is carried, only the new epoch."""

    epoch: int


class _WcpStep(ctypes.Structure):
    _fields_ = [
        ("step_id", ctypes.c_uint64),
        ("sim_time_ns", ctypes.c_int64),
        ("dt_ns", ctypes.c_int64),
        ("epoch", ctypes.c_uint64),
        ("epoch_changed", ctypes.c_uint8),
    ]


def _candidate_paths() -> Sequence[Path]:
    """Places the cdylib may live, relative to this file (workspace ``target``)."""
    explicit = os.environ.get("WCP_CLIENT_LIB")
    if explicit:
        return (Path(explicit),)
    here = Path(__file__).resolve()
    roots = [here.parent, *here.parents[1:4]]
    candidates = []
    for root in roots:
        for profile in ("debug", "release"):
            for name in _LIB_NAMES:
                candidates.append(root / "target" / profile / name)
    return candidates


def load_library(path: Optional[Union[str, os.PathLike]] = None) -> ctypes.CDLL:
    """Load the native client library and declare the C prototypes.

    Raises ``OSError`` when no candidate library exists. The result is cached by
    :class:`ModelClient`, so callers normally do not need this function.
    """
    if path is not None:
        return _configure(ctypes.CDLL(str(path)))
    candidates = _candidate_paths()
    tried = []
    for candidate in candidates:
        if candidate.is_file():
            return _configure(ctypes.CDLL(str(candidate)))
        tried.append(str(candidate))
    raise OSError(
        "coupling_client native library not found; build it with "
        "`cargo build -p coupling-client` and/or set WCP_CLIENT_LIB.\nTried:\n  "
        + "\n  ".join(tried)
    )


def _configure(lib: ctypes.CDLL) -> ctypes.CDLL:
    lib.wcp_connect.argtypes = [
        ctypes.c_uint64,
        ctypes.c_uint32,
        ctypes.c_char_p,
        ctypes.c_uint32,
        ctypes.POINTER(ctypes.c_void_p),
    ]
    lib.wcp_connect.restype = ctypes.c_int32
    lib.wcp_close.argtypes = [ctypes.c_void_p]
    lib.wcp_close.restype = None
    lib.wcp_manifest_json.argtypes = [ctypes.c_void_p]
    lib.wcp_manifest_json.restype = ctypes.c_char_p
    lib.wcp_slot_by_name.argtypes = [
        ctypes.c_void_p,
        ctypes.c_char_p,
        ctypes.POINTER(ctypes.c_uint32),
    ]
    lib.wcp_slot_by_name.restype = ctypes.c_int32
    lib.wcp_wait_step.argtypes = [
        ctypes.c_void_p,
        ctypes.c_uint32,
        ctypes.POINTER(_WcpStep),
    ]
    lib.wcp_wait_step.restype = ctypes.c_int32
    lib.wcp_ack_reset.argtypes = [ctypes.c_void_p]
    lib.wcp_ack_reset.restype = ctypes.c_int32
    for name in ("wcp_read_f64", "wcp_read_vec3", "wcp_read_quat"):
        func = getattr(lib, name)
        func.argtypes = [ctypes.c_void_p, ctypes.c_uint32, ctypes.POINTER(ctypes.c_double)]
        func.restype = ctypes.c_int32
    lib.wcp_write_f64.argtypes = [ctypes.c_void_p, ctypes.c_uint32, ctypes.c_double]
    lib.wcp_write_f64.restype = ctypes.c_int32
    lib.wcp_write_vec3.argtypes = [
        ctypes.c_void_p,
        ctypes.c_uint32,
        ctypes.POINTER(ctypes.c_double),
    ]
    lib.wcp_write_vec3.restype = ctypes.c_int32
    lib.wcp_invalidate.argtypes = [ctypes.c_void_p, ctypes.c_uint32, ctypes.c_uint8]
    lib.wcp_invalidate.restype = ctypes.c_int32
    lib.wcp_commit.argtypes = [ctypes.c_void_p]
    lib.wcp_commit.restype = ctypes.c_int32
    lib.wcp_last_error.argtypes = []
    lib.wcp_last_error.restype = ctypes.c_char_p
    return lib


def _hello_json(hello: Union[str, Mapping[str, Any], Any]) -> bytes:
    if isinstance(hello, str):
        return hello.encode("utf-8")
    if isinstance(hello, Mapping):
        return json.dumps(hello).encode("utf-8")
    # Allow dataclasses / simple objects with the protocol field names.
    try:
        return json.dumps(vars(hello)).encode("utf-8")
    except TypeError as exc:  # pragma: no cover - defensive
        raise TypeError("hello must be a JSON string, mapping or plain object") from exc


class ModelClient:
    """A connected model instance (single-threaded, like the Rust type)."""

    _lib: Optional[ctypes.CDLL] = None

    def __init__(self, handle: int, lib: ctypes.CDLL) -> None:
        self._handle = handle
        self._lib = lib
        self._manifest: Optional[dict] = None
        self._closed = False

    # ------------------------------------------------------------------ construction

    @classmethod
    def connect(
        cls,
        sim_id: int,
        instance_id: int,
        hello: Union[str, Mapping[str, Any], Any],
        timeout: float = 5.0,
    ) -> "ModelClient":
        """Connect to ``(sim_id, instance_id)``, send the hello and wait for ``Ready``.

        ``timeout`` is the overall deadline in seconds (open + manifest + handshake).
        """
        lib = cls._lib if cls._lib is not None else load_library()
        cls._lib = lib
        c_handle = ctypes.c_void_p()
        rc = lib.wcp_connect(
            int(sim_id),
            int(instance_id),
            _hello_json(hello),
            _timeout_ms(timeout),
            ctypes.byref(c_handle),
        )
        if rc < 0:
            raise WcpError(_last_error(lib))
        if rc != WCP_OK or not c_handle.value:
            raise WcpError("wcp_connect returned success without a client handle")
        return cls(c_handle.value, lib)

    # ------------------------------------------------------------------ metadata

    @property
    def manifest(self) -> dict:
        """Manifest of the connected segment, decoded from the canonical JSON."""
        if self._manifest is None:
            raw = self._lib.wcp_manifest_json(self._handle)
            if not raw:
                raise WcpError(_last_error(self._lib))
            self._manifest = json.loads(raw.decode("utf-8"))
        return self._manifest

    def slot(self, name: str) -> Optional[int]:
        """Resolve a slot name; ``None`` for unknown or ``enabled=false`` slots."""
        out = ctypes.c_uint32()
        rc = self._lib.wcp_slot_by_name(self._handle, name.encode("utf-8"), ctypes.byref(out))
        if rc == WCP_OK:
            return out.value
        if rc == WCP_TIMEOUT:
            return None
        raise WcpError(_last_error(self._lib))

    # ------------------------------------------------------------------ stepping

    def wait_step(self, timeout: float = 1.0) -> Optional[Union[Step, Reset]]:
        """Wait for the next event; ``None`` on timeout.

        Spurious wakeups are ignored. A reset is returned as :class:`Reset`; the caller
        must reinitialise its internal state, call :meth:`ack_reset`, and must not commit.
        Raises :class:`Closed` on a clean adapter shutdown.
        """
        raw = _WcpStep()
        rc = self._lib.wcp_wait_step(self._handle, _timeout_ms(timeout), ctypes.byref(raw))
        if rc == WCP_TIMEOUT:
            return None
        if rc == WCP_RESET:
            return Reset(epoch=int(raw.epoch))
        if rc == WCP_CLOSED:
            raise Closed("segment closed")
        if rc < 0:
            raise WcpError(_last_error(self._lib))
        return Step(
            step_id=int(raw.step_id),
            sim_time_ns=int(raw.sim_time_ns),
            dt_ns=int(raw.dt_ns),
            epoch=int(raw.epoch),
            epoch_changed=bool(raw.epoch_changed),
        )

    def ack_reset(self) -> None:
        """Acknowledge a :class:`Reset` after reinitialising the model's internal state.

        Raises :class:`WcpError` when no reset is pending.
        """
        if self._lib.wcp_ack_reset(self._handle) < 0:
            raise WcpError(_last_error(self._lib))

    def commit(self) -> None:
        """Publish the pending step's effort (heartbeat, ``effort_step_id``, m2s signal)."""
        if self._lib.wcp_commit(self._handle) < 0:
            raise WcpError(_last_error(self._lib))

    # ------------------------------------------------------------------ reads

    def read_f64(self, slot: int) -> Optional[float]:
        """Read an F64 slot; ``None`` when the validity is not ``Valid``."""
        out = ctypes.c_double()
        rc = self._lib.wcp_read_f64(self._handle, slot, ctypes.byref(out))
        if rc == WCP_TIMEOUT:
            return None
        if rc < 0:
            raise WcpError(_last_error(self._lib))
        return float(out.value)

    def read_vec3(self, slot: int) -> Optional[Tuple[float, float, float]]:
        out = (ctypes.c_double * 3)()
        rc = self._lib.wcp_read_vec3(self._handle, slot, out)
        if rc == WCP_TIMEOUT:
            return None
        if rc < 0:
            raise WcpError(_last_error(self._lib))
        return (float(out[0]), float(out[1]), float(out[2]))

    def read_quat(self, slot: int) -> Optional[Tuple[float, float, float, float]]:
        out = (ctypes.c_double * 4)()
        rc = self._lib.wcp_read_quat(self._handle, slot, out)
        if rc == WCP_TIMEOUT:
            return None
        if rc < 0:
            raise WcpError(_last_error(self._lib))
        return (float(out[0]), float(out[1]), float(out[2]), float(out[3]))

    # ------------------------------------------------------------------ writes

    def write_f64(self, slot: int, value: float) -> None:
        """Write a scalar effort (non-finite values mark the slot ``Invalid``).

        Raises :class:`WcpError` when ``slot`` is not a ToWebots F64 slot.
        """
        if self._lib.wcp_write_f64(self._handle, slot, float(value)) < 0:
            raise WcpError(_last_error(self._lib))

    def write_vec3(self, slot: int, value: Sequence[float]) -> None:
        """Write a Vec3 effort; non-finite components mark the slot ``Invalid``."""
        if len(value) != 3:
            raise ValueError("write_vec3 expects exactly three values")
        out = (ctypes.c_double * 3)(*(float(v) for v in value))
        if self._lib.wcp_write_vec3(self._handle, slot, out) < 0:
            raise WcpError(_last_error(self._lib))

    def invalidate(self, slot: int, validity: int = WCP_VALIDITY_ERROR) -> None:
        """Set an effort slot's validity byte (default: ``5`` = error)."""
        if self._lib.wcp_invalidate(self._handle, slot, int(validity)) < 0:
            raise WcpError(_last_error(self._lib))

    # ------------------------------------------------------------------ lifetime

    def close(self) -> None:
        """Release the native client. Idempotent."""
        if not self._closed:
            self._closed = True
            self._lib.wcp_close(self._handle)
            self._handle = 0

    def __enter__(self) -> "ModelClient":
        return self

    def __exit__(self, *_exc: Any) -> None:
        self.close()

    def __del__(self) -> None:  # pragma: no cover - best effort
        try:
            self.close()
        except Exception:
            pass


def _timeout_ms(timeout: float) -> int:
    return max(0, min(int(round(float(timeout) * 1000.0)), 0xFFFF_FFFF))


def _last_error(lib: ctypes.CDLL) -> str:
    raw = lib.wcp_last_error()
    if not raw:
        return "unknown native error"
    return raw.decode("utf-8", "replace")
