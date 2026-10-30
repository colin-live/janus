# 00 — 总览与模块地图

## 目标
启动一个指定项目时，按绑定关系把 **Webots 对象** 与 **独立进程的电气/物理模型实例** 通过共享内存连起来，每个仿真步交换机械状态与作用量。模型可以是自研也可以是开源模型（通过 SDK 或后续 FMU Runner 接入）。上游控制信号 → 模型不在本项目范围。

## 数据流
```
上游控制(不在范围) ──► [模型进程: coupling-client] ◄══共享内存(每实例一段)══► [Webots 适配器] ◄──► Webots
                                    ▲                                              ▲
                                    └──────────── coupling-host (配置/握手/监管) ─────┘
```

## 模块（Block）与依赖

| Block | 目录 | 语言 | 内容 | 依赖 |
|---|---|---|---|---|
| B1 | `crates/coupling-core` | Rust | 协议常量/类型/注册表/header/布局 | — **（已完成）** |
| B2 | `crates/coupling-shm` | Rust | 命名共享内存 + 唤醒事件（OS 层） | B1 |
| B3 | `crates/coupling-config` | Rust | YAML 配置解析、校验、单位换算、产出 `ResolvedProject` | B1 |
| B4 | `crates/coupling-client` | Rust(+C ABI, Python) | 模型侧 SDK | B1, B2 |
| B5 | `crates/coupling-host` | Rust 可执行 | 启动流程、握手、模型进程监管、CLI | B1, B2, B3 |
| B6 | `crates/mock-webots` | Rust 可执行+库 | 无 Webots 的假主控（参考适配器侧协议实现） | B1, B2 |
| B7 | `crates/dcmotor-model` | Rust 可执行 | 参考直流电机模型（基于 SDK） | B1, B4 |
| B8 | `adapters/webots-py` | Python | 真实 Webots 适配器（Supervisor 步进钩子） | 只依赖 `01-protocol` |
| B9 | `tests/`（顶层） | Rust+Python | 集成与一致性测试、spike 台架 | 全部 |

## 并行波次
```
Wave 0  B1 ✔
Wave 1  B2 ✔ ‖ B3 ✔ ‖ B8          (互相独立；B8 只靠协议文档和 offsets 表)
Wave 2  B4 ‖ B5 ‖ B6          (B4/B6 需要 B2 的 API；先按 B2 文档里冻结的接口写，用内存假实现起步)
Wave 3  B7 ‖ B9
```
B2 的公开 API 已在 `blocks/B2-shm.md` 冻结，Wave 2 的模块无需等 B2 完成即可开工。

## 阶段路线（本期之外只记录）
- 本期：机械状态读取 + 关节转矩/刚体力作用 + 锁步 + 共享内存。
- 后续：Webots 物理插件（Rust cdylib）替换 B8；FMU Runner；热/流体等物理域；Async 模式。
