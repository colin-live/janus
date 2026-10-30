# B9 — 集成、一致性与台架测试

**目录** `crates/janus-it`（新 crate，加入 workspace）、`tests/vectors/`、`tests/bench/`　**依赖** 全部　**波次** 3（测试向量格式在波次 1 就定）

## 目标
证明各 Block 拼在一起是对的：协议一致性（Rust 与 Python 两个适配器侧实现行为相同）、闭环正确性、故障下的安全性、性能。

## 1. 共享测试向量（先于实现，波次 1 由 B9 负责人定稿）
`tests/vectors/effort_validation.json`：
```json
{ "cases": [
  { "name": "clamp_max_abs", "limits": {"max_abs": 5.0, "max_rate_per_s": null}, "dt_ns": 1000000,
    "steps": [ {"effort": 7.0, "validity": "valid", "effort_step_id_ok": true, "expect": {"value": 5.0, "status": "out_of_range"}} ] },
  { "name": "rate_limit", "limits": {"max_abs": 10.0, "max_rate_per_s": 1000.0}, "dt_ns": 1000000,
    "steps": [ {"effort": 0.0, "expect": {"value": 0.0, "status": "valid"}}, {"effort": 5.0, "expect": {"value": 1.0, "status": "valid"}} ] }
] }
```
必须覆盖：夹取、限变化率、NaN、Inf、`validity≠Valid`、`effort_step_id` 落后、超时、连续 N 步失败→Error、Vec3 分量。B6（Rust）与 B8（Python）各有一个测试读取同一文件逐条断言。

`tests/vectors/names.json`：golden 名字用例（见协议 §2）；B2、B8 都要测。

## 2. 集成测试（`crates/janus-it`，CI 无需 Webots）
| 编号 | 场景 | 通过条件 |
|---|---|---|
| IT-1 | host `run` + B7 进程 + B6 mock，5000 步闭环 | 稳态转速误差 < 1%，`step_id` 全程无丢失 |
| IT-2 | 同 IT-1，4 个实例并行 | 各实例互不影响，总耗时线性增长 < 1.5× |
| IT-3 | 模型进程中途被杀 | 超时清零，`max_consecutive_misses` 步后 `Error`，host 日志含原因 |
| IT-4 | reset（epoch） | 复位后曲线与首次运行逐步一致 |
| IT-5 | 协商失败（缺必需信号、`steppable=false`、custom_signals 不一致） | host 退出码 2，信息含槽名 |
| IT-6 | Python 适配器（`FakeRobot`）对 Rust 假模型 | 1000 步无错步；与 IT-1 的 Rust 主控结果数值一致（容差 1e-9） |
| IT-7 | 模型输出 NaN / 越限 | 被适配器拦截，Webots 侧永远看不到非有限值 |

## 3. 性能台架（只报告，不卡 CI）
`tests/bench/`：锁步往返延迟 p50/p99/max（1 实例、4 实例；有/无自旋）；每步 Python 适配器开销。输出一页 Markdown 报告，作为后续是否需要 Rust 物理插件的依据。**目标值（参考）：** Rust 主控 ≥ 5 000 步/s/实例；Python 适配器每步 < 150 µs（4 槽）。

## 4. 真实 Webots spike（手动，清单式）
1. `examples/arm2dof` 的肩关节绑定 B7 电机，电压阶跃，记录 Webots 侧角速度曲线，与 IT-1 的 mock 曲线对比。
2. 差分速度的滞后误差评估；torque 控制模式切换是否需要 `setPosition(inf)`；`joint_torque_feedback` 是否可读；记录实测结论到 `docs/spike-notes.md`。
3. 不同 `basicTimeStep`（1/2/8 ms）下电机稳定性；高增益低惯量下是否出现显式耦合振荡（评估协议"一步延迟"的影响）。

## 验收标准
IT-1 ~ IT-7 在 Windows 上自动通过；向量文件被 B6 与 B8 同时消费且全部通过；性能报告和 spike 笔记各一份。
