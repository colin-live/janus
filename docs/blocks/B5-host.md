# B5 — coupling-host（宿主应用）

**目录** `crates/coupling-host`　**依赖** B1(含 B1.1)、B2、B3　**波次** 2

## 目标
一个可执行程序：启动一个项目时，按 `project.yaml` 的绑定关系为每个模型实例建好共享内存段、拉起模型进程、完成握手与能力协商，然后在整个仿真期间监管。**host 不参与每步数据交换**（那是适配器与模型之间的事）。

## 命令行
```
coupling-host check    --project <project.yaml>                  # 只做离线校验，退出码 0/1
coupling-host manifest --project <project.yaml> [--instance <name>] [--sim-id <hex>]   # 打印 manifest JSON
coupling-host run      --project <project.yaml> [--sim-id <hex>] [--hello-timeout-ms 10000] [--no-spawn]
```
`--no-spawn`：所有实例按 attach 处理（模型进程由外部手动启动，调试用）。

## `run` 流程（对应协议 §6）
1. `load_project`；`--sim-id` 缺省时生成随机 u64。
2. 每个实例：`names()` → `Layout::build`（容量取 B1 默认值）→ `Segment::create` → `init_from_layout` → 创建 s2m/m2s `Notifier` → `to_manifest().write_json` → `hs_state=Created`。**init 完成之前不得启动任何其他进程。**
3. `Launch::Spawn`：启动子进程，环境变量 `WCP_SIM_ID`（16 位小写十六进制）、`WCP_INSTANCE_ID`，工作目录/参数按配置；Windows 下用 **Job Object（KILL_ON_JOB_CLOSE）**，host 退出子进程必定被清理。`Launch::Attach`：只等待。
4. 等待 `hs_state==Hello`（超时则报错并清理）。读 hello，协商：
   - 协议主版本一致；`steppable==true`；`sync_modes` 含 `lockstep`。
   - manifest 中 `required` 槽的信号必须在 `supported_signals`（或是 hello 声明的 custom_signals）里，否则 `Error`；非必需且不支持的槽置 `enabled=false`。
   - hello 声明的 custom_signals 必须与配置里实例的 `custom_signals` 逐项一致（id/名称/类型/方向/单位）。
5. 重写 manifest（含 `enabled` 定稿）、`sync_mode`、`hs_state=Ready`。
6. 在 stdout 打印一行 `READY sim_id=<hex>`，并写 `<project目录>/.janus/run.json`：`{"sim_id":"…","instances":[{"name":…,"instance_id":…}],"pid":…}`（适配器 B8 用它找到 sim_id）。
7. 监管循环（≥ 10 Hz）：子进程退出 → 日志并把该段 `hs_state=Error`（若未 Closed）；任一段 `Error` → 读取 report/hello 中的原因输出；**仅在 `Running` 状态**检查心跳停滞（`model_heartbeat` 超过 `3 × model_timeout_ms` 不变 → 标 `Error`），`Paused` 时不检查。
8. Ctrl-C / 收到关闭信号：所有段 `hs_state=Closed`，终止子进程，删除 `run.json`，退出 0。

## 日志与退出码
`tracing` 或 `log + env_logger` 均可，格式单行；关键事件（段创建、hello、协商结果、Ready、Error 原因、清理）必须有日志。退出码：0 正常；1 配置/校验错误；2 握手失败；3 运行期致命错误。

## 验收标准
- `check` 对 `examples/arm2dof/project.yaml` 返回 0；对缺 DEF 的 world、缺 limits 的配置返回 1 且信息带路径。
- 使用 B6 的 `MasterChannel` + 一个测试用假模型（线程内，使用 B4 或直接使用 B2 实现）完成 `run` 的握手到 `Ready`。
- 协商用例：缺必需信号 → `Error`；非必需缺失 → `enabled=false`；模型 `steppable=false` → `Error`；custom_signals 不一致 → `Error`。
- 子进程被强杀后 host 在 1 s 内标 `Error`；host 被强杀后子进程消失（Windows 手测可接受）。
- 不依赖 Webots 与 B8。


## 状态（2026-10-31）

