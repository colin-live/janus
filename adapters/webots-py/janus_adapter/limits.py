"""Effort validation (protocol §10): finite check, clamp, rate limit, miss accounting.

Semantics (shared with the Rust reference in ``mock-webots`` through ``tests/vectors/effort_validation.json``):

* A slot value is *usable* only if the model marked it ``Valid`` and every component is finite.
* Unusable value -> output is zero, ``last`` becomes zero (a safety zeroing is never rate limited).
* ``|v| > max_abs`` -> clamped, status ``OutOfRange`` (the clamped value IS applied).
* Rate limit: ``|v - last| <= max_rate_per_s * dt`` applied per component after clamping; it does not
  change the status.
* A *step* is a miss when the step-level exchange failed (timeout / stale ``effort_step_id``) or when any
  enabled ``required`` effort slot is unusable. ``max_consecutive_misses`` misses in a row -> Error.
  A fully good step resets the counter. Unusable non-required slots output zero but are not misses.
"""

from __future__ import annotations

import math
from dataclasses import dataclass, field
from typing import Dict, List, Optional, Sequence, Tuple

from . import protocol as P

Values = Tuple[float, ...]


class EffortFilter:
    def __init__(self, components: int, max_abs: float, max_rate_per_s: Optional[float]):
        self.components = components
        self.max_abs = max_abs
        self.max_rate = max_rate_per_s
        self.last: List[float] = [0.0] * components

    def reset(self) -> None:
        self.last = [0.0] * self.components

    def zero(self) -> Values:
        self.last = [0.0] * self.components
        return tuple(self.last)

    def process(self, values: Optional[Sequence[float]], dt_s: float) -> Tuple[Values, str, bool]:
        """Returns (applied values, validity name, usable)."""
        if values is None or len(values) != self.components:
            return self.zero(), "Invalid", False
        if not all(math.isfinite(v) for v in values):
            return self.zero(), "Invalid", False
        status = "Valid"
        out: List[float] = []
        for i, v in enumerate(values):
            if abs(v) > self.max_abs:
                v = math.copysign(self.max_abs, v)
                status = "OutOfRange"
            if self.max_rate is not None and dt_s > 0.0:
                lim = self.max_rate * dt_s
                d = v - self.last[i]
                if d > lim:
                    v = self.last[i] + lim
                elif d < -lim:
                    v = self.last[i] - lim
            out.append(v)
        self.last = out
        return tuple(out), status, True


@dataclass
class StepResult:
    applied: Dict[int, Values] = field(default_factory=dict)
    status: Dict[int, str] = field(default_factory=dict)
    missed: bool = False
    error: bool = False
    messages: List[str] = field(default_factory=list)


class EffortProcessor:
    """Per model instance. ``slots`` are the enabled ToWebots slots (``layout.Slot``)."""

    def __init__(self, slots, max_consecutive_misses: int = 3):
        self.max_misses = max(1, int(max_consecutive_misses))
        self.misses = 0
        self.filters: Dict[int, EffortFilter] = {}
        self.required: Dict[int, bool] = {}
        for s in slots:
            comps = {"f64": 1, "vec3f64": 3}.get(s.value_type)
            if comps is None:
                continue  # custom/unsupported types are never applied to Webots
            lim = s.limits or {}
            # No limits configured -> refuse everything (max_abs 0): fail safe, B3 should have rejected it.
            self.filters[s.slot_id] = EffortFilter(comps, float(lim.get("max_abs", 0.0)), lim.get("max_rate_per_s"))
            self.required[s.slot_id] = s.required

    def reset(self) -> None:
        self.misses = 0
        for f in self.filters.values():
            f.reset()

    def zero_all(self) -> Dict[int, Values]:
        return {sid: f.zero() for sid, f in self.filters.items()}

    def process(self, raw: Optional[Dict[int, Tuple[int, Optional[Values]]]], dt_ns: int,
                reason: str = "") -> StepResult:
        """``raw`` maps slot_id -> (validity byte, values or None). ``None`` = step-level failure."""
        res = StepResult()
        dt_s = dt_ns / 1e9
        if raw is None:
            res.missed = True
            res.applied = self.zero_all()
            res.status = {sid: "Stale" for sid in self.filters}
            res.messages.append(reason or "step-level failure")
        else:
            for sid, f in self.filters.items():
                validity, values = raw.get(sid, (P.VALIDITY["Missing"], None))
                if validity != P.VALIDITY["Valid"]:
                    applied, status, usable = f.zero(), P.VALIDITY_NAME.get(validity, "Error"), False
                else:
                    applied, status, usable = f.process(values, dt_s)
                res.applied[sid] = applied
                res.status[sid] = status
                if not usable and self.required[sid]:
                    res.missed = True
                    res.messages.append("slot %d unusable (%s)" % (sid, status))
        if res.missed:
            self.misses += 1
            if self.misses >= self.max_misses:
                res.error = True
                res.messages.append("%d consecutive misses" % self.misses)
        else:
            self.misses = 0
        return res
