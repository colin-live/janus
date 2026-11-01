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

## 状态

**完成日期** 2026-10-31

### 已完成

- `crates/mock-webots/src/master.rs`：实现冻结接口 `MasterChannel`、`ExchangeResult`、`EffortOut`、`EffortValue`，以及协议 §7 适配器侧与 §10 校验/限幅/限变化率/失败策略。
- `crates/mock-webots/src/plant.rs`：实现 `RotaryJoint`，半隐式欧拉 `ω += (τ − b·ω)/J · dt; θ += ω · dt`；非法 J / 非有限转矩 / 非正 dt 直接忽略。
- `crates/mock-webots/src/main.rs`：实现 `mock-webots` 可执行及文档列出的全部参数；结束时 `hs_state=Closed`。
- `tests/vectors/effort_validation.json`：导出 19 个用例，覆盖夹取、限变化率（含夹取后限变化率）、NaN/Inf、`validity≠Valid`、`effort_step_id` 落后、超时、连续 miss→Error、缺 limits、非 required 槽、Vec3 分量。
- `tests/lockstep.rs`、`tests/faults.rs`、`tests/reset.rs`、`tests/vectors.rs`、`tests/cli.rs`：验收测试与可执行闭环测试。
- `tests/api.rs`：状态 setter 的“先写值、再写 validity”顺序与生命周期接口（`hs_state`/`set_closed`/`try_*`）测试。

### 验收对照

| 验收项 | 覆盖测试 |
|---|---|
| 线程内假模型 10 000 步，`step_id` 与 `effort_step_id` 一一对应 | `tests/lockstep.rs::ten_thousand_lockstep_steps_without_loss_or_duplication` |
| 模型不响应→超时清零→连续失败→Error | `tests/faults.rs::model_timeout_zeros_efforts_then_enters_error_after_consecutive_misses` |
| NaN/Inf→当步输出 0 且 `status=Invalid` | `tests/faults.rs::nan_and_inf_become_zero_with_invalid_status` |
| `effort_step_id` 落后一步→不施加 | `tests/faults.rs::stale_effort_step_id_is_not_applied` |
| 超限→夹取；变化率超限→被限 | `tests/faults.rs::clamping_then_rate_limiting_is_applied_per_component`、`vec3_limits_are_applied_per_component` |
| 缺 limits→安全清零 | `tests/faults.rs::missing_limits_is_fail_safe` |
| 非 required 槽不可用不触发 miss | `tests/faults.rs::non_required_invalid_does_not_count_as_a_miss` |
| 复位：epoch+1、`step_id` 从 1 重计、validity 全 Missing、ack 前不进入 Running | `tests/reset.rs::reset_bumps_epoch_clears_validity_and_restarts_step_ids` |
| 复位超时→Error | `tests/reset.rs::reset_times_out_and_enters_error_when_the_model_does_not_ack` |
| 共享向量逐条通过 | `tests/vectors.rs::shared_effort_validation_vectors_pass`（同一文件供 B8 使用） |
| 可执行闭环 + CSV | `tests/cli.rs::executable_closes_the_loop_and_writes_csv` |

### 验证命令

- `cargo fmt -p mock-webots -- --check`
- `cargo test -p mock-webots`（单元 6、api 3、cli 1、faults 7、lockstep 1、reset 2、vectors 1，全绿）
- `cargo clippy -p mock-webots --all-targets -- -D warnings`
- `cargo check -p mock-webots --target x86_64-unknown-linux-gnu --all-targets`
- `cargo test --workspace --exclude coupling-host` 通过；`cargo test --workspace` 当前因 `coupling-host` 正在开发（`src/lib.rs` 引用的 `cli/host/runfile/signals/validate` 模块文件尚不存在）而无法编译，与本 Block 无关。

### 已知问题 / 实现说明

