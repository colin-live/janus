"""Binding between a manifest (one model instance) and Webots nodes/devices.

``robot`` is duck-typed (the Webots ``Supervisor`` API subset used here), so everything is testable
without Webots through ``tests/fake_robot.py``.
"""

from __future__ import annotations

import math
from typing import Callable, Dict, List, Optional, Tuple

from . import frames
from . import protocol as P
from .layout import Slot

ADAPTER_NAME = "janus-webots-py 0.1.0"

# signal ids (names are the contract; ids come from the generated registry)
_SIG = {n: s["id"] for n, s in P.SIGNALS_BY_NAME.items()}
JOINT_STATE = {_SIG["joint_position"], _SIG["joint_velocity"], _SIG["joint_torque_feedback"]}
BODY_STATE = {_SIG["body_position"], _SIG["body_orientation"], _SIG["body_linear_velocity"], _SIG["body_angular_velocity"]}
JOINT_TORQUE, JOINT_FORCE = _SIG["joint_torque"], _SIG["joint_force"]
BODY_FORCE, BODY_TORQUE = _SIG["body_force"], _SIG["body_torque"]

JOINT_TYPES = {"HingeJoint": "rotational", "SliderJoint": "linear"}
UNITS = {"rotational": ("rad", "rad/s"), "linear": ("m", "m/s")}


class BindingError(RuntimeError):
    pass


def read_coordinate_system(robot, warnings: List[str]) -> str:
    """``WorldInfo.coordinateSystem``; worlds older than R2022b have no such field and are NUE."""
    root = robot.getRoot()
    children = root.getField("children")
    for i in range(children.getCount()):
        node = children.getMFNode(i)
        if node is not None and node.getTypeName() == "WorldInfo":
            f = node.getField("coordinateSystem")
            if f is None:
                warnings.append("WorldInfo has no coordinateSystem field: assuming legacy NUE")
                return "NUE"
            return f.getSFString()
    warnings.append("WorldInfo not found: assuming ENU")
    return "ENU"


class EffortSink:
    """Collects the effects of all instances for one step and flushes them once.

    ``setTorque`` is persistent in Webots, so every registered motor is written every step (0 by default).
    ``addForce`` family calls are single-step and additive in Webots, which equals the protocol's summation.
    """

    def __init__(self):
        self.motors: Dict[int, list] = {}  # id -> [motor, kind, sum]
        self.forces: List[tuple] = []
        self.torques: List[tuple] = []

    def register_motor(self, motor, kind: str) -> None:
        self.motors.setdefault(id(motor), [motor, kind, 0.0])

    def begin(self) -> None:
        for m in self.motors.values():
            m[2] = 0.0
        self.forces.clear()
        self.torques.clear()

    def add_joint(self, motor, value: float) -> None:
        self.motors[id(motor)][2] += value

    def flush(self) -> None:
        for motor, kind, total in self.motors.values():
            (motor.setTorque if kind == "rotational" else motor.setForce)(total)
        for node, vec, rel, point in self.forces:
            if point is None:
                node.addForce(list(vec), rel)
            else:
                node.addForceWithOffset(list(vec), list(point), rel)
        for node, vec, rel in self.torques:
            node.addTorque(list(vec), rel)

    def zero_motors(self) -> None:
        for motor, kind, _ in self.motors.values():
            (motor.setTorque if kind == "rotational" else motor.setForce)(0.0)


class _Joint:
    def __init__(self, node, kind: str, motor, sensor):
        self.node, self.kind, self.motor, self.sensor = node, kind, motor, sensor
        self.pos: Optional[float] = None
        self.vel: Optional[float] = None
        self._prev_pos: Optional[float] = None
        self._prev_t: Optional[int] = None

    def reset(self) -> None:
        self.pos = self.vel = self._prev_pos = self._prev_t = None

    def sample(self, t_ns: int) -> None:
        pos = self.sensor.getValue() if self.sensor is not None else float("nan")
        if not math.isfinite(pos):
            self.pos = self.vel = None
            self._prev_pos = self._prev_t = None
            return
        self.pos = pos
        if self._prev_pos is not None and t_ns > self._prev_t:
            self.vel = (pos - self._prev_pos) / ((t_ns - self._prev_t) / 1e9)
        else:
            self.vel = None
        self._prev_pos, self._prev_t = pos, t_ns


