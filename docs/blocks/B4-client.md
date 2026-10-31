# B4 — coupling-client（模型侧 SDK）

**目录** `crates/coupling-client`（含 `python/`、`include/`）　**依赖** B1、B2　**波次** 2

## 目标
让任何模型进程（自研 Rust/C/C++/Python，或后续 FMU Runner）用几行代码接入：连接、读状态、写作用量、提交。**模型侧的步进协议状态机在这里实现**（协议 §7 "模型"部分）。

## 冻结接口（Rust）
```rust
pub struct ModelClient { /* ... */ }
pub struct SlotHandle(u32);                                    // 连接时解析好偏移，运行期 O(1)
pub struct Step { pub step_id: u64, pub sim_time_ns: i64, pub dt_ns: i64, pub epoch: u64 }
pub enum WaitEvent { Step(Step), Reset { epoch: u64 }, Timeout }   // Reset：不携带步，模型须复位内部状态后调用 ack_reset()
impl ModelClient {
    pub fn connect(sim_id: u64, instance_id: u32, hello: Hello, timeout: Duration) -> Result<ModelClient, ClientError>;
    pub fn manifest(&self) -> &Manifest;
    pub fn slot(&self, name: &str) -> Option<SlotHandle>;
    pub fn wait_step(&mut self, timeout: Duration) -> Result<WaitEvent, ClientError>;  // 自动丢弃虚假唤醒
    pub fn ack_reset(&mut self);                                // 模型复位完内部状态后调用：写 model_epoch_ack
    pub fn read_f64(&self, h: SlotHandle) -> Option<f64>;      // validity != Valid → None
    pub fn read_vec3(&self, h: SlotHandle) -> Option<[f64; 3]>;  pub fn read_quat(&self, h: SlotHandle) -> Option<[f64; 4]>;
    pub fn write_f64(&mut self, h: SlotHandle, v: f64);          // 非有限数 → 记为 Invalid，不写入
    pub fn write_vec3(&mut self, h: SlotHandle, v: [f64; 3]);
    pub fn invalidate(&mut self, h: SlotHandle, why: Validity);
    pub fn commit(&mut self) -> Result<(), ClientError>;        // heartbeat++、写 effort_step_id(Release)、signal m2s
}
```
## C ABI（`include/wcp_client.h`，名字冻结）
`wcp_connect, wcp_close, wcp_manifest_json, wcp_slot_by_name, wcp_wait_step, wcp_ack_reset, wcp_read_f64, wcp_read_vec3, wcp_read_quat, wcp_write_f64, wcp_write_vec3, wcp_invalidate, wcp_commit`；返回码：0 成功、1 超时/值不可用、**2 `WCP_RESET`（仅 `wcp_wait_step`：epoch 复位，`*out` 只带新 epoch，模型复位后调 `wcp_ack_reset`、不得 commit）**、**3 `WCP_CLOSED`（仅 `wcp_wait_step`：适配器干净关停，模型应退出）**、<0 错误；错误文本通过 `wcp_last_error()` 获取；`wcp_ack_reset` 无 pending、写槽方向/类型不对、`wcp_invalidate` 目标不是 effort 槽时返回 <0。`crate-type = ["rlib", "cdylib"]`。

## Python 绑定（`python/wcp_client.py`）
用 `ctypes` 包装 C ABI，API 与 Rust 同名（`ModelClient.connect/slot/wait_step/read_f64/write_f64/commit`）。`wait_step` 超时返回 `None`、复位返回 `Reset`（随后调用 `ack_reset()`）、干净关停抛 `Closed`（`WcpError` 子类），其余失败抛 `WcpError`。无第三方依赖。

## 详细需求
1. `connect`：`open` 段（对 `NotFound`/`BadMagic` 在超时内重试）→ 读 header 校验（魔数/主版本）→ 读 manifest，用 `Manifest::slot_specs()` + `Layout::from_header()` 重建布局 → **对每个槽做边界校验**（`value_offset + 大小 ≤ Segment::len()`，否则报错；B2 的 `read_f64` 越界会 panic）→ 写 hello（协议 §9）→ `hs_state=Hello` → 等待 `Ready`/`Running`/`Paused`（超时报错；`Error` 时把 report 或原因带进错误，这是唯一把 `Error` 视为致命的阶段）。
2. `wait_step`：调用 `Notifier::wait`；醒来后**先**读 `epoch`：与已知值不同 → 返回 `WaitEvent::Reset`（不携带步，模型复位后调用 `ack_reset()`，SDK 记下 `last=0`）；再读 `step_id.load(Acquire)`，为 0 或等于上次处理的 id 则继续等；否则返回 `Step`。（协议 §7 的复位流程：适配器复位时会唤醒模型，不依赖下一个步。）
3. `commit`：写 `model_heartbeat`、`effort_step_id.store(step_id, Release)`、`signal(m2s)`；**每次 `WaitEvent::Step` 后恰好 `commit` 一次；`WaitEvent::Reset` 后调用 `ack_reset()`，不得 commit**（此时 `commit` 返回 `NoPendingStep`）；重复 commit 返回错误。
4. 读槽时尊重 validity；对 `enabled=false` 的槽 `slot()` 返回 `None`。
5. 把 manifest/hello 解析放在 B1.1 的类型上，不重复定义。
6. 线程模型：一个 `ModelClient` 非 `Sync`，单线程使用即可。

