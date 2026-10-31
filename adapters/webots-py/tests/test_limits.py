import json
import math
import re
import unittest
from pathlib import Path

from janus_adapter import protocol as P
from janus_adapter.layout import Slot
from janus_adapter.limits import EffortFilter, EffortProcessor

VECTORS = Path(__file__).resolve().parents[3] / "tests" / "vectors" / "effort_validation.json"


def slot(sid, vt="f64", limits=None, required=True):
    return Slot(slot_id=sid, entity_id=1, signal_id=0x0101, name="t%d" % sid, value_type=vt, direction="to_webots",
                frame="none", unit="N*m", required=required, enabled=True, limits=limits, point=None)


def snake(name):
    return re.sub(r"(?<!^)(?=[A-Z])", "_", name).lower()


VALID, MISSING, INVALID = P.VALIDITY["Valid"], P.VALIDITY["Missing"], P.VALIDITY["Invalid"]


class FilterTests(unittest.TestCase):
    def test_clamp(self):
        f = EffortFilter(1, 5.0, None)
        self.assertEqual(f.process((7.0,), 0.001), ((5.0,), "OutOfRange", True))
        self.assertEqual(f.process((-9.0,), 0.001), ((-5.0,), "OutOfRange", True))
        self.assertEqual(f.process((3.0,), 0.001), ((3.0,), "Valid", True))

    def test_rate_limit(self):
        f = EffortFilter(1, 10.0, 1000.0)  # 1 N*m per 1 ms
        self.assertEqual(f.process((0.0,), 0.001)[0], (0.0,))
        self.assertEqual(f.process((5.0,), 0.001), ((1.0,), "Valid", True))
        self.assertEqual(f.process((5.0,), 0.001)[0], (2.0,))
        self.assertEqual(f.process((-5.0,), 0.001)[0], (1.0,))

    def test_non_finite_is_zero_and_unusable(self):
        f = EffortFilter(1, 5.0, None)
        f.process((4.0,), 0.001)
        for bad in (float("nan"), float("inf"), float("-inf")):
            out, status, usable = f.process((bad,), 0.001)
            self.assertEqual((out, status, usable), ((0.0,), "Invalid", False))
        self.assertEqual(f.last, [0.0])

    def test_safety_zeroing_is_not_rate_limited(self):
        f = EffortFilter(1, 10.0, 100.0)
        for _ in range(100):
            f.process((10.0,), 0.001)
        self.assertGreater(f.last[0], 1.0)
        self.assertEqual(f.process(None, 0.001)[0], (0.0,))

    def test_vec3_per_component(self):
        f = EffortFilter(3, 2.0, None)
        out, status, _ = f.process((1.0, 3.0, -4.0), 0.001)
        self.assertEqual((out, status), ((1.0, 2.0, -2.0), "OutOfRange"))
        self.assertEqual(f.process((1.0, float("nan"), 0.0), 0.001)[2], False)


class ProcessorTests(unittest.TestCase):
    def make(self, misses=3, required=True):
        return EffortProcessor([slot(2, limits={"max_abs": 5.0}, required=required)], misses)

    def test_good_step(self):
        p = self.make()
        r = p.process({2: (VALID, (1.5,))}, 1_000_000)
        self.assertEqual((r.applied[2], r.missed, r.error), ((1.5,), False, False))

    def test_missing_limits_fail_safe(self):
        p = EffortProcessor([slot(2, limits=None)], 3)
        self.assertEqual(p.process({2: (VALID, (1.0,))}, 1_000_000).applied[2], (0.0,))

    def test_consecutive_misses_lead_to_error_and_good_step_resets(self):
        p = self.make(misses=3)
        self.assertFalse(p.process(None, 1_000_000, "timeout").error)
        self.assertFalse(p.process({2: (INVALID, None)}, 1_000_000).error)
        self.assertEqual(p.misses, 2)
        self.assertFalse(p.process({2: (VALID, (1.0,))}, 1_000_000).missed)  # resets
        self.assertEqual(p.misses, 0)
        for _ in range(2):
            self.assertFalse(p.process(None, 1_000_000).error)
        r = p.process(None, 1_000_000)
        self.assertTrue(r.error)
        self.assertEqual(r.applied[2], (0.0,))

    def test_non_required_slot_does_not_count_as_miss(self):
        p = self.make(required=False)
        r = p.process({2: (MISSING, None)}, 1_000_000)
        self.assertEqual((r.applied[2], r.missed), ((0.0,), False))

    def test_non_applicable_types_are_ignored(self):
        p = EffortProcessor([slot(9, vt="mat3f64", limits={"max_abs": 1.0})], 3)
        self.assertEqual(p.filters, {})


class SharedVectorTests(unittest.TestCase):
    """Consumes tests/vectors/effort_validation.json (owned by B9) when it exists."""

    def test_vectors(self):
        if not VECTORS.is_file():
            self.skipTest("effort_validation.json not present yet")
        for case in json.loads(VECTORS.read_text(encoding="utf-8"))["cases"]:
            lim = case["limits"]
            p = EffortProcessor([slot(0, limits={k: v for k, v in lim.items() if v is not None})], 10_000)
            for i, step in enumerate(case["steps"]):
                if not step.get("effort_step_id_ok", True):
                    r = p.process(None, case["dt_ns"], "stale")
                else:
                    validity = {"valid": VALID, "missing": MISSING, "invalid": INVALID}.get(step.get("validity", "valid"))
                    r = p.process({0: (validity, (step["effort"],))}, case["dt_ns"])
                exp = step["expect"]
                msg = "%s step %d" % (case["name"], i)
                self.assertTrue(math.isclose(r.applied[0][0], exp["value"], abs_tol=1e-12), msg)
                self.assertEqual(snake(r.status[0]) if r.status[0] != "Stale" else "stale", exp["status"], msg)


if __name__ == "__main__":
    unittest.main()
