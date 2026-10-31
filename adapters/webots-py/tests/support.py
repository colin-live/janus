"""Test harness: a fake host (creates the segment like coupling-host would) and a fake model process
(thread) that implements the model side of protocol §7."""

import json
import struct
import threading
import time

from janus_adapter import layout as L
from janus_adapter import protocol as P
from janus_adapter import shm

SIG = {n: s["id"] for n, s in P.SIGNALS_BY_NAME.items()}


def slot_dict(slot_id, entity_id, name, direction, unit, required=True, limits=None, frame="none",
              value_type="f64", enabled=True, point=None, signal_id=None):
    d = {"slot_id": slot_id, "entity_id": entity_id, "signal_id": signal_id if signal_id is not None else SIG[name],
         "name": name, "value_type": value_type, "direction": direction, "frame": frame, "unit": unit,
         "required": required, "enabled": enabled}
    if limits is not None:
        d["limits"] = limits
    if point is not None:
        d["point"] = point
    return d


def joint_manifest(sim_id, instance_id=1, def_name="JOINT", dt_ns=1_000_000, timeout_ms=50, max_misses=3,
                   limits=None, unit_pos="rad", unit_vel="rad/s"):
    return {
        "manifest_version": 1, "simulation_id": "0x%x" % sim_id, "instance_id": instance_id,
        "instance_name": "motor%d" % instance_id, "sync_mode": "lockstep", "dt_ns": dt_ns,
        "model_timeout_ms": timeout_ms,
        "effort_policy": {"ttl_steps": 1, "on_timeout": "zero", "max_consecutive_misses": max_misses},
        "world_frame": "ENU",
        "entities": [{"entity_id": 1, "name": "joint", "webots_def": def_name, "kind": "joint"}],
        "slots": [
            slot_dict(0, 1, "joint_position", "to_model", unit_pos),
            slot_dict(1, 1, "joint_velocity", "to_model", unit_vel, required=False),
            slot_dict(2, 1, "joint_torque", "to_webots", "N*m", limits=limits or {"max_abs": 5.0}, frame="joint_axis"),
        ],
    }


class FakeHost:
    """Creates the segment, events and manifest, then marks the instance Ready."""

    def __init__(self, manifest, caps=(64 * 1024, 16 * 1024, 16 * 1024)):
        self.manifest = manifest
        self.sim_id = int(manifest["simulation_id"], 16)
        self.instance_id = manifest["instance_id"]
        slots = L.slots_from_manifest(manifest)
        lay = L.build_layout(slots, *caps)
        self.layout = lay
        n = P.segment_names(self.sim_id, self.instance_id, shm.PLATFORM)
        self.names = n
        self.seg = shm.Segment.create(n["segment"], lay["total_size"])
        self.s2m = shm.Event.create(n["s2m"])
        self.m2s = shm.Event.create(n["m2s"])
        h, b, off = self.seg.header, self.seg.buf, P.OFF
        h.set_u64("total_size", lay["total_size"])
        h.set_u64("simulation_id", self.sim_id)
        h.set_u32("instance_id", self.instance_id)
        h.set_u32("header_size", P.HEADER_SIZE)
        struct.pack_into("<HH", b, off["protocol_major"], P.PROTOCOL_MAJOR, P.PROTOCOL_MINOR)
        for key in ("manifest", "report", "state", "effort"):
            h.set_u64(key + "_off", lay[key + "_off"])
            struct.pack_into("<Q", b, off[key + "_off"] + 8, lay[key + "_len"])
        struct.pack_into("<QQ", b, off["manifest_off"] + 16, lay["hello_off"], lay["hello_len"])
        h.set_u32("state_slot_count", lay["state_slot_count"])
        h.set_u32("effort_slot_count", lay["effort_slot_count"])
        h.set_u32("model_timeout_ms", manifest["model_timeout_ms"])
        self.seg.write_json_area(lay["manifest_off"], lay["manifest_len"], json.dumps(manifest).encode())
        self.seg.write_bytes(off["magic"], P.MAGIC)  # magic LAST, like coupling-shm
        h.set_u32("hs_state", P.HS["Ready"])

    @property
    def hs(self):
        return P.HS_NAME[self.seg.header.hs_state]

    def report(self):
        off, cap = self.seg.header.region("report_off")
        raw = self.seg.read_json_area(off, cap)
        return json.loads(raw) if raw else None

    def close(self):
        for e in (self.s2m, self.m2s):
            e.close()
        self.seg.close()


class StopResponding(Exception):
    pass


class FakeModel(threading.Thread):
    """Model side of protocol §7. ``behavior(step_id, states) -> {slot_name: value | (validity, value)}``."""

    def __init__(self, host: FakeHost, behavior):
        super().__init__(daemon=True)
        self.host, self.behavior = host, behavior
        self.slots = {s.name: s for s in L.place_from_header(L.slots_from_manifest(host.manifest), host.seg.header)}
        self.processed = []
        self.acked_epochs = []
        self._halt = threading.Event()
        self.hung = False

    def stop(self):
        self._halt.set()
        self.host.s2m.signal()

    def run(self):
        h = self.host.seg.header
        buf = self.host.seg.buf
        my_epoch, last = h.u64("epoch"), 0
        while not self._halt.is_set():
            if not self.host.s2m.wait(20):
                continue
            epoch = h.u64("epoch")  # epoch first: a reset wake-up carries no step
            if epoch != my_epoch:
                my_epoch, last = epoch, 0
                self.processed.clear()
                h.set_u64("model_epoch_ack", epoch)
                self.acked_epochs.append(epoch)
            n = h.u64("step_id")
            if n == 0 or n == last or self.hung:
                continue
            states = {}
            for name, s in self.slots.items():
                if s.direction == "to_model":
                    ok = buf[s.validity_offset] == P.VALIDITY["Valid"]
                    states[name] = P.F64.unpack_from(buf, s.value_offset)[0] if ok and s.value_type == "f64" else None
            try:
                out = self.behavior(n, states)
            except StopResponding:
                self.hung = True
                continue
            for name, val in out.items():
                s = self.slots[name]
                validity, value = val if isinstance(val, tuple) else (P.VALIDITY["Valid"], val)
                if value is not None:
                    P.F64.pack_into(buf, s.value_offset, value)
                buf[s.validity_offset] = validity
            self.processed.append(n)
            h.set_u64("model_heartbeat", h.u64("model_heartbeat") + 1)
            h.set_u64("effort_step_id", n)  # LAST
            self.host.m2s.signal()
            last = n