## 验收标准
- 用 B2 的内存假实现（或真实 B2）+ 一个测试用"主控"线程，跑 10 000 步锁步，`step_id` 无丢失、无重复施加。
- 乱序/虚假唤醒测试：多次 `signal` 不增加步数。
- epoch 变化测试：reset 后 `wait_step` 返回 `Reset`、`ack_reset()` 写入正确 ack，之后 `step_id` 从 1 重新开始被处理。
- C ABI 有一个 C 测试程序（`tests/c_smoke.c`，由 `build.rs` 或脚本编译）读写成功；Python 绑定有 `pytest` 冒烟测试。

## 状态

**完成**：2026-10-31。`crates/coupling-client` 的 Rust API、C ABI 与 Python 绑定均已实现；验收项对照如下。

### 验收对照

- **10 000 步锁步**：`tests/lockstep.rs::ten_thousand_lockstep_steps_without_loss_or_duplication`。使用真实 B2 段 + 进程内 host/adapter 线程，断言 `step_id` 连续、每步效力恰好施加一次（约 0.2 s）。
- **虚假/重复唤醒**：`repeated_signals_do_not_advance_the_step`。额外 3 次 `signal` 不产生新步，随后第 2 步正常。
- **epoch 复位**：`epoch_reset_is_reported_acked_then_step_ids_restart_at_one`。`wait_step` 返回 `Reset { epoch }`（不携带步，`step_id`/`effort_step_id` 保持 0）→ 复位后 `commit()` 返回 `NoPendingStep` → `ack_reset()` 写入 ack → 新 epoch 的 `step_id` 从 1 重新开始，且模型内部状态已清零。
- **C 冒烟**：`tests/c_smoke.c` + `tests/run_c_smoke.ps1`（Linux 对应 `run_c_smoke.sh`）。5 步与 1000 步均通过，输出 `C_SMOKE_OK steps=...`；程序也处理 `WCP_RESET`（ack 后不增加步数）。
- **Python 冒烟**：`python/test_wcp_client.py`。2 个 pytest 用例通过（加载真实 cdylib，`examples/mock_host` 作为独立进程）。
- **其他 API**：`tests/api.rs` 覆盖 commit 时序（重复 commit / 未 commit 再 wait 均报错）、`enabled=false` 槽不可见、非有限数 → `Invalid` 且不写值。

### 验证命令（全部通过）

- `cargo fmt -p coupling-client -- --check`
- `cargo test -p coupling-client`（api 3、lockstep 3、doctest 1）
- `cargo clippy -p coupling-client --all-targets -- -D warnings`
- `cargo check -p coupling-client --target x86_64-unknown-linux-gnu --all-targets`
- `cargo test --workspace`
- `powershell -File crates/coupling-client/tests/run_c_smoke.ps1 -Steps 5` 与 `-Steps 1000`
- `python -m pytest crates/coupling-client/python`

### 与冻结接口的偏差 / 契约缺口（需文档负责人确认）

1. **C ABI 扩展了返回码与函数名**。复位必须让 C/Python 模型也能确认 epoch，否则适配器会一直等待 `model_epoch_ack`。因此新增：
   - `#define WCP_RESET 2`：`wcp_wait_step` 在 epoch 变化时返回，`*out` 只携带新 `epoch`（`step_id == 0`、`epoch_changed == 1`）；模型复位内部状态后调用 `wcp_ack_reset`，且不得 commit。
   - `int32_t wcp_ack_reset(wcp_client *)`：写 `model_epoch_ack`。

   这超出了本文件第 29 行“返回 0/1/<0”与冻结函数名列表；Rust 侧的 `WaitEvent`/`ack_reset` 属于已更新的冻结接口，两者语义一致。若必须保持 C 返回码严格为 0/1/<0，请把 `WCP_RESET` 正式纳入文档，或改用独立的“是否有复位”查询函数。
