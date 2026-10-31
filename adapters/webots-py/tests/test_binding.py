import math
import unittest

from janus_adapter import layout as L
from janus_adapter.channel import CouplingError
from janus_adapter.webots_binding import EffortSink, InstanceBinding, read_coordinate_system

from .fake_robot import FakeRobot
from .support import SIG, joint_manifest, slot_dict


class FakeChannel:
    """Records what the binding writes, standing in for InstanceChannel."""

    def __init__(self):
        self.values, self.invalid = {}, set()

    def set_value(self, slot, v):
        self.values[slot.slot_id] = v
        self.invalid.discard(slot.slot_id)

    def set_invalid(self, slot, why="Invalid"):
        self.invalid.add(slot.slot_id)


def make(robot, manifest, cs="ENU", sink=None):
    slots = L.slots_from_manifest(manifest)
    sink = sink or EffortSink()
    return InstanceBinding(robot, manifest, slots, cs, sink), sink


def body_manifest(slots, kind="solid", def_name="BODY"):
    m = joint_manifest(0x1)
    m["entities"] = [{"entity_id": 1, "name": "body", "webots_def": def_name, "kind": kind}]
    m["slots"] = slots
    return m


class JointBindingTests(unittest.TestCase):
    def setUp(self):
        self.robot = FakeRobot()
        self.motor, self.sensor, _ = self.robot.add_hinge("JOINT")

    def test_resolve_ok_and_devices_discovered(self):
        b, sink = make(self.robot, joint_manifest(0x1))
        rep = b.resolve()
        self.assertTrue(rep["ok"], rep["errors"])
        self.assertEqual(self.sensor.enabled, 1)
        self.assertEqual(rep["entities"][0]["node_type"], "HingeJoint")
        self.assertTrue(any("finite difference" in w for w in rep["warnings"]))
        self.assertEqual(b.driven_defs(), {"JOINT"})

    def test_missing_def_is_reported(self):
        b, _ = make(self.robot, joint_manifest(0x1, def_name="NOPE"))
        rep = b.resolve()
        self.assertFalse(rep["ok"])
        self.assertIn("NOPE", rep["errors"][0])

    def test_unit_must_match_joint_type(self):
        b, _ = make(self.robot, joint_manifest(0x1, unit_pos="m", unit_vel="m/s"))
        rep = b.resolve()
        self.assertFalse(rep["ok"])
        self.assertTrue(any("unit" in e for e in rep["errors"]))

    def test_joint_torque_on_slider_is_rejected(self):
        self.robot.add_hinge("SLIDER", motor="m2", sensor="s2", joint_type="SliderJoint")
        b, _ = make(self.robot, joint_manifest(0x1, def_name="SLIDER", unit_pos="m", unit_vel="m/s"))
        rep = b.resolve()
        self.assertTrue(any("joint_torque needs a HingeJoint" in e for e in rep["errors"]))

    def test_slider_with_joint_force_ok(self):
        motor, _, _ = self.robot.add_hinge("SLIDER", motor="m2", sensor="s2", joint_type="SliderJoint")
        m = joint_manifest(0x1, def_name="SLIDER", unit_pos="m", unit_vel="m/s")
        m["slots"][2] = slot_dict(2, 1, "joint_force", "to_webots", "N", limits={"max_abs": 10.0})
        b, sink = make(self.robot, m)
        self.assertTrue(b.resolve()["ok"])
        b.apply_efforts({2: (3.0,)})
        sink.flush()
        self.assertEqual(motor.torque, 3.0)

    def test_unsupported_joint_type(self):
        self.robot.add_hinge("H2", motor="m3", sensor="s3", joint_type="Hinge2Joint")
        b, _ = make(self.robot, joint_manifest(0x1, def_name="H2"))
        self.assertFalse(b.resolve()["ok"])

    def test_motor_name_override(self):
        other = self.robot.add_hinge("J2", motor="mA", sensor="sA")[0]
        m = joint_manifest(0x1, def_name="J2")
        m["entities"][0]["motor"] = "mA"
        b, sink = make(self.robot, m)
        self.assertTrue(b.resolve()["ok"])
        b.apply_efforts({2: (1.0,)})
        sink.flush()
        self.assertEqual(other.torque, 1.0)

    def test_state_reading_and_finite_difference_velocity(self):
        b, _ = make(self.robot, joint_manifest(0x1))
        b.resolve()
        ch = FakeChannel()
        b.sample(0)  # sensor still NaN like Webots before the first step
        b.read_states(ch)
        self.assertEqual(ch.invalid, {0, 1})
        self.sensor.value = 0.0
        b.sample(1_000_000)
        b.read_states(ch)
        self.assertEqual(ch.values[0], 0.0)
        self.assertIn(1, ch.invalid)  # no previous sample yet
        self.sensor.value = 0.002
        b.sample(2_000_000)
        b.read_states(ch)
        self.assertAlmostEqual(ch.values[1], 2.0)  # 0.002 rad / 1 ms
        b.reset()
        b.sample(3_000_000)
        b.read_states(ch)
        self.assertIn(1, ch.invalid)

    def test_motor_written_every_step_even_when_zero(self):
        b, sink = make(self.robot, joint_manifest(0x1))
        b.resolve()
        for v in (1.0, 0.0):
            sink.begin()
            b.apply_efforts({2: (v,)})
            sink.flush()
        self.assertEqual(self.motor.torque_history, [1.0, 0.0])

    def test_two_instances_sum_on_one_motor(self):
        sink = EffortSink()
        b1, _ = make(self.robot, joint_manifest(0x1, instance_id=1), sink=sink)
        b2, _ = make(self.robot, joint_manifest(0x1, instance_id=2), sink=sink)
        b1.resolve(), b2.resolve()
        sink.begin()
        b1.apply_efforts({2: (1.5,)})
        b2.apply_efforts({2: (2.0,)})
        sink.flush()
        self.assertEqual(self.motor.torque, 3.5)