class InstanceBinding:
    def __init__(self, robot, manifest: dict, slots: List[Slot], coordinate_system: str, sink: EffortSink):
        self.robot, self.manifest, self.slots = robot, manifest, slots
        self.cs, self.sink = coordinate_system, sink
        self.entities = {e["entity_id"]: e for e in manifest["entities"]}
        self.nodes: Dict[int, object] = {}
        self.joints: Dict[int, _Joint] = {}
        self.readers: List[Tuple[Slot, Callable[[], object]]] = []
        self.writers: List[Tuple[Slot, Callable[[tuple], None]]] = []
        self.errors: List[str] = []
        self.warnings: List[str] = []
        self.entity_reports: List[dict] = []

    # ------------------------------------------------------------------ resolve
    def resolve(self) -> dict:
        ts = int(max(1, round(self.robot.getBasicTimeStep())))
        dt_ns = self.manifest.get("dt_ns", 0)
        if dt_ns and abs(dt_ns - ts * 1_000_000) > 1:
            self.warnings.append("manifest dt_ns=%d differs from basicTimeStep=%d ms" % (dt_ns, ts))
        try:
            frames.check_supported(self.cs)
        except ValueError as e:
            self.errors.append(str(e))
        for eid, ent in self.entities.items():
            self._resolve_entity(eid, ent, ts)
        for slot in self.slots:
            if slot.enabled and slot.entity_id in self.entities and self.nodes.get(slot.entity_id) is not None:
                self._bind_slot(slot, ts)
        if self.joints:
            self.warnings.append("joint_velocity is a finite difference of the position sensor (half-step lag)")
        return self.report()

    def report(self) -> dict:
        return {
            "ok": not self.errors,
            "adapter": ADAPTER_NAME,
            "world_coordinate_system": self.cs,
            "entities": self.entity_reports,
            "errors": list(self.errors),
            "warnings": list(self.warnings),
        }

    def _resolve_entity(self, eid: int, ent: dict, ts: int) -> None:
        rep = {"entity_id": eid, "resolved": False}
        self.entity_reports.append(rep)
        node = self.robot.getFromDef(ent["webots_def"])
        if node is None:
            self.errors.append("entity %r: DEF %r not found" % (ent["name"], ent["webots_def"]))
            rep["detail"] = "DEF not found"
            return
        tn = node.getTypeName()
        rep["node_type"] = tn
        self.nodes[eid] = node
        if ent["kind"] == "joint":
            kind = JOINT_TYPES.get(tn)
            if kind is None:
                self.errors.append("entity %r: node type %s is not a supported joint (HingeJoint/SliderJoint)" % (ent["name"], tn))
                return
            motor, sensor = self._discover(node, ent)
            self.joints[eid] = _Joint(node, kind, motor, sensor)
        elif tn.endswith("Joint"):
            self.errors.append("entity %r: kind is 'solid' but node is %s" % (ent["name"], tn))
            return
        rep["resolved"] = True

    def _discover(self, node, ent: dict):
        motor_name, sensor_name = ent.get("motor"), ent.get("sensor")
        if motor_name is None or sensor_name is None:
            dev = node.getField("device")
            for i in range(dev.getCount() if dev is not None else 0):
                item = dev.getMFNode(i)
                t = item.getTypeName()
                nm = item.getField("name").getSFString()
                if t in ("RotationalMotor", "LinearMotor") and motor_name is None:
                    motor_name = nm
                elif t == "PositionSensor" and sensor_name is None:
                    sensor_name = nm
        motor = self.robot.getDevice(motor_name) if motor_name else None
        sensor = self.robot.getDevice(sensor_name) if sensor_name else None
        return motor, sensor

    # ------------------------------------------------------------------ slots
    def _err(self, slot: Slot, msg: str) -> None:
        self.errors.append("slot %d (%s): %s" % (slot.slot_id, slot.name, msg))

    def _bind_slot(self, slot: Slot, ts: int) -> None:
        ent = self.entities[slot.entity_id]
        sid = slot.signal_id
        joint = self.joints.get(slot.entity_id)
        node = self.nodes[slot.entity_id]
        if sid >= 0x8000:  # custom signals are the model's business, never mapped to Webots
            if slot.direction == "to_model" and slot.required:
                self._err(slot, "custom to_model signals are not provided by this adapter")
            elif slot.direction == "to_model":
                self.warnings.append("custom to_model slot %d (%s) will always be Invalid" % (slot.slot_id, slot.name))
            return
        if ent["kind"] == "joint":
            if sid not in JOINT_STATE and sid not in (JOINT_TORQUE, JOINT_FORCE):
                return self._err(slot, "signal not valid on a joint entity")
            if joint is None:
                return  # entity resolution already failed
            self._bind_joint_slot(slot, joint, ts)
        else:
            if sid not in BODY_STATE and sid not in (BODY_FORCE, BODY_TORQUE):
                return self._err(slot, "signal not valid on a solid entity")
            self._bind_body_slot(slot, node)

    def _bind_joint_slot(self, slot: Slot, j: _Joint, ts: int) -> None:
        sid = slot.signal_id
        want_pos, want_vel = UNITS[j.kind]
        if sid == _SIG["joint_position"] and slot.unit != want_pos:
            return self._err(slot, "unit %r but the joint is %s (expected %r)" % (slot.unit, j.kind, want_pos))
        if sid == _SIG["joint_velocity"] and slot.unit != want_vel:
            return self._err(slot, "unit %r but the joint is %s (expected %r)" % (slot.unit, j.kind, want_vel))
        if sid == JOINT_TORQUE and j.kind != "rotational":
            return self._err(slot, "joint_torque needs a HingeJoint (use joint_force for SliderJoint)")
        if sid == JOINT_FORCE and j.kind != "linear":
            return self._err(slot, "joint_force needs a SliderJoint (use joint_torque for HingeJoint)")
        if sid in (_SIG["joint_position"], _SIG["joint_velocity"]):
            if j.sensor is None:
                return self._err(slot, "joint has no PositionSensor")
            j.sensor.enable(ts)
            if sid == _SIG["joint_position"]:
                self.readers.append((slot, lambda j=j: j.pos))
            else:
                self.readers.append((slot, lambda j=j: j.vel))
        elif sid == _SIG["joint_torque_feedback"]:
            if j.motor is None:
                return self._err(slot, "joint has no motor")
            j.motor.enableTorqueFeedback(ts) if j.kind == "rotational" else j.motor.enableForceFeedback(ts)
            get = j.motor.getTorqueFeedback if j.kind == "rotational" else j.motor.getForceFeedback
            self.readers.append((slot, get))
        else:
            if j.motor is None:
                return self._err(slot, "joint has no motor")
            self.sink.register_motor(j.motor, j.kind)
            self.writers.append((slot, lambda v, m=j.motor: self.sink.add_joint(m, v[0])))

    def _bind_body_slot(self, slot: Slot, node) -> None:
        sid, cs = slot.signal_id, self.cs
        if sid in BODY_STATE:
            if slot.frame != "world":
                return self._err(slot, "body state is only available in the 'world' frame")
            if sid == _SIG["body_position"]:
                fn = lambda: frames.vec_to_enu(node.getPosition(), cs)
            elif sid == _SIG["body_orientation"]:
                fn = lambda: frames.matrix_to_quat(frames.matrix_to_enu(node.getOrientation(), cs))
            elif sid == _SIG["body_linear_velocity"]:
                fn = lambda: frames.vec_to_enu(node.getVelocity()[0:3], cs)
            else:
                fn = lambda: frames.vec_to_enu(node.getVelocity()[3:6], cs)
            self.readers.append((slot, fn))
            return
        if slot.frame not in ("world", "body_local"):
            return self._err(slot, "effort frame must be 'world' or 'body_local'")
        rel = slot.frame == "body_local"
        point = tuple(slot.point) if slot.point is not None else None
        if rel is False and point is not None:
            return self._err(slot, "an application point requires frame 'body_local'")

        def write(v: tuple, node=node, sid=sid, rel=rel, point=point) -> None:
            vec = v if rel else frames.vec_from_enu(v, cs)
            if sid == BODY_FORCE:
                self.sink.forces.append((node, vec, rel, point))
            else:
                self.sink.torques.append((node, vec, rel))

        self.writers.append((slot, write))

    # ------------------------------------------------------------------ per step
    def sample(self, t_ns: int) -> None:
        for j in self.joints.values():
            j.sample(t_ns)

    def read_states(self, channel) -> None:
        for slot, fn in self.readers:
            try:
                v = fn()
            except Exception as e:  # a failing Webots call must never poison the step
                v = None
                self.warnings.append("read of slot %d failed: %s" % (slot.slot_id, e))
            if v is None or (isinstance(v, float) and not math.isfinite(v)) or (
                    isinstance(v, tuple) and not all(math.isfinite(x) for x in v)):
                channel.set_invalid(slot, "Invalid")
            else:
                channel.set_value(slot, v)

    def apply_efforts(self, applied: Dict[int, tuple]) -> None:
        for slot, write in self.writers:
            v = applied.get(slot.slot_id)
            if v is not None:
                write(v)

    def reset(self) -> None:
        for j in self.joints.values():
            j.reset()

    def driven_defs(self) -> set:
        out = set()
        for slot, _ in self.writers:
            out.add(self.entities[slot.entity_id]["webots_def"])
        return out