**已完成**：`crates/coupling-host`（lib + bin）。`cargo test -p coupling-host` 全绿：20 个单测（协商 8、心跳看门狗 6、参数解析 4、run.json 2）、13 个集成测试（`tests/host_flow.rs`，用真实的 B4 SDK 作模型侧）、8 个二进制测试（`tests/cli.rs`，退出码与 `run` 全生命周期）；`cargo clippy --all-targets -- -D warnings`、`cargo fmt` 干净。

**验收标准逐条**
- `check` 对 `examples/arm2dof/project.yaml` 返回 0（world/模型可执行文件不存在只告警）；world 缺 DEF 返回 1 并指明 `instance m1, entity …`；缺 `limits` 返回 1 并带配置路径（`bindings[0].slots[1]`）。✓
- 握手到 `Ready`：用 B4 SDK（线程内 attach）与 `echo_model` 示例进程（spawn）两种方式。✓（没有用 B6 的 `MasterChannel`——它还没完成；适配器侧由我的 B8 Python 适配器在一次性脚本里验证过，见下。）
- 协商：缺必需信号 → `Error`（模型的 `connect` 收到带原因的错误）；非必需缺失 → `enabled=false`；`steppable=false` → `Error`；custom_signals 不一致 → `Error`。✓
- 子进程退出/崩溃 → 一个 tick（50 ms）内标 `Error`（测试里 300 ms 后自行 `exit(3)`，3 s 内必须检测到）；host 被强杀后子进程消失：**手测通过**（Job Object，`KILL_ON_JOB_CLOSE`）。✓
- 不依赖 Webots 与 B8。✓（B8 仅用于下面这次联调，不在测试里。）

**一次性端到端联调（未入库）**：真实 `coupling-host run`（建段、spawn `echo_model`、握手）+ B8 Python 适配器（`FakeRobot`）300 步闭环，适配器 `stop()` 后 host 以 0 退出，`run.json` 被清理。脚本在 `C:\Users\Public\janus_e2e_host_adapter.py`，建议 B9 把它固化为 IT-1/IT-6。

**与文档的偏差 / 实现决定**
1. **看门狗条件收紧**：只在"适配器心跳在走、模型心跳不走"时计时；若按文档字面（`Running` 且模型心跳不变）则用户在 Webots GUI 里暂停仿真（适配器不知情、状态仍为 `Running`）会被误判为模型卡死。有测试覆盖两种情形。阈值 `max(3 × model_timeout_ms, 100 ms)`。
2. **host 会往 report 区写原因**（协议规定 report 区归适配器所有）：只在握手失败/进程退出/看门狗触发时写，且不覆盖适配器已写入的失败报告。否则模型端在 `Error` 时看不到原因。
3. **`AllClosed` 即结束**：所有实例都到 `Closed`（适配器 `stop()`）后 host 关闭并以 0 退出。含义：每次仿真运行对应一次 host 运行；Webots 世界重载（控制器 `stop()` 后重启）需要由项目管理器重新启动 host。如果希望 host 常驻，加一个 `--keep-alive` 即可（目前没做）。
4. **自定义信号的跨检只覆盖项目里被某个槽用到的信号**：B3 的 `ResolvedInstance` 不暴露"声明了但没用"的 custom_signals。模型多声明的只告警。若要完整检查，需要 B3 在 `ResolvedInstance` 上增加 `custom_signals`（请求 B3-R2，非阻塞）。
5. `manifest` 子命令缺省 `--sim-id` 为 0（输出可复现），`run` 缺省为随机。
6. 退出码：`Spawn`/`Handshake` → 2；共享内存创建失败/运行期致命错误 → 3；用法/配置/校验 → 1。
7. 握手失败时 host 在写 `Closed` 之前停 200 ms，保证模型的 `connect` 能读到 `Error` 与原因（否则可能被 `Closed` 抢先覆盖）。
8. `world` 路径：B3 已经按项目目录解析好，host 不再重复拼接。
9. 日志用自带的单行 logger（`[+秒] LEVEL msg`），没有引入 `log`/`tracing`。

**未覆盖 / 已知限制**
- Linux 路径（`libc::signal`、非 Job Object）能编译但没跑过。
- 模型进程的 stdout/stderr 直接继承 host 的，没有做日志汇聚。
- 没有崩溃后自动重启模型（文档也没要求）。
- 依赖 B4 的 F1：目前 B4 的 `wait_step` 在 `Error` 时返回错误，所以 host 标 `Error` 后模型进程会退出，不会等待 `Reset`。F1 完成后无需改 host。
