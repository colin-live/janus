# B2 — coupling-shm（命名共享内存与唤醒）

**目录** `crates/coupling-shm`　**依赖** B1　**波次** 1

## 目标
把 OS 差异封装掉：命名共享内存段、命名唤醒事件、带内存序的安全访问器。不含任何协议状态机（状态机在 B4 与 B6/B8）。

## 冻结接口（Wave 2 的 B4/B5/B6 按此编码）
```rust
pub struct SegmentNames { pub segment: String, pub s2m: String, pub m2s: String }
pub fn names(sim_id: u64, instance_id: u32) -> SegmentNames;      // 见协议 §2

pub struct Segment { /* owns mapping */ }
impl Segment {
    pub fn create(name: &str, size: usize) -> Result<Segment, ShmError>;   // host；已存在则报错
    pub fn open(name: &str) -> Result<Segment, ShmError>;                  // client/adapter
    pub fn len(&self) -> usize;
    pub fn header(&self) -> &SegmentHeader;                                // 动态字段是原子，可直接 load/store
    pub fn init_from_layout(&self, l: &Layout, sim_id: u64, instance_id: u32, model_timeout_ms: u32) -> Result<(), ShmError>;
    pub fn read_validity(&self, off: u64) -> Validity;                     // 未知字节 → Error
    pub fn write_validity(&self, off: u64, v: Validity);
    pub fn read_bytes(&self, off: u64, out: &mut [u8]);                    // 热路径：越界时 out 清零、不 panic（调用方须在连接时校验边界）
    pub fn write_bytes(&self, off: u64, data: &[u8]);                      // 热路径：越界为 no-op
    pub fn try_read_bytes(&self, off: u64, out: &mut [u8]) -> Result<(), ShmError>;   // 有错误通道的版本（已实现，现纳入冻结接口）
    pub fn try_write_bytes(&self, off: u64, data: &[u8]) -> Result<(), ShmError>;
    pub fn read_f64(&self, off: u64) -> f64;  pub fn write_f64(&self, off: u64, v: f64);
    pub fn read_json(&self, area: JsonArea) -> Result<Vec<u8>, ShmError>;  // JsonArea::{Manifest, Hello, Report}
    pub fn write_json(&self, area: JsonArea, json: &[u8]) -> Result<(), ShmError>;  // 4 字节长度前缀，超容量报错
}
pub struct Notifier { /* auto-reset binary event */ }
impl Notifier {
    pub fn create(name: &str) -> Result<Notifier, ShmError>;
    pub fn open(name: &str) -> Result<Notifier, ShmError>;
    pub fn signal(&self);
    pub fn wait(&self, timeout: Duration, spin: Duration) -> WaitResult;   // 先自旋 spin 再阻塞
}
pub enum WaitResult { Signaled, TimedOut }
```
`ShmError` 实现 `std::error::Error`，区分 NotFound / AlreadyExists / TooSmall / Os(code)。

## 详细需求
1. **Windows**：`CreateFileMappingW`/`OpenFileMappingW`（`Local\` 命名空间）、`CreateEventW`（自动复位）。用 `windows-sys`，不引入 `shared_memory` crate。
2. **Linux（CI）**：`shm_open` + `mmap`，命名信号量；创建者在 `Drop` 时 `unlink`。用 `libc`。
3. 通知语义：**Windows 为自动复位二值事件**（多次 `signal` 在一次 `wait` 前合并）；**Linux 允许信号量计数语义**（见协议 §2"唤醒语义"）。协议层靠 `step_id` 处理，不依赖计数，也不依赖合并。`read_f64`/`write_f64` 越界 panic 属于编程错误，调用方在连接时已按槽表做过边界校验（B4/B6 的要求）。
4. `wait` 的自旋阶段只用 `std::hint::spin_loop` 轮询一个本地可检查条件——如果 OS 层没有可轮询条件，则直接退化为阻塞等待；对外行为必须一致。
5. `init_from_layout` 清零整个段，写入 header 静态字段（magic、版本、各区偏移、槽数量），`hs_state=Created`。
6. 所有 `unsafe` 带 `// SAFETY:`；映射指针按 8 字节对齐；对象析构时释放句柄。
7. 不解析 JSON，不理解 manifest 内容。

## 验收标准
- 同进程内：`create` + `open` 两个句柄互相可见；跨进程测试（`std::process::Command` 拉自身测试二进制）读写一致。
- 事件：A 线程/进程 `signal`，B `wait` 在 1 ms 内返回 `Signaled`；无信号时 `wait(10ms)` 返回 `TimedOut` 且耗时误差 < 5 ms。
- `Segment::open` 对不存在的名字返回 `NotFound`；重复 `create` 返回 `AlreadyExists`。
- JSON 区：写入超容量返回 `TooSmall`；读空区返回空 Vec。
- 微基准（`cargo bench` 或测试内计时，仅报告）：`signal`→`wait` 往返延迟 p50/p99。

## 状态

**完成日期** 2026-10-30

### 已完成

`docs/blocks/B2-shm.md` 的冻结接口全部实现（`SegmentNames`/`names`、`Segment`、`Notifier`、`WaitResult`、`ShmError`），未改动任何冻结签名。

