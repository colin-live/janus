# B1 — coupling-core（协议核心）

**目录** `crates/coupling-core`　**依赖** 无　**状态** 全部完成（含 B1.1，2026-10-30）

## 目标
协议的参考实现：常量、枚举、信号注册表、`SegmentHeader`、`Layout::build`。纯 std、无 I/O、无 OS 调用，所有其他 Block 的地基。

## 已完成
header（256B，含 report 区）、`offsets` 偏移表、ValueType/Direction/Validity/FrameId/SyncMode/HandshakeState、基础注册表、布局构建与校验，共 7 个单测通过。

## 待办 B1.1（Wave 1 开头，小任务，阻塞 B3/B4/B5 的最终联调）
1. 新增 `manifest` 模块（依赖 `serde`、`serde_json`）：`Manifest`、`Entity`、`SlotEntry`、`EffortPolicy`、`Limits`、`Hello`、`CustomSignal`、`Report` 的 serde 结构，**字段名与 `01-protocol-v1.md` §8/§9 完全一致**，未知字段忽略。
2. `Manifest::from_slots(...)` 帮助函数：由 `Layout` + 配置生成 manifest，并保证 `slots` 与 `Layout.slots` 一一对应。
3. `examples/dump_offsets.rs`：输出 `protocol_offsets.json`（所有 header 偏移、ValueType 大小、注册表），供 Python（B8）读取。
4. 往返测试：manifest/hello 序列化→反序列化相等；未知字段被忽略。

## 验收标准
- `cargo test -p coupling-core` 全绿；`header_offsets_match_struct` 覆盖每个对外偏移。
- JSON 往返测试通过；`dump_offsets` 输出可被 Python `json.load`。
- 不引入除 serde/serde_json 外的依赖。

## 契约变更规则
任何改动 header 偏移、注册表已有 ID、枚举数值，都属主版本变更，必须走 `03-collaboration.md` 第 2 条。

## 完成记录（B1.1，2026-10-30）
- `coupling_core::manifest`：`Manifest/Entity/SlotEntry/EffortPolicy/Limits/Hello/CustomSignal/Report/EntityReport`；`value_type/direction/frame` 的名称⇄枚举函数；`SlotEntry::to_spec`、`Manifest::slot_specs`、`simulation_id_u64`、`slot_by_name`。
- `Layout::from_header(specs, &SegmentHeader)`：消费方（B4/B6/B8 的 Rust 部分）用 manifest 的槽重建布局，并与 header 中的偏移交叉校验，不一致即 `BadHeader`。
- `Manifest::from_slots` **未做**：host 侧由 B3 的 `to_manifest` 负责生成，避免两处生成逻辑。
- `examples/dump_offsets.rs` → `adapters/webots-py/protocol_offsets.json`（已生成并提交；B9 要加一条检查"提交的文件 == 当前输出"）。
- `Entity` 增加可选 `motor`/`sensor`；`CoreError` 增加 `BadManifest(String)`。
- 测试：`coupling-core` 共 14 个（原 7 + manifest 7），`cargo test --workspace` 全绿。
