"""Adapter-side step protocol for ONE model instance (protocol §6/§7). Reference: mock-webots ``master``."""

from __future__ import annotations

import json
import time
from dataclasses import dataclass
from typing import Dict, List, Optional, Sequence, Tuple

from . import layout as L
from . import protocol as P
from . import shm

VALID = P.VALIDITY["Valid"]
MISSING = P.VALIDITY["Missing"]


class CouplingError(RuntimeError):
    pass


@dataclass
class EffortRead:
    """Raw efforts read from the segment after a successful step; ``raw is None`` = step-level failure."""
    raw: Optional[Dict[int, Tuple[int, Optional[Tuple[float, ...]]]]]
    reason: str = ""


class InstanceChannel:
    def __init__(self, seg, s2m, m2s, manifest: dict, slots: List[L.Slot]):
        self.seg = seg
        self.buf = seg.buf
        self.h = seg.header
        self.s2m = s2m
        self.m2s = m2s
        self.manifest = manifest
        self.slots = slots
        self.by_id = {s.slot_id: s for s in slots}
        self.state_slots = [s for s in slots if s.direction == "to_model" and s.enabled]
        self.effort_slots = [s for s in slots if s.direction == "to_webots" and s.enabled]
        self.name = manifest.get("instance_name", "?")
        self.instance_id = manifest["instance_id"]
        self.timeout_ms = self.h.u32("model_timeout_ms") or manifest.get("model_timeout_ms", 50)
        self.step = 0
        self.epoch = self.h.u64("epoch")

    # ------------------------------------------------------------------ connect
    @classmethod
    def connect(cls, sim_id: int, instance_id: int, timeout_s: float = 30.0) -> "InstanceChannel":
        names = P.segment_names(sim_id, instance_id, shm.PLATFORM)
        deadline = time.monotonic() + timeout_s
        last: Exception | None = None
        seg = s2m = m2s = None
        manifest_raw = b""
        while time.monotonic() < deadline:
            try:
                if seg is None:
                    seg = shm.Segment.open(names["segment"])
                    seg.header.validate()
                if not manifest_raw:
                    off, cap = seg.header.region("manifest_off")
                    manifest_raw = seg.read_json_area(off, cap)
                if manifest_raw and s2m is None:
                    s2m = shm.Event.open(names["s2m"])
                if s2m is not None and m2s is None:
                    m2s = shm.Event.open(names["m2s"])
                if m2s is not None:
                    break
            except (shm.ShmNotFound, P.ProtocolError) as e:
                last = e
            time.sleep(0.01)
        if m2s is None:
            raise CouplingError("instance %d: could not attach within %.1fs (%s)" % (instance_id, timeout_s, last))

        manifest = json.loads(manifest_raw.decode("utf-8"))
        if manifest.get("instance_id") != instance_id:
            raise CouplingError("manifest instance_id %r != %d" % (manifest.get("instance_id"), instance_id))
        slots = L.place_from_header(L.slots_from_manifest(manifest), seg.header)
        for s in slots:  # bounds validation: later accesses are unchecked
            if s.validity_offset >= seg.size or s.value_offset + s.size > seg.size:
                raise CouplingError("slot %d out of segment bounds" % s.slot_id)
        ch = cls(seg, s2m, m2s, manifest, slots)
        ch._wait_ready(max(0.0, deadline - time.monotonic()))
        return ch

    def _wait_ready(self, timeout_s: float) -> None:
        deadline = time.monotonic() + timeout_s
        ok = (P.HS["Ready"], P.HS["Running"], P.HS["Paused"])
        while True:
            st = self.h.hs_state
            if st in ok:
                return
            if st in (P.HS["Error"], P.HS["Closed"]):
                raise CouplingError("instance %s: host reports %s" % (self.name, P.HS_NAME[st]))
            if time.monotonic() >= deadline:
                raise CouplingError("instance %s: timed out waiting for Ready (state %s)" % (self.name, P.HS_NAME.get(st, st)))
            time.sleep(0.005)

    # ------------------------------------------------------------------ lifecycle
    def errored(self) -> bool:
        return self.h.hs_state in (P.HS["Error"], P.HS["Closed"])

    def set_hs(self, name: str) -> None:
        """Adapter-owned transitions never overwrite Error/Closed."""
        if name not in ("Error", "Closed") and self.errored():
            return
        self.h.set_u32("hs_state", P.HS[name])

    def write_report(self, report: dict) -> None:
        off, cap = self.h.region("report_off")
        self.seg.write_json_area(off, cap, json.dumps(report).encode("utf-8"))

    def ensure_running(self) -> None:
        if self.h.hs_state in (P.HS["Ready"], P.HS["Paused"]):
            self.set_hs("Running")

    # ------------------------------------------------------------------ state (to model)
    def set_value(self, slot: L.Slot, value) -> None:
        """Write a value, then its validity (value first, protocol §7 step 1)."""
        vt = slot.value_type
        if vt == "f64":
            P.F64.pack_into(self.buf, slot.value_offset, value)
        elif vt == "vec3f64":
            P.V3.pack_into(self.buf, slot.value_offset, *value)
        elif vt == "quatf64":
            P.Q4.pack_into(self.buf, slot.value_offset, *value)
        else:
            raise CouplingError("set_value: unsupported value_type %s" % vt)
        self.buf[slot.validity_offset] = VALID

    def set_invalid(self, slot: L.Slot, why: str = "Invalid") -> None:
        self.buf[slot.validity_offset] = P.VALIDITY[why]

    # ------------------------------------------------------------------ step
    def begin_step(self, sim_time_ns: int, dt_ns: int) -> bool:
        """Protocol §7 adapter steps 2-3. Returns False when the instance is in Error/Closed."""
        if self.errored():
            return False
        self.step += 1
        h = self.h
        h.set_i64("sim_time_ns", sim_time_ns)
        h.set_i64("dt_ns", dt_ns)
        h.set_u64("adapter_heartbeat", h.u64("adapter_heartbeat") + 1)
        h.set_u64("step_id", self.step)  # LAST write; releases all state slots on x86 TSO
        self.s2m.signal()
        return True

    def finish_step(self) -> EffortRead:
        """Protocol §7 adapter steps 4-5 (read side). Never blocks longer than ``model_timeout_ms``."""
        deadline = time.monotonic() + self.timeout_ms / 1000.0
        while True:
            remaining_ms = (deadline - time.monotonic()) * 1000.0
            if remaining_ms <= 0 or not self.m2s.wait(max(1, int(remaining_ms + 0.999))):
                return EffortRead(None, "model timeout (%d ms) at step %d" % (self.timeout_ms, self.step))
            if self.h.u64("effort_step_id") == self.step:
                break  # otherwise: spurious / late wake-up from an older step, keep waiting
        raw: Dict[int, Tuple[int, Optional[Tuple[float, ...]]]] = {}
        for s in self.effort_slots:
            validity = self.buf[s.validity_offset]
            if validity != VALID:
                raw[s.slot_id] = (validity, None)
            elif s.value_type == "f64":
                raw[s.slot_id] = (validity, (P.F64.unpack_from(self.buf, s.value_offset)[0],))
            elif s.value_type == "vec3f64":
                raw[s.slot_id] = (validity, P.V3.unpack_from(self.buf, s.value_offset))
            else:
                raw[s.slot_id] = (validity, None)
        return EffortRead(raw)

    # ------------------------------------------------------------------ reset (epoch)
    def reset(self, timeout_s: float = 2.0) -> None:
        """Protocol §7 reset: the model acks the new epoch when it wakes (no step needed)."""
        h = self.h
        if h.hs_state == P.HS["Closed"]:
            raise CouplingError("instance %s is closed" % self.name)
        # An explicit reset is the recovery path from Error, so it bypasses the Error guard of set_hs().
        h.set_u32("hs_state", P.HS["Resetting"])
        so, sl = h.region("state_off")
        eo, el = h.region("effort_off")
        self.seg.zero(so, sl)  # every validity byte back to Missing
        self.seg.zero(eo, el)
        h.set_u64("step_id", 0)
        h.set_u64("effort_step_id", 0)
        self.step = 0
        self.epoch = h.u64("epoch") + 1
        h.set_u64("epoch", self.epoch)
        self.s2m.signal()
        deadline = time.monotonic() + timeout_s
        while h.u64("model_epoch_ack") != self.epoch:
            if time.monotonic() >= deadline:
                self.set_hs("Error")
                raise CouplingError("instance %s: model did not acknowledge epoch %d" % (self.name, self.epoch))
            time.sleep(0.0005)
        while self.m2s.wait(0):  # drop stale wake-ups from before the reset
            pass
        h.set_u32("hs_state", P.HS["Ready"])

    def close(self) -> None:
        try:
            self.set_hs("Closed")
        finally:
            self.s2m.close()
            self.m2s.close()
            self.seg.close()
