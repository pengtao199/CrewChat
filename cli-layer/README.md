# crewchat-cli

AgentOS 的 **CLI layer**：ACP 宿主、引擎管理、会话/流式、权限、花名册与持久化。
模型路由归 `model-layer/`，技能归 `skill-layer/`，MCP 归 `mcp-layer/`，UI 归 `AgentOS/`。

## 运行

```sh
cargo run
```

默认 `127.0.0.1:3081`，控制 token 在 `CLI_LAYER_TOKEN`，或写在
`~/Library/Application Support/AgentOS/cli/cli-layer.json`。

```sh
CLI_LAYER_DATA_DIR=... \
CLI_LAYER_PORT=3081 \
CLI_LAYER_TOKEN=... \
MODEL_LAYER_URL=http://127.0.0.1:3458 \
MODEL_LAYER_GATEWAY_URL=http://127.0.0.1:3456 \
  cargo run
```

## 当前实现

| 域 | 状态 |
| --- | --- |
| `health` | ✅ |
| `/ws/events` | ✅ attach / replay / event |
| ACP 会话 / prompt / cancel / permission | ✅ Claude / Codex / OpenCode + API-mode 引擎 |
| 引擎注册 / 安装 / 升级 / 环境诊断 | ✅ 15 个引擎 |
| 花名册 / 团队 / 委派 | ✅ |
| 模型代理 | ✅ `model_*` → model-layer |
| 技能代理 | ✅ `skill_*` → skill-layer |
| MCP 代理 + `session/new` 注入 | ✅ `mcp_*` / `agent_mcps_set` → mcp-layer |
| 委派设置 / 旧数据导入 | 旧数据导入已移除 |

## 目录

```
src/
  api/        HTTP / WS 兼容层与 handlers
  app/        用例：agents / sessions / skills / mcp / models / team
  domain/     领域类型与 ports
  infra/      model-layer / skill-layer / mcp-layer client + SQLite
  runtime/    ACP host、引擎注册、overlay
```

## 验证

```sh
cargo test
cargo clippy --all-targets -- -D warnings
```

真实 Claude / Codex / OpenCode 会话与 MCP 注入验收见 `AGENTS/verification.md`。
