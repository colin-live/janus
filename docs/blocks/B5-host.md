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
