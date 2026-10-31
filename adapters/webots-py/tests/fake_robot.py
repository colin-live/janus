"""Minimal stand-ins for the Webots Supervisor API subset used by the adapter."""

import math

IDENTITY = (1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0)


class FakeField:
    def __init__(self, value=None, items=None):
        self.value, self.items = value, items or []

    def getSFString(self):
        return self.value

    def getCount(self):
        return len(self.items)

    def getMFNode(self, i):
        return self.items[i]


class FakeNode:
    def __init__(self, type_name, fields=None, position=(0.0, 0.0, 0.0), orientation=IDENTITY, velocity=None):
        self.type_name = type_name
        self.fields = fields or {}
        self.position, self.orientation = position, orientation
        self.velocity = list(velocity or [0.0] * 6)
        self.forces, self.torques = [], []

    def getTypeName(self):
        return self.type_name

    def getField(self, name):
        return self.fields.get(name)

    def getPosition(self):
        return list(self.position)

    def getOrientation(self):
        return list(self.orientation)

    def getVelocity(self):
        return list(self.velocity)

    def addForce(self, f, rel):
        self.forces.append((tuple(f), rel, None))

    def addForceWithOffset(self, f, off, rel):
        self.forces.append((tuple(f), rel, tuple(off)))

    def addTorque(self, t, rel):
        self.torques.append((tuple(t), rel))


class FakeMotor:
    def __init__(self):
        self.torque = 0.0
        self.torque_history = []
        self.feedback_enabled = False
        self.feedback = 0.0

    def setTorque(self, v):
        self.torque = v
        self.torque_history.append(v)

    setForce = setTorque

    def enableTorqueFeedback(self, ts):
        self.feedback_enabled = True

    enableForceFeedback = enableTorqueFeedback

    def getTorqueFeedback(self):
        return self.feedback

    getForceFeedback = getTorqueFeedback


class FakeSensor:
    def __init__(self):
        self.value = float("nan")  # like Webots before the first step
        self.enabled = None

    def enable(self, ts):
        self.enabled = ts

    def getValue(self):
        return self.value


class FakeRobot:
    def __init__(self, coordinate_system="ENU", basic_time_step=1.0):
        info_fields = {} if coordinate_system is None else {"coordinateSystem": FakeField(coordinate_system)}
        self.world_info = FakeNode("WorldInfo", info_fields)
        self.defs = {}
        self.devices = {}
        self.basic_time_step = basic_time_step

    def getRoot(self):
        return FakeNode("Group", {"children": FakeField(items=[self.world_info])})

    def getBasicTimeStep(self):
        return self.basic_time_step

    def getFromDef(self, name):
        return self.defs.get(name)

    def getDevice(self, name):
        return self.devices.get(name)

    # -- builders for tests --
    def add_hinge(self, def_name, motor="m1", sensor="s1", joint_type="HingeJoint"):
        m_node = FakeNode("RotationalMotor", {"name": FakeField(motor)})
        s_node = FakeNode("PositionSensor", {"name": FakeField(sensor)})
        node = FakeNode(joint_type, {"device": FakeField(items=[m_node, s_node])})
        self.defs[def_name] = node
        self.devices[motor], self.devices[sensor] = FakeMotor(), FakeSensor()
        return self.devices[motor], self.devices[sensor], node

    def add_solid(self, def_name, **kw):
        node = FakeNode("Solid", **kw)
        self.defs[def_name] = node
        return node


def finite(x):
    return not (math.isnan(x) or math.isinf(x))
