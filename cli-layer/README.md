# crewchat-cli

CrewChat 的 **CLI layer**：取代 codeg 的小型 Rust sidecar。只做 ACP 宿主、
引擎管理、会话/流式、权限、花名册、持久化；模型路由归 `model-layer/`，
UI 归 `AgentOS/`。

状态：AgentOS 默认核心（2026-10-23）。15 个引擎注册，10 个支持 per-role
模型注入；skill-layer 代理已接，MCP/委派后置。

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
| `acp_connect` / `acp_prompt` / `acp_cancel` / `acp_respond_permission` | ✅ Claude / Codex / OpenCode + 7 个 API-mode 引擎 |
| `acp_list_agents` | ✅ 15 个内置引擎（Claude / Codex / OpenCode / Gemini / OpenClaw / Cline / Hermes / CodeBuddy / Kimi / Pi / Grok / Cursor / DeepSeek / Qoder / Antigravity） |
| `ccr_health` / `ccr_call` / provider save/delete/probe / disabled models | ✅ 薄代理到 model-layer |
| `agent_roles_*` / `agent_role_model_face` | ✅ 10 个引擎支持 per-role 模型 |
| folders / conversations | ✅ 基础 |
| CLI 偏好 / env | ✅ `acp_update_agent_preferences` |
| 引擎版本 / 安装 / 升级 | ✅ `acp_fetch_agent_latest_release`、`acp_prepare_npx_agent`、`acp_download_agent_binary`、`acp_install_pi_binary` |
| 环境诊断 | ✅ `acp_env_diagnostics` |
| skills（skill-layer 代理） | ✅ `skill_list` / `skill_read` / `skill_install` / `agent_skills_set` 等 |
| MCP / delegation | 501，后续补 |

## 已验证（2026-10-23）

- `cargo test`（18 个）/ `cargo clippy --all-targets -D warnings` 通过。
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
- CLI 管理后端：
  - 引擎注册表 15 个（12 个 npx、3 个 binary），launch spec 对齐 codeg 的 pin；
    Claude / Codex / OpenCode / Grok / Hermes / Kimi / OpenClaw / Pi / Cline / DeepSeek
    支持 per-role 模型注入，其余用 CLI 自己的模型配置。
- `acp_list_agents` 返回 installed/registry version、enabled、env；
  - `acp_update_agent_preferences` 的 env 能进入子进程（fake agent 收到 `env:bar`）；
  - Claude 0.89.1 / Codex 2.2.2 / Pi 0.0.34 走 `acp_prepare_npx_agent`，OpenCode 1.18.35 走
    `acp_download_agent_binary`，本地安装后 `acp_connect` 可初始化；
  - `acp_fetch_agent_latest_release` 实测 Claude 0.94.0 / OpenCode 1.19.4 / Pi 0.0.34；
  - `acp_env_diagnostics` 返回 Node/npm/adapter/数据目录检查。
  - `acp_install_pi_binary` 路由已实现，但没在本机跑全局 npm 安装。
- 新增真实 session：Grok `grok-ok`、Hermes `hermes-ok`、Kimi `kimi_code-ok`、
  OpenClaw `openclaw-ok`、Cline `cline-ok`、DeepSeek `deepseek-ok`、Pi `pi-ok`，
  共用 model-layer 的 `local-gateway` key。
- AgentOS 切换：重启后 3080 无 codeg、3081 为 cli-layer；skill-layer 启动、
  81 个技能导入 registry（gstack 因 3519 文件/70MB 超过 skill-layer 限制未导入）；Skills 页通过 `skill_list` 读取 81 个技能；Claude 全局适配器连接成功。
- OpenClaw 的 ACP 是 Gateway bridge；host 为每个 role 启动独立
  `openclaw gateway run`（独立 port/config/state），连接关闭时回收。
- MCP / delegation / 旧 transcript 导入仍未完成；其余未支持的 CLI：Antigravity 还需要
  `$GEMINI_HOME/antigravity-acp/settings.json` 的 auth 配置，Cursor / Antigravity
  没有可查的 latest 端点（返回 null）。

## 接到 AgentOS（开发开关，默认关闭）

AgentOS 仍默认启动 codeg。要试新运行时：

1. 构建 `cli-layer`：`cargo build`（产物 `cli-layer/target/debug/crewchat-cli`）。
2. 在 Xcode 的 `AgentOS` scheme 里启用 `AGENTOS_CLI_LAYER_BINARY`，再加
   `AGENTOS_CLI_LAYER=1`（或创建 `~/Library/Application Support/AgentOS/cli/required`）。
3. 启动 App：壳会先拉模型层，再拉 `crewchat-cli`，并把核心连接指向 3081，跳过 codeg。

当前 `cli-layer` 已覆盖会话 / 模型页 / 花名册 / 项目 / 会话列表 / 技能；
MCP、委派仍是 501，所以设置页还不完整。切换前先只测聊天。

## 架构

当前按模块化单体组织，依赖方向 `api → app → domain`，`runtime`/`infra` 实现 port。
model / skill / MCP 是独立资源域，cli-layer 只代理，不拥有它们的存储。详见
[`docs/plans/cli-layer-architecture.md`](../docs/plans/cli-layer-architecture.md)。

## 目录

```
src/
  api/             AgentOS HTTP / WS 适配
  app/             用例编排（后续逐步抽出）
  config.rs        环境与数据目录
  domain/          领域类型与 port（后续逐步抽出）
  infra/
    model_layer.rs ccr_* → model-layer
    skill_layer.rs skill-layer 客户端（技能库 / materialize）
    store/         SQLite：folders/conversations/turns/roster
  runtime/
    acp/           ACP host / events
    engine/        CLI 注册表 / 安装 / 版本
    overlay/       per-role home / 原生配置
  main.rs          只做 wiring
```

## 来源与许可

- ACP host 骨架参考官方 `agentclientprotocol/rust-sdk` 的
  `examples/yolo_one_shot_client.rs`（Apache-2.0）；
- 进程/会话池的设计参考 OpenAB `crates/openab-core/src/acp`（MIT）；
- 事件/roster/overlay 的形状参考本仓库 `codeg/`（Apache-2.0）。
- 具体文件与对应许可见 [`NOTICE.md`](NOTICE.md)。

本 crate 采用 Apache-2.0。
