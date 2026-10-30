# 01 — 耦合协议 v1.0（冻结契约）

> 这是所有模块共享的**唯一契约**。代码与本文冲突时以本文为准。修改本文必须：①升版本号 ②在 `docs/CHANGELOG-protocol.md` 记一行 ③通知所有 Block 负责人。B1（`coupling-core`）是本文的参考实现。

## 1. 范围与术语

- 只定义 **Webots 适配器（adapter）⇄ 外部物理模型（model）** 的数据交换。上游控制信号 → 模型不在范围内。
- **segment**：每个*模型实例*一个共享内存段（崩溃域隔离、各自超时）。
- **entity**：被绑定的 Webots 对象（Joint 或 Solid），由 `entity_id` 标识，对应一个 DEF 名。
- **slot**：一个信号在段内的固定位置。运行期只传值，不传名字。
- 方向：`ToModel`（适配器写，模型读：状态/观测）；`ToWebots`（模型写，适配器读：作用量）。
- 端序：**小端**；字节对齐：**8**；时间：**i64 纳秒**；单位：**SI**；字符串仅出现在 JSON 区。
- 电压、电流、磁通等电气量**不进入协议必需信号**，只留在模型内部（可选遥测另走）。

## 2. 命名

| 对象 | Windows | Linux |
|---|---|---|
| 内存段 | 文件映射 `Local\wcp.<sim_id_hex>.<instance_id>` | `shm_open("/wcp.<sim_id_hex>.<instance_id>")` |
| 唤醒 s2m（状态就绪，适配器→模型） | 命名事件（自动复位）`…​.s2m` | 命名信号量 `…​.s2m` |
| 唤醒 m2s（作用量就绪，模型→适配器） | 命名事件（自动复位）`…​.m2s` | 命名信号量 `…​.m2s` |

`sim_id` 是 host 启动时生成的 u64 随机数，通过环境变量 `WCP_SIM_ID` 和命令行传给模型进程与适配器。

