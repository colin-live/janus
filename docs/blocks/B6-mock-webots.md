# B6 — mock-webots（无 Webots 的假主控）

**目录** `crates/mock-webots`（库 + 可执行）　**依赖** B1(含 B1.1)、B2　**波次** 2

## 目标
1. 让 B4/B5/B7 在**没有 Webots** 的情况下做闭环开发与测试。
2. 提供协议 §7 **适配器侧**与 §10 的 **Rust 参考实现**（`master` 模块）。B8 的 Python 适配器必须与它行为一致（用 B9 的共享测试向量验证）。

## 库接口（冻结）
```rust
pub mod master {
    pub struct MasterChannel { /* 打开某实例的段与 s2m/m2s */ }
    pub struct ExchangeResult { pub step_id: u64, pub efforts: Vec<EffortOut>, pub model_timed_out: bool, pub errors: Vec<String> }
    pub struct EffortOut { pub slot_id: u32, pub value: EffortValue, pub status: Validity }   // 已经过 §10 校验/限幅/限变化率
    pub enum EffortValue { Scalar(f64), Vec3([f64; 3]) }
    impl MasterChannel {
        pub fn attach(sim_id: u64, instance_id: u32, timeout: Duration) -> Result<Self, MasterError>;  // 等 Ready，重试 NotFound/BadMagic
        pub fn manifest(&self) -> &Manifest;
        pub fn set_state_f64(&mut self, slot_id: u32, v: f64);   pub fn set_state_vec3(&mut self, slot_id: u32, v: [f64; 3]);
        pub fn set_state_quat(&mut self, slot_id: u32, v: [f64; 4]);   pub fn set_state_invalid(&mut self, slot_id: u32, why: Validity);
        pub fn exchange(&mut self, sim_time_ns: i64, dt_ns: i64) -> ExchangeResult;   // 协议 §7 适配器 1–5，含 §10
        pub fn write_report(&self, r: &Report);
        pub fn set_running(&self);  pub fn set_paused(&self);
        pub fn reset(&mut self, timeout: Duration) -> Result<(), MasterError>;   // epoch 复位流程
    }
}
pub mod plant { pub struct RotaryJoint { pub j: f64, pub b: f64, pub theta: f64, pub omega: f64 } impl RotaryJoint { pub fn step(&mut self, torque: f64, dt: f64); } }
```
`plant` 积分用半隐式欧拉：`ω += (τ − b·ω)/J · dt; θ += ω · dt`。

## 可执行
`mock-webots --sim-id <hex> --instance-id <n> [--steps N=5000] [--inertia 0.01] [--damping 0.001] [--csv out.csv] [--reset-at K] [--stop-after-error]`
行为：`attach` → 读 manifest，找到 `joint_position/joint_velocity`（to_model）与 `joint_torque`（to_webots）槽 → 每步：写状态 → `exchange` → 用返回的转矩推进 `RotaryJoint` → 写 CSV（`step,t,theta,omega,torque,status`）。`--reset-at` 触发一次 epoch 复位并把 plant 回到初值。完成后 `hs_state=Closed`。

## 详细需求
1. `attach` 与 `exchange` 必须严格按协议 §7、§10 实现；每个 ToModel 槽先写值再写 validity；`step_id` 最后 Release 写；等待用 `Notifier::wait`。
2. **连接时边界校验**：对 manifest 的每个槽确认 `value_offset + size <= Segment::len()`，否则报错（B2 的 `read_f64` 越界会 panic）。布局用 `Layout::from_header`。
3. 超时/无效处理：不可用的 Effort 槽输出 0，`status` 标明原因；连续 `max_consecutive_misses` 步不可用 → `hs_state=Error`，此后 `exchange` 直接返回全 0。
4. 限幅与限变化率：`|v|>max_abs` → 夹到 `±max_abs`，`status=OutOfRange`；`|Δv| > max_rate_per_s·dt` → 夹变化量。Vec3 按分量处理。
5. 日志输出 `hs_state` 变化与每次错误。

## 验收标准
- 使用线程内的"假模型"（直接用 B2 API 写 effort）完成 10 000 步，`step_id` 与 `effort_step_id` 全程一一对应。
- 故障注入：模型不响应→超时清零→连续失败→`Error`；效果值为 NaN/Inf→当步输出 0 且 `status=Invalid`；`effort_step_id` 落后一步→不施加；超限→夹取；变化率超限→被限。
- 复位：`reset()` 后 `epoch` 加一、`step_id` 从 1 重新计、validity 全部回到 `Missing`，模型 ack 前不得进入 `Running`。
- 导出一份 `tests/vectors/effort_validation.json`（输入序列→期望输出）供 B8 对照；与 B9 协调格式（见 B9 文档）。
