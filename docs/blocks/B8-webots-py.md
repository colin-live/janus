# B8 — webots-py（真实 Webots 适配器，Python）

**目录** `adapters/webots-py`　**依赖** 仅 `docs/01-protocol-v1.md` 与 B1 产出的 `protocol_offsets.json`　**波次** 1（可与 B2/B3 并行）

## 目标
在现有 Supervisor 的步进循环里，每步用共享内存与模型进程交换数据：读 Webots 状态 → 写入 ToModel 槽；读 ToWebots 槽 → 校验、聚合后施加到 Webots。第一阶段只做 Python；以后可由 Rust 物理插件替换，共享内存格式不变。

## 公开 API
```python
class CouplingAdapter:
    def __init__(self, robot, run_file: str | None = None, log=print): ...
    def start(self, timeout_s: float = 30.0) -> None   # 读 run.json、连接所有实例、解析并校验绑定、写 report；失败抛 CouplingError
    def exchange(self, sim_time_ns: int, dt_ns: int) -> None   # 每步在 robot.step() 之前调用
    def reset(self) -> None        # 世界复位/重载后调用（epoch 流程）
    def pause(self) -> None;  def resume(self) -> None
    def stop(self) -> None         # 置 Closed、清零所有作用量
    def driven_entities(self) -> set[str]   # 被外部模型驱动的 DEF 名，供 MCP 的电机工具拒绝/转发
```
`run_file` 缺省取环境变量 `WCP_RUN_FILE`，再退化为 `<世界目录>/.janus/run.json`（由 B5 写出）。

## 模块划分
`protocol.py`（读 `protocol_offsets.json`，所有偏移来自它，**禁止手写数字**）、`shm_win.py`（`mmap` + `ctypes` 调 kernel32：`OpenEventW/SetEvent/WaitForSingleObject`）、`shm_posix.py`（CI 用，可简化）、`channel.py`（协议 §7 适配器侧状态机）、`limits.py`（§10 校验/限幅/限变化率/聚合）、`webots_binding.py`（绑定解析与读写 Webots）、`adapter.py`。

## 详细需求
1. **命名**逐字节遵循协议 §2 的 golden 名字；Windows 打开映射需先以长度 256 读 header，取 `total_size` 后用该长度重新映射。
2. **平台限制**：仅 x86-64（依赖 TSO 的 acquire/release 语义）；启动时检查 `platform.machine()`，其他架构直接拒绝。8 字节对齐字段用 `struct.pack_into`/`unpack_from` 一次读写；写 `step_id` 必须是整个 ToModel 写入之后的最后一次写。
3. **绑定解析与校验**（`start`）：对每个 entity 用 `robot.getFromDef(def)`；检查节点类型与 `kind`、`unit` 一致：`HingeJoint`⇔`rad`/`joint_torque`，`SliderJoint`⇔`m`/`joint_force`，`Solid/Robot…`⇔body 类信号；关节的 motor/sensor 取 manifest 的覆盖名，否则读节点 `device` 字段自动发现；找不到、类型不符、坐标系为 `EUN`（左手系，v1 不支持）等一律写入 report 并置 `Error`。`world_coordinate_system` 写入 report。
4. **状态读取**：关节位置用 `PositionSensor`（按 `basicTimeStep` 使能）；**关节速度由位置差分得到**（首步无效），并在 report 的 `warnings` 记录"差分速度有半步滞后"。刚体量用 Supervisor 接口 (`getPosition/getOrientation/getVelocity`)，统一转换为 **ENU 右手系**（支持 `ENU`、`NUE`；`NUE→ENU` 为 `(E,N,U)=(z,x,y)`，需单测）。读不到时写 `Invalid`，不得保留上一步的值冒充有效。
5. **作用量施加**：`joint_torque/joint_force` → `motor.setTorque/setForce`（**持久型**，每步必须显式写，无效时写 0）；`body_force/body_torque` → `addForce/addForceWithOffset/addTorque`（单步型）。同一目标、同一类作用量先转到 World 帧再**求和**。
6. 步进与失败策略完全按协议 §7、§10；**行为必须与 B6 的 `MasterChannel` 一致**，用 `tests/vectors/effort_validation.json`（B9 持有）逐条断言。
7. **复位**：`reset()` 走 epoch 流程；暂停期间 `pause()` 置 `Paused`，host 不判心跳。
8. 性能：每步 Python 开销在 4 个槽以内应 < 150 µs（用 `time.perf_counter_ns` 统计并在 `stop()` 输出 p50/p99）；批量读写，不做逐字段 Python 函数调用的过度封装。
9. 没有 Webots 也能单测：`webots_binding` 对 `robot` 的依赖通过一个很薄的协议类注入，提供 `FakeRobot` 用于测试。

## 验收标准
- `pytest`（无需 Webots）：offsets 与 `dump_offsets` 输出一致；名字 golden 测试；ENU/NUE 转换；`limits.py` 通过共享测试向量；用 `FakeRobot` + B6 的 `mock-webots`/一个 Python 假模型跑 1000 步锁步无错步。
- 故障注入同 B6：超时、NaN、落后一步、超限、连续失败→`Error` 并清零。
- 手测清单（有 Webots 时）：`examples/arm2dof` 的肩关节被 B7 驱动，`start` 后 report `ok=true`；`stop` 后电机转矩回到 0；reset 后复现首次曲线。

## 不在本 Block
与 MCP 服务器现有电机工具的仲裁逻辑（被外部驱动的关节让 `set_motor_target` 之类拒绝或转发为模型指令）只暴露 `driven_entities()`，接线放到集成阶段。
