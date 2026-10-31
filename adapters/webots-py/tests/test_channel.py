import json
import os
import random
import sys
import tempfile
import time
import unittest

from janus_adapter import CouplingAdapter, CouplingError
from janus_adapter import protocol as P

from .fake_robot import FakeRobot
from .support import FakeHost, FakeModel, StopResponding, joint_manifest

WINDOWS = sys.platform == "win32"
VALID = P.VALIDITY["Valid"]


@unittest.skipUnless(WINDOWS, "shared-memory backend is Windows-only in v1")
class LockstepTests(unittest.TestCase):
    J, B, DT_NS = 0.01, 0.1, 1_000_000

    def setUp(self):
        self.sim_id = random.getrandbits(63) | 1
        self.tmp = tempfile.TemporaryDirectory()
        self.robot = FakeRobot()
        self.motor, self.sensor, _ = self.robot.add_hinge("JOINT")
        self.host = self.model = self.adapter = None
        self.theta = self.omega = 0.0
        self.t = 0
        self.logs = []

    def tearDown(self):
        if self.adapter is not None:
            try:
                self.adapter.stop()
            except Exception:
                pass
        if self.model is not None:
            self.model.stop()
            self.model.join(2)
        if self.host is not None:
            self.host.close()
        self.tmp.cleanup()

    # -- helpers --
    def boot(self, behavior, **manifest_kw):
        self.host = FakeHost(joint_manifest(self.sim_id, **manifest_kw))
        self.model = FakeModel(self.host, behavior)
        self.model.start()
        run = os.path.join(self.tmp.name, "run.json")
        with open(run, "w", encoding="utf-8") as f:
            json.dump({"sim_id": "%016x" % self.sim_id, "instances": [{"name": "motor1", "instance_id": 1}]}, f)
        self.adapter = CouplingAdapter(self.robot, run_file=run, log=self.logs.append)
        self.adapter.start(timeout_s=5.0)

    def step(self):
        """One Webots step: exchange, then a viscous rotary plant integrates the applied torque."""
        self.adapter.exchange(self.t, self.DT_NS)
        dt = self.DT_NS / 1e9
        self.omega += (self.motor.torque - self.B * self.omega) / self.J * dt
        self.theta += self.omega * dt
        self.sensor.value = self.theta
        self.t += self.DT_NS

    # -- tests --
    def test_closed_loop_one_exchange_per_step(self):
        self.boot(lambda n, s: {"joint_torque": 1.0})
        n = 1500
        for _ in range(n):
            self.step()
        self.assertEqual(self.model.processed, list(range(1, n + 1)))  # no loss, no duplicate, no reorder
        self.assertEqual(len(self.motor.torque_history), n)
        self.assertAlmostEqual(self.omega, 1.0 / self.B, delta=0.02 * (1.0 / self.B))
        self.assertEqual(self.host.hs, "Running")
        rep = self.host.report()
        self.assertTrue(rep["ok"], rep)
        self.assertEqual(rep["world_coordinate_system"], "ENU")

    def test_model_sees_position_and_velocity(self):
        seen = []
        self.boot(lambda n, s: (seen.append(dict(s)) or {"joint_torque": 0.5}))
        for _ in range(5):
            self.step()
        self.assertIsNone(seen[0]["joint_position"])  # sensor NaN on the first exchange -> Invalid
        self.assertIsNotNone(seen[2]["joint_velocity"])

    def test_over_limit_effort_is_clamped(self):
        self.boot(lambda n, s: {"joint_torque": 9.0})
        for _ in range(3):
            self.step()
        self.assertEqual(self.motor.torque, 5.0)

    def test_rate_limit_applies(self):
        self.boot(lambda n, s: {"joint_torque": 5.0}, limits={"max_abs": 5.0, "max_rate_per_s": 1000.0})
        for _ in range(4):
            self.step()
        self.assertEqual([round(x, 9) for x in self.motor.torque_history], [1.0, 2.0, 3.0, 4.0])

    def test_nan_effort_is_zeroed_then_recovers(self):
        def behavior(n, s):
            return {"joint_torque": float("nan") if n == 3 else 2.0}
        self.boot(behavior)
        for _ in range(5):
            self.step()
        self.assertEqual(self.motor.torque_history, [2.0, 2.0, 0.0, 2.0, 2.0])
        self.assertEqual(self.host.hs, "Running")

    def test_invalid_validity_is_zeroed(self):
        def behavior(n, s):
            return {"joint_torque": (P.VALIDITY["Invalid"], None) if n == 2 else 1.0}
        self.boot(behavior)
        for _ in range(3):
            self.step()
        self.assertEqual(self.motor.torque_history, [1.0, 0.0, 1.0])

    def test_model_hang_times_out_zeroes_and_goes_to_error_without_blocking(self):
        def behavior(n, s):
            if n > 10:
                raise StopResponding
            return {"joint_torque": 3.0}
        self.boot(behavior, timeout_ms=20, max_misses=3)
        for _ in range(10):
            self.step()
        self.assertEqual(self.motor.torque, 3.0)
        t0 = time.perf_counter()
        for _ in range(3):  # three misses at ~20 ms each
            self.step()
        self.assertEqual(self.motor.torque, 0.0)
        self.assertEqual(self.host.hs, "Error")
        t1 = time.perf_counter()
        for _ in range(200):  # Error state: no waiting any more
            self.step()
        self.assertLess(time.perf_counter() - t1, 1.0)
        self.assertTrue(set(self.motor.torque_history[10:]) == {0.0})
        self.assertLess(t1 - t0, 1.0)

    def test_stale_effort_step_id_is_not_applied(self):
        """A model that answers with an old step id (late / duplicate answer) must never be applied."""
        h = {}

        def behavior(n, s):
            h["n"] = n
            return {"joint_torque": 4.0}
        self.boot(behavior, timeout_ms=30, max_misses=100)
        self.step()
        self.step()
        applied_before = list(self.motor.torque_history)
        self.model.hung = True  # stops answering
        hdr = self.host.seg.header
        for _ in range(2):
            # forge: signal m2s without advancing effort_step_id
            self.host.m2s.signal()
            self.step()
        self.assertEqual(self.motor.torque_history[len(applied_before):], [0.0, 0.0])

    def test_reset_restarts_steps_and_model_acks_epoch(self):
        self.boot(lambda n, s: {"joint_torque": 1.0})
        for _ in range(20):
            self.step()
        self.adapter.reset()
        self.assertEqual(self.model.acked_epochs, [1])
        self.assertEqual(self.host.hs, "Ready")
        hdr = self.host.seg.header
        self.assertEqual(hdr.u64("step_id"), 0)
        self.theta = self.omega = 0.0
        self.t = 0
        for _ in range(5):
            self.step()
        self.assertEqual(self.model.processed, [1, 2, 3, 4, 5])
        self.assertEqual(self.host.hs, "Running")

    def test_pause_and_resume_do_not_break_stepping(self):
        self.boot(lambda n, s: {"joint_torque": 1.0})
        self.step()
        self.adapter.pause()
        self.assertEqual(self.host.hs, "Paused")
        self.adapter.resume()
        self.step()
        self.assertEqual(self.host.hs, "Running")
        self.assertEqual(self.model.processed, [1, 2])

    def test_stop_zeroes_motor_and_closes(self):
        self.boot(lambda n, s: {"joint_torque": 2.0})
        self.step()
        self.adapter.stop()
        self.assertEqual(self.motor.torque, 0.0)
        self.assertEqual(self.host.hs, "Closed")
        self.adapter = None

    def test_driven_entities(self):
        self.boot(lambda n, s: {"joint_torque": 0.0})
        self.assertEqual(self.adapter.driven_entities(), {"JOINT"})

    def test_binding_failure_writes_report_and_sets_error(self):
        self.host = FakeHost(joint_manifest(self.sim_id, def_name="MISSING"))
        run = os.path.join(self.tmp.name, "run.json")
        with open(run, "w", encoding="utf-8") as f:
            json.dump({"sim_id": "%016x" % self.sim_id, "instances": [{"name": "motor1", "instance_id": 1}]}, f)
        adapter = CouplingAdapter(self.robot, run_file=run, log=self.logs.append)
        with self.assertRaises(CouplingError) as cm:
            adapter.start(timeout_s=5.0)
        self.assertIn("MISSING", str(cm.exception))
        rep = self.host.report()
        self.assertFalse(rep["ok"])
        self.assertEqual(self.host.hs, "Error")

    def test_attach_times_out_when_host_is_absent(self):
        run = os.path.join(self.tmp.name, "run.json")
        with open(run, "w", encoding="utf-8") as f:
            json.dump({"sim_id": "%016x" % self.sim_id, "instances": [{"name": "x", "instance_id": 7}]}, f)
        adapter = CouplingAdapter(self.robot, run_file=run, log=self.logs.append)
        with self.assertRaises(CouplingError):
            adapter.start(timeout_s=0.3)

    def test_exchange_latency_report(self):
        self.boot(lambda n, s: {"joint_torque": 1.0})
        for _ in range(500):
            self.step()
        s = self.adapter.stats()
        print("\n[perf] exchange n=%d p50=%.0fus p99=%.0fus max=%.0fus" % (s["n"], s["p50_us"], s["p99_us"], s["max_us"]))
        self.assertEqual(s["n"], 500)


if __name__ == "__main__":
    unittest.main()