- Windows：`CreateFileMappingW`/`OpenFileMappingW`/`MapViewOfFile`（`Local\` 命名空间），`CreateEventW`（自动复位，`bManualReset=FALSE`），只用 `windows-sys`；`Drop` 关闭句柄。
- Linux：`shm_open` + `ftruncate` + `mmap`，`munmap`/`shm_unlink` 由创建者 `Drop` 负责；事件用命名 POSIX 信号量（`sem_open`/`sem_post`/`sem_timedwait`），创建者 `sem_unlink`。
- 所有 `unsafe` 带 `// SAFETY:`；映射基址为页对齐（≥ 8 字节），`header()` 断言段长 ≥ 256 且调试期校验对齐；`Segment`/`Notifier` 显式 `Send + Sync`。
- `init_from_layout` 清零整段、写静态字段、`magic` 最后写、`hs_state=Created` 以 Release 发布。
- 访问器：`read/write_validity`（Acquire/Release，未知字节 → `Error`）、`read/write_bytes`（越界不 panic，见已知问题 3）、`read/write_f64`、`read/write_json`（4 字节小端长度前缀；先写负载后写前缀；超容量 `TooSmall`）。
- `try_read_bytes` / `try_write_bytes` 提供带错误通道的版本，已按评审意见正式纳入冻结接口。
- 名字严格按协议 §2 的逐字节规范生成：`sim_id` 16 位小写十六进制左补零、`instance_id` 十进制无前导零、Windows 前缀 `Local\`、Linux 前缀 `/`，事件名在段名后加 `.s2m` / `.m2s`。名字构造抽成平台无关的 `build(sim_id, instance_id, prefix)`，golden 测试在同一进程内同时断言 Windows 与 Linux 两套前缀对 `tests/vectors/names.json` 的逐字节一致性（与 B8 Python 适配器共用同一份数据）。

### 验收对照

| 验收项 | 覆盖测试 |
|---|---|
| 同进程 create + open 互相可见、读写一致 | `tests/segment.rs::create_and_open_share_bytes_and_header` |
| 跨进程（`Command` 拉自身测试二进制）读写一致 | `tests/cross_process.rs::cross_process_handshake_and_step` + `child_process_worker`（open 段、校验 header/manifest、读状态、写作用量、回信号） |
| `signal` → `wait` 1 ms 内 `Signaled` | `tests/notifier.rs::signal_wakes_another_thread_within_1ms`、`signal_before_wait_returns_immediately` |
| 无信号 `wait(10ms)` → `TimedOut`，误差 < 5 ms | `tests/notifier.rs::wait_times_out_with_sub_5ms_error`（断言 5–15 ms） |
| `open` 不存在 → `NotFound`；重复 `create` → `AlreadyExists` | `segment.rs`/`notifier.rs` 的 `open_missing_*`、`duplicate_create_is_rejected` |
| JSON 超容量 → `TooSmall`；空区 → 空 Vec | `tests/segment.rs::json_capacity_is_enforced`、`json_areas_roundtrip_and_share` |
| 名字逐字节符合协议 §2 的 golden 向量（Windows / Linux 两套前缀） | `src/names.rs::build_matches_shared_golden_vectors_for_both_prefixes`、`names_uses_the_platform_prefix_from_the_golden_vectors`（数据：`tests/vectors/names.json`） |
| 微基准 p50/p99（仅报告） | `tests/bench_notify.rs`（`#[ignore]`） |

- 单元测试 4（其中 2 个是 golden 名字用例）、segment 13、notifier 9、跨进程 1、基准 1（ignored）；Windows 上 `cargo test -p coupling-shm` 全绿（完成时 `cargo test --workspace` 亦全绿；此后其它 Block 的未提交改动可能影响 workspace 构建，与 B2 无关），`cargo clippy -p coupling-shm --all-targets` 无警告，`cargo fmt` 已跑。
- Linux 仅静态验证：本机无 Linux 运行环境，`cargo check -p coupling-shm --target x86_64-unknown-linux-gnu --all-targets` 通过；Linux 测试尚未实际执行（需 CI 确认）。
- 微基准（本机 Windows，20 000 次同线程 signal→wait）：p50 = 300 ns，p90 = 300 ns，p99 = 300 ns，max = 12 µs。命令：`cargo test -p coupling-shm --test bench_notify -- --ignored --nocapture`。
- 超时精度实现说明：`WaitForSingleObject` 的超时会被系统时钟 tick 向上取整（本机实测 `WaitOne(10ms)` 可能 20.6 ms、`WaitOne(50ms)` 约 62–65 ms），直接用它无法满足 "< 5 ms 误差"。因此 `wait` 只阻塞 `remaining − 20 ms` 的部分，最后 20 ms 用 `yield_now()` 轮询到截止时间；`wait(10ms)` 实测误差 < 5 ms。

### 已知问题 / 平台差异

