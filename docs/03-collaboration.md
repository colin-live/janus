

1. **一个 Block 一个负责人，一个目录。** 只改自己 Block 目录下的文件。需要别的 Block 改东西，提 issue/在 `docs/requests/` 写一份请求，不要直接改。
2. **契约先行。** `docs/01-protocol-v1.md`、`docs/02-config-schema.md`、各 Block 文档里的"冻结接口"一节是契约。要改契约：先改文档、升版本、记 `docs/CHANGELOG-protocol.md`，再通知受影响 Block。
3. **B1 是协议参考实现。** Python（B8）与 C ABI 一律使用 `coupling_core::header::offsets` 里的数值；不得自行推算偏移。
4. **每个 Block 必须自带测试**，满足该文档的"验收标准"后才算完成；`cargo test --workspace` 必须保持通过。
5. **代码与提交信息用英文**，文档用中文。禁止 `unsafe` 之外的未说明 UB；所有 `unsafe` 块要有 `// SAFETY:` 注释。
6. 不引入未在该 Block 文档中列出的重量级依赖；新增依赖需在 PR 说明理由。
7. 目标平台：**Windows 为主（x86-64）**，Linux 用于 CI；不支持大端。
8. 完成时在 Block 文档末尾的"状态"一节更新：完成日期、已知问题、对契约的疑问。

## 给 Agent 的启动提示模板
> 你负责 Block **Bx**。先读 `docs/00-overview.md`、`docs/01-protocol-v1.md`、`docs/03-collaboration.md` 和 `docs/blocks/Bx-*.md`。只修改 Block 文档规定的目录。按文档"验收标准"逐条完成并写测试。发现契约问题时不要绕过，写进 Block 文档的"状态"一节并停下来说明。