2. **详细需求 3 的措辞已过时**。第 37 行“每次 `wait_step` 后必须恰好 `commit` 一次”不适用于 `Reset`：当前实现要求 `Reset` 后调用 `ack_reset()`、不得 `commit`（`commit` 返回 `NoPendingStep`）。建议改为“每次 `Step` 后恰好 commit 一次；`Reset` 后 ack_reset”。
3. **`ack_reset()` 无 pending reset 时是 no-op**（debug 构建触发 `debug_assert!`）：冻结签名没有错误通道。C ABI 的 `wcp_ack_reset` 同样返回 0。

### 已知限制 / 实现说明

- `connect` 对 `NotFound`、`BadMagic`、空 manifest 在同一个 deadline 内重试，随后等待 `Ready`/`Running`/`Paused`；校验请求/header/manifest 的 id 一致、`sync_mode == lockstep`、槽方向/类型及槽边界（`value_offset + 类型大小 ≤ Segment::len()`）。
- `write_f64`/`write_vec3` 写错方向或类型时，冻结签名保留 `debug_assert!` 并忽略；需要检测误用请用 `try_*`（返回 `WrongSlot`），FFI/Python 已改用 `try_*` 并返回错误。非有限数只把 validity 置 `Invalid`，不写值。
- `wait_step` 只在 `hs_state == Closed` 时返回 `ClientError::Closed`；`Error` 可恢复（等待 reset 并 ack），`connect` 阶段仍视为致命。`hs_state()` 暴露当前握手状态供模型记录。
- Windows 上 `Notifier::wait` 的定时精度受系统调度影响，超时为尽力而为。
- Python 绑定按 `WCP_CLIENT_LIB` 或工作区 `target/{debug,release}` 查找 cdylib；测试 fixture 先 `cargo build -p coupling-client`（生成 cdylib）再构建 `mock_host`。
- C 冒烟脚本在 Windows 需要 gcc；部分 MSYS2 把 `libgcc_s_seh-1.dll` / `libwinpthread-1.dll` 放在 `.../ucrt64/bin/cpp`，脚本已自动把该目录加入 `PATH`。
- 运行 pytest 会在仓库根生成 `.pytest_cache/`（未提交）。


## 评审（B1/协议负责人，2026-10-31）

**结论：验收通过，附 4 个小的跟进项（不阻塞 B5/B7 开工）。**

### 我独立复核了什么
- `cargo test -p coupling-client`（api 3、lockstep 3、doctest 1）、`cargo clippy -p coupling-client --all-targets -- -D warnings`、`python -m pytest crates/coupling-client/python`（2 passed）、`run_c_smoke.ps1 -Steps 1000`（`C_SMOKE_OK`）——全部在我这边复现通过。
- **跨实现互操作**（一次性脚本，未入库）：B8 的 Python 适配器 + Python 版 FakeHost 作主控，B4 的 Rust SDK（经其 Python 绑定）作模型。结果：布局（Python 独立实现的偏移 vs `Layout::from_header`）一致；100 步锁步 `step_id` 无丢失；NaN 效力被适配器拦截并清零后恢复；`dt_ns`/`sim_time_ns` 传递正确；reset 后 SDK 返回 `Reset{epoch:1}`、`ack_reset()` 被适配器接受、`step_id` 从 1 重新开始。这是 Rust/Python 两个独立实现首次互通，建议 B9 把它固化为 IT-6。
- 通读 `client.rs`、`ffi.rs`、`wcp_client.h`：epoch 先于 step_id 检查 ✓；value 先于 validity、`effort_step_id` 最后 Release、再 signal ✓；heartbeat ✓；所有导出函数 `catch_unwind` + 空指针检查 + `// SAFETY` ✓；连接时槽边界校验 ✓；`connect` 对 NotFound/BadMagic/空 manifest 重试 ✓。

### 对你们 3 个"契约缺口"的回复
1. **C ABI 的 `WCP_RESET`(=2) 与 `wcp_ack_reset`：采纳，已写入本文第 29 行。** 没有它们 C/Python 模型无法在复位时 ack，适配器会一直等到超时，所以这是必须的，不是偏差。
2. **需求 3 措辞：已改**（第 37 行）：每次 `Step` 后恰好 commit 一次；`Reset` 后 `ack_reset()`，不得 commit。
3. **`ack_reset()` 无 pending 时 no-op：Rust 签名保持不变（接受），但 C ABI/Python 不应静默成功**——见下面跟进项 F2。