1. **`efforts` 只包含 enabled 的 ToWebots 槽**（按 manifest 顺序）。`enabled=false` 的槽既不读取也不输出。
2. **状态语义**：步级失败（等待超时、`effort_step_id≠N`）时所有作用量槽输出 0、`status=Error`；任一 enabled+required 槽不可用时整步清零，原本有效的槽改报 `Error`，不可用槽保留自身原因（`Invalid`/`Stale`/`Missing`/`Error`）；缺 limits 视为配置错误，输出 0、`status=Error`；夹取记 `OutOfRange` 但值仍施加；限变化率不改变 status。
3. **`Error`/`Closed`/`Paused`/`Resetting` 下 `exchange` 不等待模型**，返回全 0；其中 `Error`/`Closed`/`Paused`/`Resetting` 不推进 `step_id`。显式 `reset()` 可从 `Error` 恢复。
4. **日志**使用 `eprintln!`（未引入 logging 依赖），输出 hs_state 变化、每次校验错误与进入 Error 的原因。
5. **新增了冻结接口之外的只读/错误通道方法**：`hs_state()`、`set_closed()`、`try_set_state_*`、`try_write_report`；冻结签名全部保持不变。`set_state_*` 误用时 debug 构建断言、release 构建记录错误并忽略。
6. **`attach` 接受 `Ready`/`Running`/`Paused`**（与 B4 的 F4 一致）；`Error`/`Closed` 直接失败并带上 report/hello 内容。
7. **`reset` 清 m2s 残留**最多排空 1024 次零超时等待；Windows 自动复位事件与 Linux 计数信号量都适用。
8. **CLI `--reset-at K` 在步 K 之前复位**并把 plant 回到初值；CSV 的 `t` 从 0 开始，复位后 `t` 继续累加（协议未要求重置仿真时间）。
9. 新增依赖 `serde_json`（workspace 已有，B1 也在用），仅用于 manifest/report 的 JSON 读写，不属于重量级依赖。

### 对契约的疑问 / 需要协调

1. **B9 的共享向量文件在波次 1 未落地。** B9 文档规定 `tests/vectors/effort_validation.json` 由 B9 负责人先定稿，但仓库中只有 `names.json`；B6 文档又要求 B6 导出该文件。为不阻塞验收，B6 按 B9 示例格式创建了该文件，并扩展了示例未覆盖的字段：
   - 用例级：`value_type`（`f64`/`vec3f64`）、`model_timeout_ms`、`max_consecutive_misses`、`required`。
   - 步级：`validity`、`effort_step_id_ok`、`respond`；`effort` 允许字符串 `"nan"`/`"inf"`/`"-inf"`（JSON 无 NaN 字面量）和三元数组。
   - 期望：`expect.value`、`expect.status`、`expect.model_timed_out`、`expect.hs_state`。
   请 B9/B8 负责人确认这套 schema；若 B9 决定改名/改结构，B6 的 `tests/vectors.rs` 需要同步（代码改动很小）。
2. **§10 未钉死"缺 limits"和"required 槽不可用导致 miss"时其它槽的 `status`。** B6 取 `Error`（缺 limits / 步级超时）与保留原因为准（required 槽 invalid 时该槽仍为 `Invalid`）。B8 必须采用同一解释才能通过共享向量；建议协议负责人在 §10 补一句。
3. **`set_state_*` / `write_report` 冻结签名没有错误通道。** B6 保留原签名并新增 `try_*`；这不是契约变更，但 B8 若需要严格错误处理可参考同一做法。



## 评审（B1/协议负责人，2026-10-31）

**结论：验收通过，附 3 个跟进项和 1 条已解决的契约问题。** 这是目前质量最高的一个 Block：接口按冻结签名实现，每一条验收项都有对应测试，故障注入覆盖完整。

### 我独立复核了什么
- `cargo test -p mock-webots`（单元 6、api 1、cli 1、faults 7、lockstep 1、reset 2、vectors 1，共 19 个）全部复现通过；clippy `-D warnings`、`fmt --check` 干净。
- 通读 `exchange()`/`reset()`：与协议 §7 逐步对照一致——先写全部 ToModel 值再写 validity；`sim_time/dt/heartbeat` 之后最后 Release 写 `step_id` 再唤醒；等待时忽略虚假唤醒；复位时先清 validity、`step_id/effort_step_id` 清零、epoch 加一、**唤醒模型**并轮询 ack、排空残留 m2s、回到 `Ready`（不直接 `Running`）；`enabled=false` 的槽不读不写。
- **纯 Rust 全链路联调**（一次性脚本，未入库，`C:\Users\Public\janus_b6_rust_loop.py`）：B5 `coupling-host run`（spawn B4 SDK 的 `echo_model`）+ B6 `mock-webots` 作适配器，2000 步；`omega` 末值 181.277，对解析解 `(τ/b)(1−e^{−bt/J})=181.269` 误差 < 0.005%，host 与 mock-webots 均以 0 退出，状态序列 `Ready→Running→Closed` 正确。B2/B3/B4/B5/B6 五个 Block 首次一起跑通。
- **与 B8 的一致性**：我把共享向量 `tests/vectors/effort_validation.json`（19 个用例）接入了 B8 的 Python 实现。第一次运行**失败**——B8 与 B6 对两件事理解不同（见下面"已解决的契约问题"）；B8 已按 B6 的语义修正，现在 19 个用例在 Python 里也全部通过（B8 共 61 个测试）。Rust 与 Python 两个适配器侧实现现在在效力值校验上行为一致。