class BodyBindingTests(unittest.TestCase):
    def test_state_in_enu_from_nue_world(self):
        robot = FakeRobot("NUE")
        node = robot.add_solid("BODY", position=(1.0, 2.0, 3.0), velocity=[0.1, 0.2, 0.3, 0.4, 0.5, 0.6])
        m = body_manifest([
            slot_dict(0, 1, "body_position", "to_model", "m", frame="world", value_type="vec3f64"),
            slot_dict(1, 1, "body_linear_velocity", "to_model", "m/s", frame="world", value_type="vec3f64"),
            slot_dict(2, 1, "body_orientation", "to_model", "1", frame="world", value_type="quatf64"),
        ])
        b, _ = make(robot, m, cs="NUE")
        self.assertTrue(b.resolve()["ok"])
        ch = FakeChannel()
        b.sample(0)
        b.read_states(ch)
        self.assertEqual(ch.values[0], (3.0, 1.0, 2.0))
        self.assertEqual(ch.values[1], (0.3, 0.1, 0.2))
        self.assertEqual(ch.values[2], (0.0, 0.0, 0.0, 1.0))

    def test_body_state_needs_world_frame(self):
        robot = FakeRobot()
        robot.add_solid("BODY")
        m = body_manifest([slot_dict(0, 1, "body_position", "to_model", "m", frame="body_local", value_type="vec3f64")])
        b, _ = make(robot, m)
        self.assertFalse(b.resolve()["ok"])

    def test_force_world_frame_converted_back_to_webots_axes(self):
        robot = FakeRobot("NUE")
        node = robot.add_solid("BODY")
        m = body_manifest([slot_dict(0, 1, "body_force", "to_webots", "N", frame="world", value_type="vec3f64",
                                     limits={"max_abs": 10.0})])
        b, sink = make(robot, m, cs="NUE")
        self.assertTrue(b.resolve()["ok"])
        sink.begin()
        b.apply_efforts({0: (1.0, 2.0, 3.0)})  # ENU: E=1, N=2, U=3
        sink.flush()
        self.assertEqual(node.forces, [((2.0, 3.0, 1.0), False, None)])  # NUE: x=N, y=U, z=E

    def test_force_body_local_with_point(self):
        robot = FakeRobot()
        node = robot.add_solid("BODY")
        m = body_manifest([slot_dict(0, 1, "body_force", "to_webots", "N", frame="body_local", value_type="vec3f64",
                                     limits={"max_abs": 10.0}, point=[0.1, 0.0, 0.0])])
        b, sink = make(robot, m)
        self.assertTrue(b.resolve()["ok"])
        sink.begin()
        b.apply_efforts({0: (0.0, 0.0, 5.0)})
        sink.flush()
        self.assertEqual(node.forces, [((0.0, 0.0, 5.0), True, (0.1, 0.0, 0.0))])

    def test_point_requires_body_local(self):
        robot = FakeRobot()
        robot.add_solid("BODY")
        m = body_manifest([slot_dict(0, 1, "body_force", "to_webots", "N", frame="world", value_type="vec3f64",
                                     limits={"max_abs": 10.0}, point=[0.1, 0.0, 0.0])])
        self.assertFalse(make(robot, m)[0].resolve()["ok"])

    def test_joint_signal_on_solid_is_rejected(self):
        robot = FakeRobot()
        robot.add_solid("BODY")
        m = body_manifest([slot_dict(0, 1, "joint_position", "to_model", "rad")])
        self.assertFalse(make(robot, m)[0].resolve()["ok"])

    def test_eun_world_is_rejected(self):
        robot = FakeRobot("EUN")
        robot.add_solid("BODY")
        m = body_manifest([slot_dict(0, 1, "body_position", "to_model", "m", frame="world", value_type="vec3f64")])
        self.assertFalse(make(robot, m, cs="EUN")[0].resolve()["ok"])


class CoordinateSystemTests(unittest.TestCase):
    def test_reads_field(self):
        self.assertEqual(read_coordinate_system(FakeRobot("NUE"), []), "NUE")

    def test_legacy_world_without_field_is_nue(self):
        w = []
        self.assertEqual(read_coordinate_system(FakeRobot(None), w), "NUE")
        self.assertTrue(w)


if __name__ == "__main__":
    unittest.main()
