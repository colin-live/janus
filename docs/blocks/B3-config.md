# B3 — coupling-config（配置解析与校验）

**目录** `crates/coupling-config`　**依赖** B1（含 B1.1 的 manifest 类型）　**波次** 1

## 目标
把 `project.yaml`（见 `02-config-schema.md`）解析、校验、解析成 `ResolvedProject`；提供单位换算工具。

## 冻结接口
```rust
pub fn load_project(path: &Path) -> Result<ResolvedProject, ConfigError>;
pub struct ResolvedProject { pub sim: SimSettings, pub instances: Vec<ResolvedInstance>, pub base_dir: PathBuf }
pub struct ResolvedInstance {
    pub name: String, pub instance_id: u32, pub launch: Launch,   // Launch::Spawn{exe,args,cwd} | Launch::Attach
    pub entities: Vec<Entity>, pub slots: Vec<SlotSpec>,          // B1 类型，slot_id 已分配
    pub slot_meta: Vec<SlotMeta>,                                 // required, limits, frame, point, name, unit
}
impl ResolvedInstance { pub fn to_manifest(&self, sim: &SimSettings, sim_id: u64) -> Manifest; }
pub mod units { pub fn parse(s: &str) -> Result<f64, UnitError>; pub fn parse_as(s:&str, dim: Dim) -> Result<f64, UnitError>; }
pub fn check_world_defs(world_path: &Path, defs: &[&str]) -> Vec<String>;   // 返回缺失的 DEF
```

## 详细需求
1. 依赖 `serde`、`serde_yaml`（或 `serde_yml`，由实现者选维护更好的）、`thiserror` 可选。
2. 校验并给出带路径的错误信息（如 `bindings[0].slots[2].limits: required for effort signal`）：
   - 实例 id 唯一且 > 0；绑定引用的实例/实体存在；信号名在注册表或本实例自定义信号中存在。
   - Effort 槽必须有 `limits.max_abs`；`body_force/body_torque` 必须有 `frame`。
   - 同一实例内（entity, signal, direction）不重复。
   - `basic_time_step_ms` 换算为整数纳秒 `dt_ns`，必须 > 0 且是整数纳秒。
   - `sync_mode` 非 `lockstep` → 明确的"v1 不支持"错误。
3. `slot_id` 按声明顺序自动分配；`Layout::build` 能通过（调用一次以提前暴露冲突）。
4. 单位工具：支持 `rpm、rad/s、deg、rad、mm、cm、m、kg、g、N、N*m、mN*m、A、V、ohm、mH、H、degC、degF、K、W、kW、ms、s、us` 及其常见写法；维度不匹配（把 `mm` 当 `Dim::Angle`）报错。
5. `check_world_defs`：对 `.wbt` 文本做 `DEF <name>` 扫描（容忍空白和换行），仅用于离线预检，不替代适配器在线校验。
6. 未知字段：`log::warn` 后忽略。

## 验收标准
- `examples/arm2dof/project.yaml` 解析成功，`to_manifest` 的 JSON 能被 B1 的 `Manifest` 反序列化。
- 至少 15 个单测：每条校验规则一个正例一个反例（含错误信息断言）；单位表 round-trip（`"3000 rpm"`→≈314.159 rad/s，`"25 degC"`→298.15 K）。
- 不依赖 B2。

## 状态

- **完成日期**：2026-10-30。
- **已实现**：`load_project`、`ResolvedProject`/`ResolvedInstance`/`SimSettings`/`Launch`/`SlotMeta`、`SlotSpec` 自动分配（`slot_id` 按声明顺序、`entity_id` 按实例内 entities 出现顺序）、`to_manifest`、全部校验规则、`units::parse/parse_as`、`check_world_defs`、未知字段 `log::warn` 后忽略。
- **测试**：`cargo test -p coupling-config` 共 46 个测试（8 个库内单测 + 37 个配置集成测试 + 1 个文档测试）全绿；`cargo test --workspace` 全绿；`cargo clippy -p coupling-config --all-targets -- -D warnings` 无告警。
- **依赖缺口（已解决，2026-10-30）**：B1.1 的 `coupling_core::manifest` 已落地，`crates/coupling-config/src/manifest.rs` 已改为 `pub use coupling_core::manifest::…` 的 re-export，镜像删除。B1 负责人为 `Entity` 增加了可选字段 `motor`/`sensor`（适配器覆盖设备名，协议 §8），`RawEntity` 原样传递；B3 已补有/无两种测试，并在 `to_manifest` 的 JSON 往返中断言字段被保留/省略。
- **Schema 缺口（需 `02-config-schema.md` 负责人确认）**：B3 要求校验“本实例自定义信号”，但 schema 文档没有定义自定义信号的声明语法。B3 暂定支持实例级可选字段：
  ```yaml
  instances:
    shoulder_motor:
      model: dc_motor
      id: 1
      custom_signals:
        - { signal_id: 32769, name: dc_bus_voltage, value_type: f64, direction: to_model, unit: V, domain: electrical }
  ```
  `signal_id`/`id` 二选一，范围 `0x8000..=0xFFFF`，名称不得与基础注册表冲突。若正式的 schema 采用别的形状，本节即为变更请求。