### 已解决的契约问题（你们的疑问 2）
B6 取的语义是对的，已写入协议 §10 并成为规范：
1. **槽级状态**：不可用槽的 status 取自身原因；非有限值 → `Invalid`；缺少/非法 limits → `Error`。
2. **miss 让整个实例这一步静默**：步级失败时每个槽 `Error`；槽级失败时其余可用槽改报 `Error`、导致 miss 的槽保留自己的原因。理由：一个实例的多个作用量互相配合，只施加一部分比全部不施加更危险。

### 对你们的另外两个疑问
- **向量 schema（疑问 1）：确认采用。** 请把 `schema_version: 1` 视为 v1 契约；B9 负责人只需接手维护，不用重新定稿。B8 已按此 schema 消费（含 `"nan"/"inf"` 字符串、Vec3、`respond`/`effort_step_id_ok`、`expect.model_timed_out/hs_state`）。
- **`set_state_*`/`write_report` 的 `try_*` 变体（疑问 3）：接受**，与 B2/B4 的做法一致。

### 跟进项（B6 负责人）
- **F1（应修）：`set_running()`/`set_paused()` 会无条件覆盖 `Error`/`Closed`。** 它们直接调用 `store_state`，没有守卫。这违反了协议 §5（适配器只拥有 Ready/Running/Paused/Resetting 的转换，`Error` 是任何一方都能写的终态）：host 看门狗或崩溃检测写下的 `Error` 会被适配器的 `set_running()` 静默抹掉，host 和模型都不会再知道。B8 的 `set_hs()` 有这个守卫。请在 `set_running/set_paused` 里对 `Error`/`Closed` 直接返回（`reset()` 是显式的恢复路径，保持现状），并补一个测试：状态为 `Error` 时 `set_running()` 之后仍为 `Error`。
- **F2（小）：`reset()` 可以把 `Closed` 复活。** 对 `Closed` 的实例调用 `reset()` 会写成 `Resetting`→`Ready`。B8 在这种情况下直接报错。建议 `reset()` 在 `Closed` 时返回 `MasterError`。
- **F3（信息，不需要改）：`exchange` 在 `Paused` 下返回全 0 且不推进步；B8 在 `Paused` 下调用 `exchange` 会隐式恢复为 `Running`**（因为 Webots GUI 里的暂停适配器根本不知道，而 `pause()`/`resume()` 是 MCP 工具发起的）。B6 作为"严格的参考适配器"保持现状即可，B9 不要拿 `Paused` 下的行为做一致性断言。

### 对 B9 的影响
- `effort_validation.json` 已经是共享契约，两侧都有消费测试；B9 只需补一条检查"B6 导出的文件与 B8 读取的文件是同一份"（目前是同一路径）。
- 建议把上面那次 Rust 全链路联调固化为 IT-1，把 B8 的 `tests/` 里的 FakeHost/FakeModel 联调固化为 IT-6。

## 评审跟进（B6 负责人，2026-11-01）

- **F1 已修**：`set_running()` / `set_paused()` 现在遇到 `Error`/`Closed` 直接返回并记录日志，不再覆盖终态；`set_closed()` 保持可写（允许从 `Error` 正常关闭）。新增 `tests/api.rs::terminal_states_are_not_overwritten_by_running_or_paused`，覆盖 `Error` 与 `Closed` 两种终态下 `set_running()`/`set_paused()` 均不改变状态。
- **F2 已修**：`reset()` 在 `Closed` 时返回 `MasterError::State { state: Closed, .. }`，不写 `Resetting`、不改 epoch/`step_id`。新增 `tests/api.rs::reset_on_closed_instance_is_rejected_without_side_effects`。从 `Error` 调用 `reset()` 仍是显式恢复路径，行为不变。
- **F3 确认不改**：`exchange` 在 `Paused` 下返回全 0 且不推进步，作为严格参考适配器保留现状。
- 复核：`cargo fmt -p mock-webots -- --check`、`cargo clippy -p mock-webots --all-targets -- -D warnings`、`cargo test -p mock-webots`（api 3 个，共 21 个）全部通过。
