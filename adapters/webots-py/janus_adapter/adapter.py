"""Public entry point: ``CouplingAdapter`` (docs/blocks/B8-webots-py.md)."""

from __future__ import annotations

import json
import os
import platform
import time
from typing import Dict, List, Optional

from . import protocol as P
from .channel import CouplingError, InstanceChannel
from .limits import EffortProcessor
from .webots_binding import EffortSink, InstanceBinding, read_coordinate_system


class _Instance:
    def __init__(self, name, channel, binding, processor):
        self.name, self.channel, self.binding, self.processor = name, channel, binding, processor
        self.miss_logged = 0


class CouplingAdapter:
    def __init__(self, robot, run_file: Optional[str] = None, log=print):
        self.robot = robot
        self.run_file = run_file
        self.log = log
        self.instances: List[_Instance] = []
        self.sink = EffortSink()
        self._durations_ns: List[int] = []
        self.started = False

    # ------------------------------------------------------------------ start
    def _find_run_file(self) -> str:
        if self.run_file:
            return self.run_file
        env = os.environ.get("WCP_RUN_FILE")
        if env:
            return env
        dirs = []
        world = getattr(self.robot, "getWorldPath", None)
        if callable(world):
            try:
                dirs.append(os.path.dirname(world()))
            except Exception:
                pass
        here = os.getcwd()
        for _ in range(5):
            dirs.append(here)
            here = os.path.dirname(here)
        for d in dirs:
            cand = os.path.join(d, ".janus", "run.json")
            if os.path.isfile(cand):
                return cand
        raise CouplingError("run.json not found (set WCP_RUN_FILE or pass run_file)")

    def start(self, timeout_s: float = 30.0) -> None:
        if platform.machine().lower() not in ("amd64", "x86_64"):
            raise CouplingError("the Python adapter relies on x86-64 memory ordering; got %s" % platform.machine())
        with open(self._find_run_file(), "r", encoding="utf-8") as f:
            run = json.load(f)
        sim_id = int(str(run["sim_id"]), 16)
        warnings: List[str] = []
        cs = read_coordinate_system(self.robot, warnings)
        failures: List[str] = []
        for w in warnings:
            self.log("[janus] warning: " + w)
        for item in run["instances"]:
            channel = InstanceChannel.connect(sim_id, item["instance_id"], timeout_s)
            binding = InstanceBinding(self.robot, channel.manifest, channel.slots, cs, self.sink)
            report = binding.resolve()
            channel.write_report(report)
            if not report["ok"]:
                channel.set_hs("Error")
                failures += ["%s: %s" % (channel.name, e) for e in report["errors"]]
            policy = channel.manifest.get("effort_policy", {})
            if policy.get("on_timeout", "zero") != "zero" or policy.get("ttl_steps", 1) != 1:
                self.log("[janus] warning: %s: only on_timeout=zero, ttl_steps=1 are implemented" % channel.name)
            proc = EffortProcessor([s for s, _ in binding.writers], policy.get("max_consecutive_misses", 3))
            self.instances.append(_Instance(channel.name, channel, binding, proc))
            for w in report["warnings"]:
                self.log("[janus] %s: %s" % (channel.name, w))
        if failures:
            self.sink.zero_motors()
            raise CouplingError("binding validation failed:\n  " + "\n  ".join(failures))
        self.started = True
        self.log("[janus] adapter started: %d instance(s), world frame %s" % (len(self.instances), cs))

    # ------------------------------------------------------------------ per step
    def exchange(self, sim_time_ns: int, dt_ns: int) -> None:
        """Call right before ``robot.step()``. Never blocks longer than the slowest ``model_timeout_ms``."""
        t0 = time.perf_counter_ns()
        active: Dict[str, bool] = {}
        for inst in self.instances:
            ch = inst.channel
            if ch.errored():
                active[inst.name] = False
                continue
            ch.ensure_running()
            inst.binding.sample(sim_time_ns)
            inst.binding.read_states(ch)
            active[inst.name] = ch.begin_step(sim_time_ns, dt_ns)
        self.sink.begin()
        for inst in self.instances:
            ch = inst.channel
            if active[inst.name]:
                er = ch.finish_step()
                res = inst.processor.process(er.raw, dt_ns, er.reason)
                if res.messages:
                    self._log_misses(inst, ch.step, res.messages)
                if res.error:
                    ch.set_hs("Error")
                    self.log("[janus] %s: entering Error state, efforts are zeroed" % inst.name)
                applied = res.applied
            else:
                applied = inst.processor.zero_all()
            inst.binding.apply_efforts(applied)
        self.sink.flush()
        self._durations_ns.append(time.perf_counter_ns() - t0)

    def _log_misses(self, inst: _Instance, step: int, messages: List[str]) -> None:
        inst.miss_logged += 1
        if inst.miss_logged <= 3 or inst.miss_logged % 100 == 0:
            self.log("[janus] %s step %d: %s (miss #%d)" % (inst.name, step, "; ".join(messages), inst.miss_logged))

    # ------------------------------------------------------------------ control
    def reset(self, timeout_s: float = 2.0) -> None:
        self.sink.zero_motors()
        for inst in self.instances:
            inst.channel.reset(timeout_s)
            inst.processor.reset()
            inst.binding.reset()
            inst.miss_logged = 0

    def pause(self) -> None:
        for inst in self.instances:
            inst.channel.set_hs("Paused")

    def resume(self) -> None:
        for inst in self.instances:
            inst.channel.ensure_running()

    def stop(self) -> None:
        self.sink.zero_motors()
        for inst in self.instances:
            inst.channel.close()
        self.instances = []
        s = self.stats()
        if s:
            self.log("[janus] exchange: n=%(n)d p50=%(p50_us).0fus p99=%(p99_us).0fus max=%(max_us).0fus" % s)

    def driven_entities(self) -> set:
        out = set()
        for inst in self.instances:
            out |= inst.binding.driven_defs()
        return out

    def stats(self) -> dict:
        d = sorted(self._durations_ns)
        if not d:
            return {}
        pick = lambda q: d[min(len(d) - 1, int(q * len(d)))] / 1000.0
        return {"n": len(d), "p50_us": pick(0.5), "p99_us": pick(0.99), "max_us": d[-1] / 1000.0}