- **单位歧义（已确认）**：注册表里 `joint_position`/`joint_velocity` 的单位是 `rad|m`、`rad/s|m/s`，无法自动判定转动还是移动。B3 默认取第一种（`rad`/`rad/s`），并允许槽上显式写 `unit: m`/`m/s`（按维度校验后归一化为 SI 拼写）。manifest 中该槽仍必须写单一单位。
- **关节运动一致性预检（评审建议，已实现）**：同一实体上显式 `unit: m`/`m/s` 与 `joint_torque`、显式 `unit: rad`/`rad/s` 与 `joint_force`、或同时出现 `joint_torque` 与 `joint_force` 时，配置阶段即报带路径错误；未显式写单位的槽保持注册表默认，不参与推断，仍由适配器按实际节点类型在线校验。
- **目录说明**：验收标准点名 `examples/arm2dof/project.yaml`，该目录原为空；本次按 `02-config-schema.md` 的示例创建了该 fixture。除此之外只修改了 `crates/coupling-config` 与本文档。
- **依赖选择**：`serde_yaml 0.9` 已在上游标记 deprecated，但它是本机离线缓存中可用且行为满足冻结 schema 的 YAML 后端；如后续引入 `serde_yml`，只需替换本 crate 的反序列化入口，不影响公开 API。
- **对契约的疑问**：无其他协议级冲突；上述 schema/上游依赖缺口均已由评审回复确认或修复，B3 未擅自修改 `01-protocol-v1.md`、`02-config-schema.md` 或 B1 代码。

## 评审回复（B1 负责人，2026-10-30）

1. **依赖缺口（B1.1 的 manifest 类型）→ 已修复。** `coupling_core::manifest` 已落地（`Manifest`/`Entity`/`SlotEntry`/`EffortPolicy`/`Limits`，另有 `Hello`/`CustomSignal`/`Report`、名称⇄枚举辅助函数、`Manifest::slot_specs()`、`Layout::from_header()`），字段与你们的镜像逐字一致。`crates/coupling-config/src/manifest.rs` 已改为 `pub use coupling_core::manifest::…` 的 re-export，镜像删除。因为 `Entity` 新增了可选字段 `motor`/`sensor`（适配器用来覆盖设备名，见协议 §8），`config.rs` 里 `RawEntity` 增加了两个可选字段并原样传入 `Entity`——这是对 B3 代码的最小改动。`cargo test --workspace` 全绿（B3 的 41 个测试不变）。
   - **留给 B3 的小任务**：为 `motor`/`sensor` 补 2 个单测（有/无两种），并在 `to_manifest` 的 JSON 往返测试中断言它们被保留。
2. **自定义信号的声明语法 → 采纳你们的提案**，已写入 `02-config-schema.md`：实例级 `custom_signals` 列表，`signal_id`（或 `id`）范围 `0x8000..=0xFFFF`，名称不得与基础注册表冲突，`direction` 必填。补充一条跨进程规则：host 握手时要求模型 hello 里的 `custom_signals` 与配置逐项一致（B5 已写入）。
3. **单位歧义 → 采纳你们的默认**：`joint_position`/`joint_velocity` 默认 `rad`、`rad/s`，移动关节显式写 `unit: m`/`m/s`；manifest 里必须是单一单位。已写入协议 §4：适配器在 `start` 时按实际节点类型校验（`HingeJoint`⇔`rad`+`joint_torque`，`SliderJoint`⇔`m`+`joint_force`），不一致 → report `Error`。**B3 无需再改**，但如果同一关节的 `unit` 与 `joint_torque`/`joint_force` 选择明显矛盾（例如 `unit: m` 配 `joint_torque`），建议在配置校验里提前报错（可选，不卡验收）。
4. **`serde_yaml` 已弃用 → 接受。** 保持现状，但请把 YAML 反序列化入口集中在一个函数（现在基本已是），将来换 `serde_yml` 只改一处。
5. **`examples/arm2dof/project.yaml` fixture → 接受**，作为全项目的权威示例。后续 B5/B9 直接引用；若 B7 需要 `models/shoulder_motor.yaml`，由 B7 负责人新增。
