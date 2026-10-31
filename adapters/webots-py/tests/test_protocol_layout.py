import json
import unittest
from pathlib import Path

from janus_adapter import layout as L
from janus_adapter import protocol as P

HERE = Path(__file__).resolve().parent

GOLDEN = [
    (0x1234ABCD5678EF90, 1, "Local\\wcp.1234abcd5678ef90.1", "/wcp.1234abcd5678ef90.1"),
    (0x1, 12, "Local\\wcp.0000000000000001.12", "/wcp.0000000000000001.12"),
]


class ProtocolTests(unittest.TestCase):
    def test_golden_names(self):
        for sim, inst, win, lin in GOLDEN:
            w = P.segment_names(sim, inst, "windows")
            self.assertEqual(w["segment"], win)
            self.assertEqual(w["s2m"], win + ".s2m")
            self.assertEqual(w["m2s"], win + ".m2s")
            self.assertEqual(P.segment_names(sim, inst, "linux")["segment"], lin)

    def test_names_match_shared_vector_if_present(self):
        vec = HERE.parent.parent.parent / "tests" / "vectors" / "names.json"
        if not vec.is_file():
            self.skipTest("tests/vectors/names.json not present")
        for case in json.loads(vec.read_text(encoding="utf-8-sig"))["cases"]:
            sim, inst = int(case["sim_id"], 16), case["instance_id"]
            self.assertEqual(P.segment_names(sim, inst, "windows"), case["windows"])
            self.assertEqual(P.segment_names(sim, inst, "linux"), case["linux"])

    def test_generated_constants_are_sane(self):
        self.assertEqual(P.HEADER_SIZE, 256)
        self.assertEqual(P.MAGIC, b"WCOUPLE1")
        self.assertEqual(P.OFF["step_id"], 64)
        self.assertEqual(P.HS["Ready"], 2)
        self.assertEqual(P.VALIDITY["Missing"], 0)
        self.assertEqual(P.VALUE_SIZE["vec3f64"], 24)
        for name in ("joint_position", "joint_torque", "body_force"):
            self.assertIn(name, P.SIGNALS_BY_NAME)


class LayoutTests(unittest.TestCase):
    def setUp(self):
        v = json.loads((HERE / "fixtures" / "layout_vector.json").read_text(encoding="utf-8"))
        self.vec = v
        self.slots = [
            L.Slot(slot_id=s["slot_id"], entity_id=s["entity_id"], signal_id=s["signal_id"], name="s%d" % s["slot_id"],
                   value_type=s["value_type"], direction=s["direction"], frame="none", unit="", required=False,
                   enabled=True, limits=None, point=None)
            for s in v["specs"]
        ]

    def test_build_layout_matches_rust_reference(self):
        c, e = self.vec["caps"], self.vec["expected"]
        lay = L.build_layout(self.slots, c["manifest"], c["hello"], c["report"])
        for key in ("manifest_off", "hello_off", "report_off", "state_off", "state_len", "effort_off", "effort_len", "total_size"):
            self.assertEqual(lay[key], e[key], key)
        got = {s.slot_id: (s.validity_offset, s.value_offset) for s in lay["slots"]}
        for exp in e["slots"]:
            self.assertEqual(got[exp["slot_id"]], (exp["validity_offset"], exp["value_offset"]), exp["slot_id"])

    def test_place_from_header_matches_build(self):
        c = self.vec["caps"]
        lay = L.build_layout(self.slots, c["manifest"], c["hello"], c["report"])
        buf = bytearray(P.HEADER_SIZE)
        h = P.Header(buf)
        for key in ("state", "effort"):
            h.set_u64(key + "_off", lay[key + "_off"])
            P.U64.pack_into(buf, P.OFF[key + "_off"] + 8, lay[key + "_len"])
        placed = L.place_from_header(self.slots, h)
        for a, b in zip(placed, lay["slots"]):
            self.assertEqual((a.validity_offset, a.value_offset), (b.validity_offset, b.value_offset))

    def test_place_from_header_rejects_mismatch(self):
        buf = bytearray(P.HEADER_SIZE)
        h = P.Header(buf)
        h.set_u64("state_off", 1000)
        P.U64.pack_into(buf, P.OFF["state_off"] + 8, 8)  # wrong length
        with self.assertRaises(P.ProtocolError):
            L.place_from_header(self.slots, h)


if __name__ == "__main__":
    unittest.main()
