# B7 — dcmotor-model（参考直流电机模型进程）

**目录** `crates/dcmotor-model`（另含 `examples/arm2dof/models/shoulder_motor.yaml`）　**依赖** B1、B4　**波次** 3

## 目标
第一个真实的独立进程模型：永磁直流电机 + 电压指令，作为 SDK 的参考用法，同时是整个耦合链路的闭环验证对象。**上游控制信号不在范围**：电压指令由参数文件给出（常量或分段表），不做任何上游通信。

## 模型
- 电气：`L·di/dt = V − R·i − Ke·ω`；机械耦合：`τ = Kt·i`（输出 `joint_torque`）；`ω` 取自 Webots 给定的 `joint_velocity`，在一个 Webots 步内保持不变。
- 内部子步：`internal_dt`（默认 100 µs）。每个外部 `dt_ns` 被切成整数个子步，余数并入最后一步；电气积分采用对 `i` 的精确离散（`i ← i·e^{-R·h/L} + (V−Ke·ω)/R·(1−e^{-R·h/L})`），避免小 `L` 时刚性。
- 限流：`|i| ≤ i_max`（硬夹取，计入日志计数）。

## 参数文件（YAML，厂家单位在导入阶段换算为 SI，使用 `coupling-config::units`）
```yaml
resistance: 1.2 ohm
inductance: 0.8 mH
torque_constant: 0.05            # N*m/A
back_emf_constant: 0.05          # V*s/rad
current_limit: 20 A
internal_dt: 100 us
voltage:                         # 二选一：常量 / 分段（按仿真时间，保持上一个值）
  constant: 12 V
  # profile: [ {t: 0 s, v: 0 V}, {t: 0.1 s, v: 12 V} ]
```
`--params <path>`、`--sim-id`、`--instance-id`（缺省读环境变量 `WCP_SIM_ID`、`WCP_INSTANCE_ID`）。

## 详细需求
1. 用 B4 的 `ModelClient`：`connect`（hello：`steppable=true`、`sync_modes=["lockstep"]`、`internal_dt_ns`、`supported_signals=[0x0001,0x0002,0x0101]`，并声明自定义 `motor_current`(0x8001, f64, to_webots, A, electrical)）→ 循环 `wait_step`/计算/`commit`。
2. `epoch_changed` 时复位 `i=0`、仿真时间基准、计数器。
3. 状态槽无效（`read_f64` 为 `None`）→ 该步输出槽标 `Invalid`（不写数值），模型内部状态保持，不 panic。
4. 若配置里没有 `motor_current` 槽，`slot()` 返回 `None`，跳过写入，不报错。
5. 退出：收到 `Closed` 或 `wait_step` 连续超时（`model_timeout_ms × 20`）后退出并返回非 0。
6. 结构：`model.rs`（纯函数式电机模型，无 I/O，可单测）+ `main.rs`（I/O 与参数）。

## 验收标准
- 纯模型单测：阶跃电压下稳态电流 `(V − Ke·ω)/R`；电气时间常数 `L/R` 内达到 63% ±2%；子步划分对结果的影响 < 0.1%。
- 闭环测试（B6 的 `RotaryJoint` + 本模型，线程内或进程间）：稳态转速 `ω = Kt·V /(R·b + Kt·Ke)` 误差 < 1%；`i_max` 生效。
- reset 后输出与首次运行逐步一致（确定性）。
- 参数单位解析：`"3000 rpm"`、`"0.8 mH"`、`"100 us"` 均正确。
