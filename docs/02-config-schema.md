# 02 — 配置文件 Schema（B3 实现，B5 消费）

三类静态配置用 YAML；运行期不再出现名字。

## project.yaml
```yaml
coupling_version: 1
simulation:
  world: worlds/arm2dof.wbt          # 可选，用于离线校验 DEF 是否存在
  basic_time_step_ms: 1.0            # 必须等于世界的 basicTimeStep
  sync_mode: lockstep                # v1 只支持 lockstep
  model_timeout_ms: 50
  effort_policy: { ttl_steps: 1, on_timeout: zero, max_consecutive_misses: 3 }

models:                              # 怎么启动一个模型进程
  dc_motor:
    launch:                          # 二选一：host 拉起 / 外部自行 attach
      exe: bin/dcmotor-model         # 相对 project.yaml 所在目录
      args: ["--params", "models/shoulder_motor.yaml"]
    # attach: true

instances:                           # 模型实例
  shoulder_motor:
    model: dc_motor
    id: 1
    custom_signals:                  # 可选：该实例自定义信号（须与模型 hello 声明一致）
      - { signal_id: 0x8001, name: motor_current, value_type: f64, direction: to_webots, unit: A, domain: electrical }

bindings:                            # 实例 ⇄ Webots 对象；同一对象可被多个实例绑定
  - instance: shoulder_motor
    entities:
      joint: { webots_def: SHOULDER_JOINT, kind: joint }      # kind: joint | solid；关节可选 motor/sensor 覆盖设备名
    slots:
      - { signal: joint_position, entity: joint, required: true }
      - { signal: joint_velocity, entity: joint, required: true }
      - { signal: joint_torque,   entity: joint, required: true,
          limits: { max_abs: 5.0, max_rate_per_s: 2000.0 } }
```
规则：
- `signal` 用注册表名称或自定义信号名；`direction`、`value_type`、`unit` 由注册表推断，不得手写冲突值。
- `slot_id` 由 B3 按声明顺序自动分配，`entity_id` 按实例内 entities 出现顺序从 1 分配。
- `body_force`/`body_torque` 必须写 `frame: world|body_local`，可写 `point: [x,y,z]`。
- `limits` 对所有 Effort 槽必填（安全兜底），缺失即配置错误。
- `custom_signals`：`signal_id`（或 `id`）范围 `0x8000..=0xFFFF`，名称不得与基础注册表冲突，`direction` 必填；声明后即可在 `slots` 里按名称引用。host 握手时要求模型 hello 的 `custom_signals` 与此逐项一致。
- `joint_position`/`joint_velocity` 默认单位 `rad`、`rad/s`；移动关节在槽上显式写 `unit: m`（或 `m/s`）。manifest 里的单位必须是单一具体单位。
- 路径相对 `project.yaml`。未知字段：警告并忽略（前向兼容）。

## 模型参数文件（由模型自己定义）
B3 只提供**单位工具**，不规定模型参数结构：`units::parse("3000 rpm") -> 314.159…(rad/s)`，支持 rpm、deg、mm、cm、degC、degF、kW、mH、ms 等；任何厂家单位在**参数导入阶段**换算为 SI，进入协议的数据一律 SI。

## B3 的输出
`ResolvedProject`：每个实例的 `Vec<SlotSpec>`（B1 类型）+ entities + 绑定目标 + 超时/策略 + 模型启动方式，可直接 `serde_json` 序列化为协议 §8 的 manifest。