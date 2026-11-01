"""Effort validation (protocol §10): finite check, clamp, rate limit, miss accounting.

Semantics (identical to the Rust reference in ``mock-webots`` and pinned by
``tests/vectors/effort_validation.json``):

* A slot value is *usable* only if the model marked it ``Valid``, every component is finite and the slot has
  valid limits (``max_abs`` finite and >= 0, ``max_rate_per_s`` finite and >= 0 when given).
* Unusable value -> output is zero, ``last`` becomes zero (safety zeroing is never rate limited). The status
  is the slot's own reason: the validity the model reported, ``Invalid`` for non-finite values, ``Error`` for
  missing/invalid limits.
* ``|v| > max_abs`` -> clamped, status ``OutOfRange`` (the clamped value IS applied).
* Rate limit: ``|v - last| <= max_rate_per_s * dt`` per component after clamping; the status is unchanged.
* A *step* is a miss when the step-level exchange failed (timeout / stale ``effort_step_id``) or when any
  enabled ``required`` effort slot is unusable. **A miss zeroes the whole instance for that step**: every
  slot outputs 0 and slots that were usable are reported as ``Error``; step-level failures report ``Error``
  on every slot. Unusable non-required slots output zero but do not make a miss.
* ``max_consecutive_misses`` misses in a row -> Error; a fully good step resets the counter.
"""

from __future__ import annotations

import math
from dataclasses import dataclass, field
from typing import Dict, List, Optional, Sequence, Tuple

from . import protocol as P

Values = Tuple[float, ...]


def _valid_limit(x: Optional[float]) -> bool:
    return x is not None and math.isfinite(x) and x >= 0.0


class EffortFilter:
    def __init__(self, components: int, max_abs: Optional[float], max_rate_per_s: Optional[float]):
        self.components = components
        self.max_abs = max_abs
        self.max_rate = max_rate_per_s
        self.configured = _valid_limit(max_abs) and (max_rate_per_s is None or _valid_limit(max_rate_per_s))
        self.last: List[float] = [0.0] * components

    def reset(self) -> None:
        self.last = [0.0] * self.components

    def zero(self) -> Values:
        self.last = [0.0] * self.components
        return tuple(self.last)

    def process(self, values: Optional[Sequence[float]], dt_s: float) -> Tuple[Values, str, bool]:
        """Returns (applied values, validity name, usable)."""
        if not self.configured:
            return self.zero(), "Error", False
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
    """Per model instance. ``slots`` are the enabled ToWebots slots (``layout.Slot``) that reach Webots."""

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
            max_abs = lim.get("max_abs")
            rate = lim.get("max_rate_per_s")
            self.filters[s.slot_id] = EffortFilter(
                comps, None if max_abs is None else float(max_abs), None if rate is None else float(rate))
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
            res.status = {sid: "Error" for sid in self.filters}
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
                # A required slot is unusable: the whole instance is silent for this step.
                for sid, f in self.filters.items():
                    res.applied[sid] = f.zero()
                    if res.status[sid] in ("Valid", "OutOfRange"):
                        res.status[sid] = "Error"
        if res.missed:
            self.misses += 1
            if self.misses >= self.max_misses:
                res.error = True
                res.messages.append("%d consecutive misses" % self.misses)
        else:
            self.misses = 0
        return res