**名字格式（逐字节规范，两端必须一致）**：`base = "wcp." + sim_id(16 位小写十六进制，左补零) + "." + instance_id(十进制，无前导零)`。Windows：段名 `Local\` + base；事件名 = 段名 + `.s2m` / `.m2s`。Linux：段名 `/` + base；信号量名 = 段名 + `.s2m` / `.m2s`。Golden 用例（B2、B8 都要有测试，数据见 `tests/vectors/names.json`）：

| sim_id | instance_id | Windows 段名 | Linux 段名 |
|---|---|---|---|
| `0x1234abcd5678ef90` | 1 | `Local\wcp.1234abcd5678ef90.1` | `/wcp.1234abcd5678ef90.1` |
| `0x1` | 12 | `Local\wcp.0000000000000001.12` | `/wcp.0000000000000001.12` |

**唤醒语义**：协议只要求"至少一次唤醒、不丢信号"，**不**要求严格二值合并。Windows 事件天然合并；Linux 命名信号量可能累计计数。接收方必须靠 `step_id`/`effort_step_id` 判断是否是新数据，并容忍多余的唤醒（§7 已规定）。Lockstep 下一个方向同一时刻至多有一个有效信号，所以实现在 `wait` 返回后可以安全地排空多余信号，但不是必须。`Async` 模式启用时再重新评估。

**可见范围**：Windows 的 `Local\` 命名空间按登录会话隔离，host、模型、Webots 必须在同一会话运行（服务/其他桌面不支持）。

## 3. 段布局

```
[Header 256B][manifest JSON 区][hello JSON 区][report JSON 区][ToModel 区][ToWebots 区]
```
Header 字段与偏移见 `crates/coupling-core/src/header.rs`（`offsets` 模块是 Python/C 实现的权威偏移表，单测保证与结构体一致）。各区大小由 `Layout::build` 计算，写入 header 的 `*_off/*_len`。

**数据区内部**（两个方向相同）：`[每槽 1 字节 validity，整体补齐到 8][各槽数值，每个补齐到 8 字节]`；槽顺序 = `slot_id` 在该方向上出现的顺序。

**JSON 区**：UTF-8，前 4 字节为 u32 长度 `n`，随后 `n` 字节 JSON；`n=0` 表示空。只有该区的所有者可写（见 §5）。

**ValueType**：Bool=1(1B) I32=2 U32=3 I64=4 F32=5 F64=6 Vec3F64=7(24B) QuatF64=8(x,y,z,w) Mat3F64=9(行主序)。

**Validity**：Missing=0（零内存即缺失，**不是**有效的 0）Valid=1 Invalid=2 Stale=3 OutOfRange=4 Error=5。

## 4. 基础信号注册表（ID 永久稳定）

| ID | 名称 | 类别 | 类型 | 单位 |
|---|---|---|---|---|
| 0x0001 | joint_position | State | F64 | rad（转动）/ m（移动） |
| 0x0002 | joint_velocity | State | F64 | rad/s / m/s |
| 0x0003 | joint_torque_feedback | Observation | F64 | N·m（需实测可用） |
| 0x0010 | body_position | State | Vec3 | m |
| 0x0011 | body_orientation | State | Quat | 1 |
| 0x0012 | body_linear_velocity | State | Vec3 | m/s |
| 0x0013 | body_angular_velocity | State | Vec3 | rad/s |
| 0x0101 | joint_torque | Effort | F64 | N·m |
| 0x0102 | joint_force | Effort | F64 | N |
| 0x0110 | body_force | Effort | Vec3 | N（作用点见绑定） |
| 0x0111 | body_torque | Effort | Vec3 | N·m |

ID 区间：`0x0000–0x0FFF` 基础｜`0x1000` 电气｜`0x2000` 机械扩展｜`0x3000` 热｜`0x4000` 流体｜`0x5000` 液压｜`0x6000` 磁｜`0x8000–0xFFFF` 实例自定义（须在 hello 中声明，且类型/方向/单位完整）。

**关节位置/速度的单位**：基础注册表写 `rad|m` 是因为转动与移动关节共用 ID；**manifest 槽里的 `unit` 必须是单一具体单位**（`rad`、`rad/s` 或 `m`、`m/s`），由配置决定（默认转动；移动关节须显式写 `unit: m`）。适配器在 `start` 时按实际节点类型校验：`HingeJoint`⇔`rad`+`joint_torque`，`SliderJoint`⇔`m`+`joint_force`，不一致写入 report 并置 `Error`。

**坐标约定**：协议中的 `World` 帧固定为 **右手 ENU（Z 向上，重力 −Z）**；适配器负责把 Webots 世界的 `coordinateSystem` 转换过来（v1 支持 `ENU`、`NUE`；`EUN` 为左手系，不支持，报错）。四元数 (x,y,z,w)。关节正方向 = Webots 关节 `axis` 方向；`joint_torque` 沿同一轴为正，对 endPoint 施加、对父体产生反作用（用 `Motor.setTorque` 实现）。

## 5. 所有权（谁能写什么）

| 区域 | 写入者 |
|---|---|
| Header 静态字段、manifest JSON | host（段创建者） |
| `hs_state`：Created→Hello 由 client；Hello→Ready 由 host；Ready/Running/Paused/Resetting 由适配器；Error/Closed 任何一方 | 见左 |
| `epoch`、`step_id`、`sim_time_ns`、`dt_ns`、`adapter_heartbeat`、ToModel 区 | 适配器 |
| `model_epoch_ack`、`effort_step_id`、`model_heartbeat`、ToWebots 区、hello JSON | 模型（client） |
| report JSON | 适配器 |

## 6. 握手（初始化一次）

1. host 解析配置 → `Layout::build` → 创建段（`init_from_layout` 完成，`magic` 最后写）与事件 → 写 header、**manifest**（含全部槽与绑定，见 §8）→ `hs_state=Created`。**init 完成前不得启动模型进程或通知适配器**。
2. host 启动（或等待连接）模型进程；client 对 `open` 的 `NotFound` 与 `BadMagic` 在超时内**重试**，成功后读 manifest、校验协议主版本、对每个槽做边界校验（`value_offset + 大小 ≤ 段长`）、写 **hello**（§9）→ `hs_state=Hello`。
3. host 对照 manifest 的 `required`/`optional` 与 hello 的能力：缺必需信号、模型不可步进（`steppable=false`）、不支持 Lockstep → 写 `Error` + 原因；否则把不被支持的可选槽标 `enabled=false`，定稿 `sync_mode`，`hs_state=Ready`。**槽布局在握手中不再改变。**
4. 适配器连接、校验每个绑定（DEF 存在、类型匹配、轴/电机存在……），写 **report**；失败则 `hs_state=Error`。通过后首次步进时置 `Running`。

## 7. 步进协议（Lockstep，v1.0 必须实现；Async 保留，`sync_mode=1` 一律拒绝）

**适配器，每个 Webots 步 N（从 1 起，每个 epoch 重新计数），在 `robot.step()` 之前：**
1. 写所有 ToModel 槽的值，再写其 validity（`Valid` 或错误状态）。
2. 写 `sim_time_ns`、`dt_ns`，`adapter_heartbeat += 1`。
3. `step_id.store(N, Release)`；触发 s2m。
4. 等待 m2s，最长 `model_timeout_ms`；醒来后 `effort_step_id.load(Acquire)`，不等于 N 视为虚假唤醒，继续等。
5. 读 ToWebots 槽，校验（§10）→ 聚合 → 施加到 Webots → `robot.step()`。

**模型：**
```
loop { wait(s2m); n = step_id.load(Acquire); if n == last { continue }
       if epoch != my_epoch { reinit(); my_epoch = epoch; model_epoch_ack.store(epoch) }
       读状态 → 计算（可内部多子步，但机械反馈只用本步给定值）
       写 ToWebots 槽值 → validity；model_heartbeat += 1
       effort_step_id.store(n, Release); signal(m2s); last = n }
```
保证：同一 `step_id` 的数据属同一仿真步；模型读不到未来步；一份作用量最多施加一次（只有 `effort_step_id == N` 才生效）。

**一步延迟**：作用量基于第 N 步状态算出，在 N→N+1 期间生效（显式耦合）。

**复位（epoch）**：reset/重载世界时适配器：`hs_state=Resetting` → `epoch += 1` → `step_id=0`、`effort_step_id=0`、所有 validity 清为 Missing → 等 `model_epoch_ack == epoch`（超时同 §10）→ `Running`。模型在 epoch 变化时必须复位全部内部状态。

**暂停**：适配器置 `Paused`，host 不得因心跳不变而判定模型/适配器死亡。

## 8. manifest JSON（host 写）

```json
{
  "manifest_version": 1,
  "simulation_id": "0x...", "instance_id": 1, "instance_name": "shoulder_motor",
  "sync_mode": "lockstep", "dt_ns": 1000000, "model_timeout_ms": 50,
  "effort_policy": { "ttl_steps": 1, "on_timeout": "zero", "max_consecutive_misses": 3 },
  "world_frame": "ENU",
  "entities": [
    { "entity_id": 1, "name": "joint", "webots_def": "SHOULDER_JOINT", "kind": "joint" },
    { "entity_id": 2, "name": "link", "webots_def": "UPPER_ARM", "kind": "solid" }
  ],
  "slots": [
    { "slot_id": 0, "entity_id": 1, "signal_id": 1, "name": "joint_position", "value_type": "f64",
      "direction": "to_model", "frame": "none", "unit": "rad", "required": true, "enabled": true },
    { "slot_id": 2, "entity_id": 1, "signal_id": 257, "name": "joint_torque", "value_type": "f64",
      "direction": "to_webots", "frame": "joint_axis", "unit": "N*m", "required": true, "enabled": true,
      "limits": { "max_abs": 5.0, "max_rate_per_s": 2000.0 }, "point": null }
  ]
}
```
`body_force/body_torque` 槽的 `frame`（`world|body_local`）与 `point`（作用点，body_local 坐标，默认质心）必填。关节类 entity 可带可选的 `motor`、`sensor`（Webots 设备名覆盖；缺省时适配器从关节的 `device` 字段自动发现）。

## 9. hello JSON（client 写）

```json
{ "protocol": [1, 0], "model_name": "dcmotor", "model_version": "0.1.0",
  "steppable": true, "sync_modes": ["lockstep"], "internal_dt_ns": 100000,
  "supported_signals": [1, 2, 257],
  "custom_signals": [ { "signal_id": 32769, "name": "dc_bus_voltage", "value_type": "f64",
                        "direction": "to_webots", "unit": "V", "domain": "electrical" } ] }
```

## 10. 有效性、校验与失败策略（适配器侧强制）

对每个 ToWebots 槽：validity≠Valid、含 NaN/Inf、`effort_step_id≠N` 均视为不可用。可用值先按 `limits.max_abs` 限幅（记 `OutOfRange`），再按 `max_rate_per_s` 限变化率，然后施加。
不可用或等待超时 → 按 `on_timeout`：`zero`（默认，显式写 0，因为 `Motor.setTorque` 会保持）。连续 `max_consecutive_misses` 步不可用 → `hs_state=Error` 并停止施加、清零。
作用量聚合：同一 entity、同一类作用量，统一到 World 帧后**求和**；`addForce` 类每步自动失效，`setTorque` 类必须每步显式写。
适配器不得把模型当作可信输入。

## 11. 版本规则

主版本变（header 布局、槽布局规则、步进语义）= 不兼容；次版本只增 JSON 字段和注册表条目。JSON 里未知字段必须忽略。
