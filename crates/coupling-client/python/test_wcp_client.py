"""pytest smoke test of the Python binding (Block B4 acceptance criterion).

The native cdylib and the ``mock_host`` example are built with cargo; the host runs
as a separate process so the binding is exercised over the real C ABI.
"""

from __future__ import annotations

import os
import secrets
import subprocess
import sys
from pathlib import Path

import pytest

PYDIR = Path(__file__).resolve().parent
CRATE = PYDIR.parent
WORKSPACE = CRATE.parents[1]
DEBUG = WORKSPACE / "target" / "debug"

LIB = DEBUG / {
    "nt": "coupling_client.dll",
    "posix": "libcoupling_client.so",
}.get(os.name, "libcoupling_client.dylib")
HOST = DEBUG / "examples" / ("mock_host.exe" if os.name == "nt" else "mock_host")

HELLO = {
    "protocol": [1, 0],
    "model_name": "python-smoke",
    "model_version": "0.1.0",
    "steppable": True,
    "sync_modes": ["lockstep"],
    "internal_dt_ns": 100_000,
    "supported_signals": [1, 257],
    "custom_signals": [],
}


@pytest.fixture(scope="session")
def wcp() -> "object":
    """Build the crate and import the binding against the fresh library."""
    subprocess.run(
        ["cargo", "build", "-p", "coupling-client"],
        cwd=WORKSPACE,
        check=True,
    )
    subprocess.run(
        ["cargo", "build", "-p", "coupling-client", "--example", "mock_host"],
        cwd=WORKSPACE,
        check=True,
    )
    assert LIB.is_file(), f"missing native library {LIB}"
    assert HOST.is_file(), f"missing mock host {HOST}"
    os.environ["WCP_CLIENT_LIB"] = str(LIB)
    sys.path.insert(0, str(PYDIR))
    import wcp_client  # noqa: PLC0415 - must follow the environment setup

    return wcp_client


def test_lockstep_roundtrip(wcp) -> None:
    steps = 5
    sim_id = secrets.randbits(63) | 1
    host = subprocess.Popen(
        [str(HOST), str(sim_id), "1", str(steps)],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        with wcp.ModelClient.connect(sim_id, 1, HELLO, timeout=15.0) as client:
            manifest = client.manifest
            assert manifest["instance_id"] == 1
            assert manifest["sync_mode"] == "lockstep"
            assert client.slot("does_not_exist") is None

            position = client.slot("joint_position")
            torque = client.slot("joint_torque")
            assert position is not None
            assert torque is not None

            # Misuse is reported as an error (F2 review), not silently ignored.
            with pytest.raises(wcp.WcpError):
                client.ack_reset()
            with pytest.raises(wcp.WcpError):
                client.write_f64(position, 1.0)

            for n in range(1, steps + 1):
                step = client.wait_step(15.0)
                assert step is not None, f"timed out at step {n}"
                assert step.step_id == n
                assert step.dt_ns > 0
                p = client.read_f64(position)
                assert p == pytest.approx(n * 0.5)
                client.write_f64(torque, 2.0 * p)
                client.commit()

        out, err = host.communicate(timeout=15)
        assert host.returncode == 0, f"mock_host failed: {err}"
        assert "HOST_DONE" in out
    finally:
        if host.poll() is None:
            host.kill()
            host.wait(timeout=5)


def test_connect_to_missing_segment_raises(wcp) -> None:
    with pytest.raises(wcp.WcpError):
        wcp.ModelClient.connect(0xDEAD_BEEF_0000_0001, 7, HELLO, timeout=0.2)


def test_wait_step_maps_closed(wcp) -> None:
    """A clean shutdown (``WCP_CLOSED``) raises the dedicated ``Closed`` error."""
    assert wcp.WCP_CLOSED == 3
    assert issubclass(wcp.Closed, wcp.WcpError)

    class FakeLib:
        @staticmethod
        def wcp_wait_step(handle, timeout_ms, out):
            return wcp.WCP_CLOSED

        @staticmethod
        def wcp_last_error():
            return b""

    client = wcp.ModelClient(1, FakeLib())
    with pytest.raises(wcp.Closed):
        client.wait_step(0.01)
