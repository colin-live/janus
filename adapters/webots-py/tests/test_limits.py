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

    def test_bad_limits_are_unusable_with_error_status(self):
        for args in ((None, None), (float("nan"), None), (-1.0, None), (5.0, float("inf")), (5.0, -2.0)):
            f = EffortFilter(1, *args)
            self.assertEqual(f.process((1.0,), 0.001), ((0.0,), "Error", False), args)


class ProcessorTests(unittest.TestCase):
    def make(self, misses=3, required=True):
        return EffortProcessor([slot(2, limits={"max_abs": 5.0}, required=required)], misses)

    def test_good_step(self):
        p = self.make()
        r = p.process({2: (VALID, (1.5,))}, 1_000_000)
        self.assertEqual((r.applied[2], r.missed, r.error), ((1.5,), False, False))

    def test_missing_limits_fail_safe_with_error_status(self):
        p = EffortProcessor([slot(2, limits=None)], 3)
        r = p.process({2: (VALID, (1.0,))}, 1_000_000)
        self.assertEqual((r.applied[2], r.status[2], r.missed), ((0.0,), "Error", True))

    def test_step_level_failure_reports_error_on_every_slot(self):
        p = EffortProcessor([slot(1, limits={"max_abs": 5.0}), slot(2, limits={"max_abs": 5.0})], 3)
        r = p.process(None, 1_000_000, "timeout")
        self.assertEqual(r.status, {1: "Error", 2: "Error"})
        self.assertEqual(r.applied, {1: (0.0,), 2: (0.0,)})

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

    def test_required_slot_unusable_silences_the_whole_instance(self):
        p = EffortProcessor([slot(1, limits={"max_abs": 5.0}), slot(2, limits={"max_abs": 5.0})], 3)
        r = p.process({1: (VALID, (3.0,)), 2: (INVALID, None)}, 1_000_000)
        self.assertTrue(r.missed)
        self.assertEqual(r.applied, {1: (0.0,), 2: (0.0,)})
        self.assertEqual(r.status, {1: "Error", 2: "Invalid"})  # usable slot -> Error, culprit keeps its reason
        self.assertEqual(p.filters[1].last, [0.0])

    def test_non_required_slot_does_not_count_as_miss_or_silence_others(self):
        p = EffortProcessor([slot(1, limits={"max_abs": 5.0}, required=True),
                             slot(2, limits={"max_abs": 5.0}, required=False)], 3)
        r = p.process({1: (VALID, (3.0,)), 2: (MISSING, None)}, 1_000_000)
        self.assertEqual((r.missed, r.applied, r.status), (False, {1: (3.0,), 2: (0.0,)}, {1: "Valid", 2: "Missing"}))

    def test_non_applicable_types_are_ignored(self):
        p = EffortProcessor([slot(9, vt="mat3f64", limits={"max_abs": 1.0})], 3)
        self.assertEqual(p.filters, {})


def parse_number(x):
    return float(x) if isinstance(x, str) else x  # "nan" / "inf" / "-inf" inject non-finite values


class SharedVectorTests(unittest.TestCase):
    """Runs tests/vectors/effort_validation.json (schema v1, owned by B9, produced by B6)."""

    def test_vectors(self):
        if not VECTORS.is_file():
            self.skipTest("effort_validation.json not present")
        doc = json.loads(VECTORS.read_text(encoding="utf-8-sig"))
        self.assertEqual(doc["schema_version"], 1)
        names = set()
        for case in doc["cases"]:
            names.add(case["name"])
            vt = case.get("value_type", "f64")
            lim = case.get("limits")
            limits = None if lim is None else {k: v for k, v in lim.items() if v is not None}
            p = EffortProcessor([slot(0, vt=vt, limits=limits, required=case.get("required", True))],
                                case.get("max_consecutive_misses", 3))
            for i, step in enumerate(case["steps"]):
                msg = "%s step %d" % (case["name"], i)
                if not step.get("respond", True) or not step.get("effort_step_id_ok", True):
                    r = p.process(None, case["dt_ns"], "step-level failure")
                    timed_out = True
                else:
                    effort = step.get("effort", 0.0)
                    values = tuple(parse_number(x) for x in effort) if isinstance(effort, list) else (parse_number(effort),)
                    validity = P.VALIDITY[step.get("validity", "valid").title().replace("_", "")]
                    r = p.process({0: (validity, values)}, case["dt_ns"])
                    timed_out = False
                exp = step["expect"]
                got = r.applied[0]
                want = tuple(exp["value"]) if isinstance(exp["value"], list) else (exp["value"],)
                self.assertEqual(len(got), len(want), msg)
                for a, b in zip(got, want):
                    self.assertTrue(math.isclose(a, b, abs_tol=1e-12), "%s: %r != %r" % (msg, got, want))
                self.assertEqual(snake(r.status[0]), exp["status"], msg)
                if "model_timed_out" in exp:
                    self.assertEqual(timed_out, exp["model_timed_out"], msg)
                if "hs_state" in exp:
                    self.assertEqual("error" if r.error else "running", exp["hs_state"], msg)
                else:
                    self.assertFalse(r.error, msg)
        self.assertGreaterEqual(len(names), 19)


if __name__ == "__main__":
    unittest.main()
