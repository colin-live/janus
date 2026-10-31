# janus_adapter (Block B8)

Webots-side adapter of the Janus coupling protocol. Inside the Supervisor step loop it exchanges one
lock-step message per model instance over shared memory: Webots state -> model, model efforts -> Webots.
Normative docs: `docs/01-protocol-v1.md`, `docs/blocks/B8-webots-py.md`.

## Use in a Supervisor controller

```python
from controller import Supervisor
from janus_adapter import CouplingAdapter

robot = Supervisor()
dt = int(robot.getBasicTimeStep())
adapter = CouplingAdapter(robot)          # finds run.json via WCP_RUN_FILE or <world>/.janus/run.json
adapter.start()                           # attach, validate bindings, write report; raises CouplingError

t_ns = 0
while True:
    adapter.exchange(t_ns, dt * 1_000_000)   # right before robot.step()
    if robot.step(dt) == -1:
        break
    t_ns += dt * 1_000_000
adapter.stop()                            # zero all driven motors, mark segments Closed
```

After a world reset call `adapter.reset()`; use `pause()`/`resume()` around simulation pauses.
`adapter.driven_entities()` returns the DEF names driven by external models (so other tools, e.g. the
MCP motor tools, can refuse to control them).

## Behaviour worth knowing

* Torque is applied with `Motor.setTorque` (persistent in Webots), written **every** step; unusable model
  output becomes an explicit 0. Body forces/torques use `addForce`/`addTorque` (single step, additive).
* `joint_velocity` is a finite difference of the PositionSensor (half-step lag, invalid on the first step).
* The first exchange sees the PositionSensor before any `robot.step()`: position is `Invalid` that step.
* World frames: `ENU` and `NUE` are supported and converted to the protocol's ENU; `EUN` is rejected.
* Windows + x86-64 only in v1 (`shm_posix.py` is a placeholder).

## Tests

```
cd adapters/webots-py
python -m unittest discover -s tests -t .
```
No Webots needed (`tests/fake_robot.py`); the lock-step tests use real Windows shared memory with a fake
host and a threaded fake model (`tests/support.py`).

## Generated files (do not edit by hand)

| file | generator |
|---|---|
| `protocol_offsets.json` | `cargo run -q -p coupling-core --example dump_offsets` |
| `tests/fixtures/layout_vector.json` | `cargo run -q -p coupling-core --example dump_layout` |

Write them with a redirect that does not add a BOM or change the encoding (e.g. `cmd /c "... > file"`).
