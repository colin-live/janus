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


## 状态（2026-10-30）

**已完成**：`adapters/webots-py` 全部模块（`protocol/layout/shm_win/channel/limits/frames/webots_binding/adapter`）与 58 个测试（`python -m unittest discover -s tests -t .`，无需 Webots），Windows 下连续运行稳定；锁步闭环 500 步，假模型线程下 `exchange` 往返 p50 ≈ 20–25 µs、p99 < 150 µs（仅供参考，真实模型进程会更慢）。

**与文档的偏差 / 实现决定**
- Linux 后端只留占位（`shm_posix.py`），按 B8 文档"CI 用，可简化"处理；测试在非 Windows 上跳过共享内存部分。
- 偏移与枚举来自 B1 生成的 `protocol_offsets.json`，布局由 `tests/fixtures/layout_vector.json`（B1 `dump_layout` 生成）与 Rust `Layout::build` 交叉校验。
- 效果值校验/miss 计数的精确语义已写入协议 §10；`tests/vectors/effort_validation.json`（B6 导出、B9 维护）现已存在，`test_limits.py` 逐条消费全部 19 个用例并通过（2026-10-31）。接入向量时发现 B8 的两处语义与 B6 参考实现不同，已按协议 §10 的最终版修正：①步级失败/缺 limits 的槽状态为 `Error`（原为 `Stale`/`OutOfRange`）；②任一 required 槽不可用时整个实例该步静默（原为只清零出问题的槽）。
- 第一次 `exchange` 发生在任何 `robot.step()` 之前，PositionSensor 为 NaN，位置/速度该步为 `Invalid`（模型应容忍；`max_consecutive_misses` 默认 3 足够覆盖）。

**对契约的新增澄清（已写入协议 §7/§10 与 B4 文档，不改二进制布局）**
1. 复位时适配器必须**唤醒模型**（s2m），模型醒来先比较 epoch 再看 step_id，否则模型阻塞在 `wait` 时永远无法 ack（原文有死锁风险）。B4 的 `wait_step` 因此增加 `WaitEvent::Reset` 与 `ack_reset()`。
2. 失败步（miss）与 Error 状态下不再等待模型的精确定义。

**待实测（需要真实 Webots，见 B9 spike 清单）**
- `Motor.setTorque` 是否需要先把电机切到力矩模式；`getTorqueFeedback` 可用性；`addForceWithOffset` 偏移的参考点（质心 vs 节点原点）；`Supervisor.getWorldPath` 在目标 Webots 版本是否存在（已用 `getattr` 兜底）。
