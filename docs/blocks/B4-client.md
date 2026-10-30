# B4 — coupling-client（模型侧 SDK）

**目录** `crates/coupling-client`（含 `python/`、`include/`）　**依赖** B1、B2　**波次** 2

## 目标
让任何模型进程（自研 Rust/C/C++/Python，或后续 FMU Runner）用几行代码接入：连接、读状态、写作用量、提交。**模型侧的步进协议状态机在这里实现**（协议 §7 "模型"部分）。

## 冻结接口（Rust）
```rust
pub struct ModelClient { /* ... */ }
pub struct SlotHandle(u32);                                    // 连接时解析好偏移，运行期 O(1)
pub struct Step { pub step_id: u64, pub sim_time_ns: i64, pub dt_ns: i64, pub epoch: u64, pub epoch_changed: bool }
impl ModelClient {
    pub fn connect(sim_id: u64, instance_id: u32, hello: Hello, timeout: Duration) -> Result<ModelClient, ClientError>;
    pub fn manifest(&self) -> &Manifest;
    pub fn slot(&self, name: &str) -> Option<SlotHandle>;
    pub fn wait_step(&mut self, timeout: Duration) -> Result<Option<Step>, ClientError>;  // None = 超时；自动丢弃虚假唤醒
    pub fn read_f64(&self, h: SlotHandle) -> Option<f64>;      // validity != Valid → None
    pub fn read_vec3(&self, h: SlotHandle) -> Option<[f64; 3]>;  pub fn read_quat(&self, h: SlotHandle) -> Option<[f64; 4]>;
    pub fn write_f64(&mut self, h: SlotHandle, v: f64);          // 非有限数 → 记为 Invalid，不写入
    pub fn write_vec3(&mut self, h: SlotHandle, v: [f64; 3]);
    pub fn invalidate(&mut self, h: SlotHandle, why: Validity);
    pub fn commit(&mut self) -> Result<(), ClientError>;        // heartbeat++、写 effort_step_id(Release)、signal m2s
}
```
## C ABI（`include/wcp_client.h`，名字冻结）
`wcp_connect, wcp_close, wcp_manifest_json, wcp_slot_by_name, wcp_wait_step, wcp_read_f64, wcp_read_vec3, wcp_read_quat, wcp_write_f64, wcp_write_vec3, wcp_invalidate, wcp_commit`；返回 0 成功、1 超时/无效值、<0 错误；错误文本通过 `wcp_last_error()` 获取。`crate-type = ["rlib", "cdylib"]`。

## Python 绑定（`python/wcp_client.py`）
用 `ctypes` 包装 C ABI，API 与 Rust 同名（`ModelClient.connect/slot/wait_step/read_f64/write_f64/commit`）。无第三方依赖。

## 详细需求
1. `connect`：`open` 段（对 `NotFound`/`BadMagic` 在超时内重试）→ 读 header 校验（魔数/主版本）→ 读 manifest，用 `Manifest::slot_specs()` + `Layout::from_header()` 重建布局 → **对每个槽做边界校验**（`value_offset + 大小 ≤ Segment::len()`，否则报错；B2 的 `read_f64` 越界会 panic）→ 写 hello（协议 §9）→ `hs_state=Hello` → 等待 `Ready`（超时报错；`Error` 时把 report 或原因带进错误）。
2. `wait_step`：调用 `Notifier::wait`；醒来后 `step_id.load(Acquire)`，等于上次处理的 id 则继续等；`epoch` 变化时设置 `epoch_changed=true` 且**在 `commit` 前自动写 `model_epoch_ack`**（模型需先复位内部状态）。
3. `commit`：写 `model_heartbeat`、`effort_step_id.store(step_id, Release)`、`signal(m2s)`；每次 `wait_step` 后必须恰好 `commit` 一次，重复 commit 返回错误。
4. 读槽时尊重 validity；对 `enabled=false` 的槽 `slot()` 返回 `None`。
5. 把 manifest/hello 解析放在 B1.1 的类型上，不重复定义。
6. 线程模型：一个 `ModelClient` 非 `Sync`，单线程使用即可。

## 验收标准
- 用 B2 的内存假实现（或真实 B2）+ 一个测试用"主控"线程，跑 10 000 步锁步，`step_id` 无丢失、无重复施加。
- 乱序/虚假唤醒测试：多次 `signal` 不增加步数。
- epoch 变化测试：reset 后 `epoch_changed=true` 且 ack 正确。
- C ABI 有一个 C 测试程序（`tests/c_smoke.c`，由 `build.rs` 或脚本编译）读写成功；Python 绑定有 `pytest` 冒烟测试。