### 跟进项（B4 负责人，小改动）
- **F1（协议一致性，建议优先做）：`Error` 不应让模型退出。** 现在 `wait_step` 在 `hs_state==Error` 时返回 `Err(Remote)`，模型只能退出；但协议 §7/§10 规定适配器在 Error 下可被显式 `reset` 恢复，模型进程一退出，恢复就会因无人 ack 而再次失败。我已在协议 §10 写明：**只有 `Closed` 终止模型，`Error` 仅在 `connect` 阶段致命**。请把 `check_state()` 改为只对 `Closed` 返回错误，并增加 `ModelClient::hs_state() -> HandshakeState` 供模型自行记录/观察；补一个测试：适配器进入 Error 后 reset，模型收到 `Reset` 并恢复。
- **F2：误用不要静默成功。** `wcp_ack_reset`（无 pending）与 `wcp_write_f64/vec3`、`wcp_invalidate`（槽方向/类型不对）在 release 构建里返回 0 且什么也没写，模型作者只会看到适配器那边"required 槽 Missing → miss → Error"。建议给 `ModelClient` 加 `try_ack_reset()`/`try_write_f64()`/`try_write_vec3()`/`try_invalidate()`（返回 `Result`，原签名保留并调用它们），FFI 与 Python 用 `try_*` 并返回 -1。
- **F3：`Closed` 应可区分。** 正常关停时 Python 抛出的是普通 `WcpError('segment closed')`（互操作脚本里就看到了），C 侧只能比对字符串。建议增加返回码 `WCP_CLOSED = 3`（仅 `wcp_wait_step`），Python 绑定返回/抛出专门的 `Closed`，让模型能区分"干净关停"与"出错"。
- **F4：`connect` 接受 `Paused`。** 握手循环只接受 `Ready/Running`；模型在适配器暂停期间重连会一直等到超时。把 `Paused` 也视为可连接即可。
- 极小：`client.rs` 的 `ModelClient` 文档注释里 `Segmentation` 应为 `Segment`。

### 对 B5/B7 的影响
B7 可以直接基于 `WaitEvent`/`ack_reset` 开工（B7 文档里"收到 Closed 或连续超时后退出"与 F1 一致）。F1–F4 不改变冻结接口的名字，只改变错误处理，B7 不需要等。

## 跟进完成（B4 负责人，2026-10-31）

F1–F4 与文档笔误均已处理，冻结接口名字未变。

- **F1**：`check_state()` 只对 `Closed` 返回 `Err(ClientError::Closed)`；`Error` 可恢复。新增 `ModelClient::hs_state() -> HandshakeState`（未知原始值按 `Error` 报告）。新测试 `error_state_does_not_terminate_and_reset_recovers`：适配器进入 `Error` → reset → 模型收到 `Reset`、ack 并重新初始化状态，随后正常继续。
- **F2**：新增 `try_ack_reset()`、`try_write_f64()`、`try_write_vec3()`、`try_invalidate()`（返回 `Result`），错误变体 `NoPendingReset`、`WrongSlot(String)`；原签名保留并委托给 `try_*`（误用仅 `debug_assert!`）。FFI/Python 全部改用 `try_*`，误用返回 `-1`。新测试 `fallible_operations_report_misuse` 与 pytest 误用断言覆盖。
- **F3**：新增 `WCP_CLOSED = 3`（仅 `wcp_wait_step`）；Python 绑定抛 `Closed(WcpError)`。新测试 `tests/ffi.rs` 验证关停返回 `WCP_CLOSED`、误用返回 `WCP_ERROR`；pytest 用假库验证 `WCP_CLOSED → Closed`。
- **F4**：`connect` 接受 `Ready`/`Running`/`Paused`；新测试 `connect_accepts_paused_adapter`。
- 笔误：`Segmentation` → `Segment`。

验证（本机复现）：`cargo fmt -p coupling-client`；`cargo test -p coupling-client`（api 5、ffi 1、lockstep 4、doctest 1）；`cargo clippy -p coupling-client --all-targets -- -D warnings`；`cargo check -p coupling-client --target x86_64-unknown-linux-gnu --all-targets`；`python -m pytest crates/coupling-client/python -q`（3 passed）；`run_c_smoke.ps1 -Steps 5/1000`（`C_SMOKE_OK`）；`cargo test --workspace --exclude coupling-host`（全部通过）。`cargo test --workspace` 当前因其他 block 正在开发的 `coupling-host` 缺少模块文件而失败，与本 block 改动无关。