1. **Linux 唤醒是计数语义，不是严格二值（协议 §2 已允许）。** Windows 自动复位事件天然合并；Linux 命名信号量是计数对象，为不丢唤醒实现**不**在 `wait` 后 drain，因此 N 次 `signal` 可能让 N 次 `wait` 立即返回。协议 §2 已明确只要求"至少一次唤醒、不丢信号"，不要求二值合并，接收方靠 `step_id`/`effort_step_id` 容忍多余唤醒（§7）；Lockstep 下 `wait` 返回后排空多余信号是可选优化，不是要求。
2. **Windows `Segment::open` 的长度来源。** 命名文件映射不直接暴露大小：`len()` 优先读 header 的 `total_size`（要求 magic 有效），否则退化为 `VirtualQuery` 的 `RegionSize`（按页/分配粒度向上取整）。前提是 host 在 client `open` 之前完成 `init_from_layout`（协议 §6 的顺序）；若先发布名字再初始化，`len()` 会偏大（不会越界写出映射，但数值不精确）。
3. **`read_bytes` / `write_bytes` 越界策略（评审已采纳）。** 保持冻结签名 `()`：越界读把目标缓冲清零、越界写为 no-op，均不 panic；需要错误通道时用 `try_read_bytes`/`try_write_bytes`（已纳入冻结接口）。`read_f64`/`write_f64` 越界 panic 视为编程错误，B4/B6 在连接时已按槽表校验边界。
4. `Notifier::create` 对已存在名字返回 `AlreadyExists`（文档未规定该行为）；`ShmError::TooSmall` 采用结构体变体 `{ what, needed, have }`（文档只给出了变体名）。
5. Linux 的 `sem_timedwait` 基于 `CLOCK_REALTIME`，系统时间被大幅调整会影响超时；未使用非可移植的 `sem_clockwait`。
6. Windows 事件以 `EVENT_ALL_ACCESS` 打开，且名字在 `Local\` 会话命名空间：跨会话（服务 / 其他桌面）不可见。
7. 创建者 `Drop` 的可见性差异：POSIX 立即 `unlink`（名字立刻消失，已打开者仍可用），Windows 在最后一个句柄关闭后对象才消失。测试已按平台分别断言。

### 对契约的疑问（已由下方评审回复确认，2026-10-30）

1. **`read_bytes` 的错误通道矛盾 → 已确认：采用现有实现。** `read_bytes`/`write_bytes` 保持 `()` 签名（热路径，越界不 panic：读清零、写 no-op），`try_read_bytes`/`try_write_bytes` 正式纳入冻结接口；`read_f64`/`write_f64` 越界 panic 视为编程错误。冻结接口段中自相矛盾的注释已删除。
2. **对象名格式未钉死 → 已确认：写入协议 §2 并给出 golden 用例。** 协议 §2 现已规定逐字节格式，共享数据为 `tests/vectors/names.json`；B2 已补 golden 测试逐条断言 `names()` 输出（Windows 与 Linux 两套前缀），B8 读同一份数据。
3. **Linux 二值语义 → 已确认：保持计数信号量 + `step_id` 容忍。** 协议 §2 明确只要求"至少一次唤醒、不丢信号"，不要求二值合并；接收方靠 `step_id` 判断新数据，可容忍多余唤醒。


## 评审回复（B1 负责人，2026-10-30）

**对契约的疑问**
1. **`read_bytes` 错误通道** → 采纳现有实现。`read_bytes`/`write_bytes` 保持 `()` 签名（热路径，越界不 panic：读清零、写 no-op），`try_read_bytes`/`try_write_bytes` 正式纳入冻结接口；`read_f64`/`write_f64` 越界 panic 视为编程错误，由 B4/B6 在连接时对每个槽做边界校验兜底（协议 §6 第 2 步、B4/B6 文档已写入）。文档里自相矛盾的注释已删除。
2. **对象名格式** → 已写入协议 §2：16 位小写十六进制左补零 + 十进制 instance_id，附两条 golden 用例，数据在 `tests/vectors/names.json`。**请 B2 补一个测试**逐条断言 `names()` 输出（Windows 与 Linux 各自的前缀）；B8 的 Python 实现同一文件断言。
3. **Linux 二值语义** → 不升协议。协议 §2 已改为"至少一次唤醒、不丢信号，不要求合并"；接收方本来就必须靠 `step_id` 过滤。Linux 保持计数语义，不需要 futex。Lockstep 下可在 `wait` 返回后排空多余信号（安全，因为一个方向同一时刻至多一个有效信号），属可选优化，不是需求。

**已知问题**
- 1、3、4、5、6、7 接受，无需改动。6 已在协议 §2 写明"同一登录会话"的限制。
- 2（Windows `open` 的长度）→ 协议 §6 第 1 步已规定"init 完成前不得启动其他进程，`magic` 最后写"，client 对 `NotFound`/`BadMagic` 重试，因此 `len()` 取 header 的 `total_size` 的路径是确定的；`VirtualQuery` 的退化路径只在异常顺序下出现，保留即可。
- `Notifier::create` 对已存在名字返回 `AlreadyExists`、`TooSmall{what,needed,have}` 的结构体变体：接受并视为接口的一部分。

**留给 B2 的小任务**：①`names()` golden 测试（见上）。②无其他变更。
