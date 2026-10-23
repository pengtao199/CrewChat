# 验证

## 适用范围

完成任何改动、准备汇报或提交之前。

## 原则

- 只跑与改动相关的最小检查，里程碑再跑全量。
- 声称「已验证」必须给出实际命令和结果。
- 不把旧运行时的验证命令带进新运行时。

## 默认检查

| 改动 | 命令 |
| --- | --- |
| `cli-layer/` | `cd cli-layer && cargo test`；必要时 `cargo clippy --all-targets -- -D warnings` |
| `model-layer/` | `cd model-layer && cargo test`；`cargo clippy --all-targets -- -D warnings` |
| `skill-layer/` | `cd skill-layer && cargo test`；`cargo clippy --all-targets -- -D warnings` |
| `mcp-layer/` | `cd mcp-layer && cargo test`；`cargo clippy --all-targets -- -D warnings` |
| `AgentOS/` | `cd AgentOS && xcodebuild -project AgentOS.xcodeproj -scheme AgentOS -configuration Debug -sdk macosx CODE_SIGNING_ALLOWED=NO build` |
| 规则 / 文档 | 检查路径、命令、链接是否还存在；不跑无关构建 |

## 真实会话

改 ACP / 模型注入 / 技能 / MCP 后至少跑一次：

1. 启动 `crewchat-model`、`crewchat-skill`、`crewchat-mcp`、`crewchat-cli`。
2. AgentOS 连接 cli-layer。
3. Claude / Codex / OpenCode 中至少一个真实会话完成一轮 prompt。
4. 涉及 Agent 能力时，验证会话实际拿到该 Agent 的模型 / 技能 / MCP。

## 禁止

- 用旧运行时的测试命令冒充当前验证。
- 只看编译通过就声称运行时可用。
- 跳过失败项不汇报。
