# crewchat-cli

CrewChat 的 **CLI layer**：取代 codeg 的小型 Rust sidecar。只做 ACP 宿主、
引擎管理、会话/流式、权限、花名册、持久化；模型路由归 `model-layer/`，
UI 归 `AgentOS/`。

状态：Phase 0/1 骨架（2026-10-22）。代码已落，尚未编译/联调。

## 运行

```sh
cd cli-layer
cargo run

# 一次性导入 codeg.db（folders / conversations / roster）
cargo run -- --import-codeg
# 或指定来源
cargo run -- --import-codeg --codeg /path/to/codeg.db
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
| `/ws/events`（attach / replay / event） | ✅ 骨架 |
| `acp_connect` / `acp_prompt` / `acp_cancel` / `acp_respond_permission` | ✅ Claude 路径 |
| `acp_list_agents` | ✅ Claude / Codex / OpenCode |
| `ccr_health` / `ccr_call` / provider save/delete/probe / disabled models | ✅ 薄代理到 model-layer |
| `agent_roles_*` / `agent_role_model_face` | ✅ 基础 |
| folders / conversations | ✅ 基础 |
| skills / MCP / engine install / delegation | 501，Phase 2–4 |

## 已验证（2026-10-22）

- `cargo check` / `cargo test`（5 个）/ `cargo clippy -D warnings` 通过。
- `scripts/smoke.sh`：假 ACP agent → `content_delta` → `turn_complete`。
- 假 ACP 权限：`permission_request` → `acp_respond_permission(allow_once)` → 工具继续。
- 真实 Claude（`claude-agent-acp@0.89.1`，经 model-layer）：
  - 基础回复 `ok`；
  - 工具 `Read File` → completed → `probe-ok`；
  - Bash `echo permission-ok` → completed；
  - per-role：`deepseek-v4.1-flash` 的会话 `_claude/model` 正确；
- 真实 Codex（`codex-acp@2.2.2`）：`codex-ok` + usage + turn_complete。
- 真实 OpenCode（`opencode acp`）：thinking + `opencode-ok` + usage + turn_complete。
- 旧数据导入：`--import-codeg` 导入 1 个 folder、5 个 conversation、8 个 role；
  stale provider id（`75f800be`、`opencode-go-chat-completions`）按模型回退到
  `OpenCode Go`。
- `/ws/events`：ready → replay → attached → event。
- 模型页代理：`getConfig` 返回 3 provider / 38 models。

## 目录

```
src/
  acp/        ACP host（events + host）
  config.rs   环境与数据目录
  engines.rs  CLI 注册表（Claude/Codex/OpenCode）
  http.rs     AgentOS 的 45 命令兼容层
  model_proxy.rs  ccr_* → model-layer
  roles.rs    per-role home（模型注入）
  store.rs    SQLite：folders/conversations/turns/roster
  ws.rs       /ws/events
```

## 来源与许可

- ACP host 骨架参考官方 `agentclientprotocol/rust-sdk` 的
  `examples/yolo_one_shot_client.rs`（Apache-2.0）；
- 进程/会话池的设计参考 OpenAB `crates/openab-core/src/acp`（MIT）；
- 事件/roster/overlay 的形状参考本仓库 `codeg/`（Apache-2.0）。
- 具体文件与对应许可见 [`NOTICE.md`](NOTICE.md)。

本 crate 采用 Apache-2.0。
