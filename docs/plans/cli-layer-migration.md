# CLI Layer：移除 Codeg

Updated: 2026-10-22
Status: In Progress
关系：模型层已完成（[模型层：移除 CCR](model-layer-remove-ccr.md)）；技能统一管理见 [Skill 层](skill-layer.md)（全局库 + Agent 选择 + session home）。本计划取代
[Swift 原生运行时迁移](swift-runtime-migration.md) 的 Phase 2–5，作为近期路径；
Swift 原生 runtime 仍是终态选项，不在本计划内。

---

## 0. 结论

- **不要**把 `codeg/` 原地改名成 `cli-layer/`。那只是换标签，475k 行 Rust、23GB 目录、
  Next.js 工作台、浏览器/画布/宠物全都还在。
- **新建 `cli-layer/`**：只做 ACP 宿主、引擎管理、会话/流式、权限、花名册、持久化、委派。
- **协议用官方 crate**，不重写：`agent-client-protocol` 2.2（Apache-2.0）。
- **ACP 宿主骨架直接 copy 官方 `rust-sdk` 的 client 示例**；会话池参考/复制
  OpenAB 的 `crates/openab-core/src/acp`（MIT）。
- **从 codeg 只 copy 纯逻辑模块**：session state、event stream、question/approval、
  delegation、agent_host overlay、roster/store。**不 copy** 它的 `acp/connection`、
  `manager`、`parsers`、`web`、Next.js。
- codeg 降级为**只读参考 + 数据导入器 + 回退**；等 `cli-layer` 对上 AgentOS 的
  45 个命令后删除。
- 工期 **5–8 周**；Phase 1 只做 Claude，先证明一条会话。

---

## 1. 项目定位与边界

CrewChat 是**本地多 Agent 协作桌面应用**：

```
Agent = 大脑（模型）+ 引擎（CLI）+ 技能 + MCP + 记忆 + 规则
```

- 群聊里有 Leader（`crewchat-manager`），成员可以是不同引擎；
- **每个 Agent 可以跑不同模型**，由 per-role overlay 注入，不绑定 CLI；
- 本地优先，AgentOS 负责渲染，核心负责进程/会话/权限/持久化。

`cli-layer` 的职责：

- 拉起/管理 CLI 引擎（claude / codex / opencode），通过 ACP 说 JSON-RPC；
- 会话、turn、流式事件、取消、权限审批；
- 项目/会话/turn 持久化；
- 花名册、技能、MCP、引擎安装/升级；
- 委派/团队、用量；
- per-role overlay：把 Agent 的 runner 模型注入 CLI。

不负责：

- 模型路由/密钥/额度 → `model-layer/`；
- UI → `AgentOS/`；
- 旧 transcript 考古 → 只做一次性导入器。

---

## 2. 现状盘点

### 2.1 codeg 体量

| 部分 | 行数 | 处理 |
| --- | --- | --- |
| 全部 Rust | 474,804 | 只取子集 |
| `acp/` | 127,450 | 只取纯逻辑模块，不取 connection/manager |
| `parsers/` | 66,434 | 不迁；只写 importer |
| `web/` | 21,439 | 不迁；`cli-layer` 自己实现 HTTP/WS |
| `db/` | 26,837 | 取 roster/folder/conversation/turn 参考 |

目录 23GB，主要是 node_modules、Next.js 产物、Tauri 依赖、测试资源；不是有效代码量。

### 2.2 AgentOS 的实际接口面

`AgentOS/engine-api.md` 登记了 **45 个命令**。这是 `cli-layer` 必须兼容的唯一清单，
不是 codeg 的全部路由：

| 域 | 命令 | 目标模块 | Phase |
| --- | --- | --- | --- |
| 生命周期 | `health` | `http/health` + `lifecycle` | P1 |
| 会话 | `acp_connect` / `acp_prompt` / `acp_cancel` / `/ws/events` / `acp_respond_permission` | `acp-host` + `sessions` + `events` | P1–P2 |
| 模型页 | `ccr_health` / `ccr_call` / `ccr_provider_*` / `ccr_disabled_models` / `ccr_supervisor_start` | `http/model_proxy.rs`（薄代理到 model-layer） | P1 |
| 花名册 | `agent_roles_list` / `agent_roles_save` / `agent_roles_delete` / `agent_role_model_face` | `store/roster` + `roles` | P2 |
| 技能 | `acp_list_agent_skills` / `acp_read_agent_skill`；新 `skill_*` / `agent_skills_set` | `cli-layer` 代理 `skill-layer` | ✅ |
| MCP | `mcp_scan_local` / `mcp_upsert_local_server` / `mcp_remove_server` | `capabilities/mcp` | 后置 |
| 引擎 | `acp_list_agents` / `acp_update_agent_preferences` / `acp_fetch_agent_latest_release` / `acp_prepare_npx_agent` / `acp_download_agent_binary` / `acp_install_pi_binary` / `acp_env_diagnostics` | `engines` | P3 |
| 项目/会话 | `list_open_folder_details` / `open_folder` / `remove_folder_from_workspace` / `list_all_conversations` / `delete_conversation` / `create_conversation` / `get_folder_conversation` / `get_conversation` | `store` + `http` | P3 |
| 设置 | `get_delegation_settings` / `set_delegation_settings` | `team` + `settings` | P4 |

**模型页的 `ccr_*` 保留为薄代理**：AgentOS 继续走核心命令，`cli-layer` 把调用转给
`model-layer` 的 3458。这样 Phase 1–3 不需要改 Swift 壳；以后可以把模型页直接指到
model-layer，再删代理。

---

## 3. 复用策略：copy / fork / dependency

| 资产 | 许可 | 方式 | 说明 |
| --- | --- | --- | --- |
| `agentclientprotocol/agent-client-protocol`（2.2） | Apache-2.0 | **直接依赖** | 官方 ACP runtime（client/agent），不 fork |
| `agentclientprotocol/rust-sdk` 的 `examples/v2_one_shot_client.rs` / `v2_session_coordination.rs` | Apache-2.0 | **copy + 改** | ACP host 骨架：spawn、initialize、prompt、事件循环 |
| OpenAB `crates/openab-core/src/acp/{connection,pool,protocol}.rs` | MIT | **copy + 改** | 紧凑的 stdio JSON-RPC、会话池、turn 结果；替换自动权限策略 |
| codeg `acp/session_state.rs` / `event_stream.rs` / `question.rs` / `plan_approval.rs` | Apache-2.0 | **copy + 改** | turn 状态机、WS 事件、提问、计划审批 |
| codeg `acp/delegation/` | Apache-2.0 | **copy + 改** | 委派/团队 broker |
| codeg `agent_host/{overlay,role_home,faces,gateway}.rs` | Apache-2.0 | **copy + 改** | per-role 模型注入；`gateway.rs` 改成指向 model-layer |
| codeg `db/entities/{custom_agent,folder,conversation,message}.rs` + 对应 service | Apache-2.0 | **copy + 改** | roster/项目/会话/turn 的 schema 参考 |
| codeg `acp/{registry,binary_cache,latest_release,mcp_scope}.rs` | Apache-2.0 | **copy + 裁剪** | 只留 Claude/Codex/OpenCode |
| [`farion1231/cc-switch`](https://github.com/farion1231/cc-switch) | MIT | **copy + 改** | 各 CLI 的配置写入/路径解析：Claude、Codex、Gemini、Grok、Hermes、Kimi、OpenClaw、OpenCode、Antigravity、Pi |
| `claude-code-acp` / `codex-acp` / `opencode acp` | 各自 | **外部依赖** | 作为 CLI 适配器安装/启动，不 vendor |
| `zed-industries/codex-acp` | 许可证未明确（NOASSERTION） | **不 copy** | 需要时作为外部二进制，先用法务确认 |
| `rebornix/Agmente` | MIT | 只读参考 | Swift ACP client，将来 Swift 迁移用 |
| `agentic.nvim` / `obsidian-agent-client` / `vscode-acp` | 各自 | 只读参考 | 会话/事件/权限交互；不 copy |

**不 copy 的 codeg 部分**：

- `acp/connection/mod.rs`（18k 行）和 `acp/manager/mod.rs`（4.4k 行）：与 `AppState`、
  web、DB、browser/terminal runtime 深度耦合；用官方 SDK + OpenAB 的池替代。
- `acp/browser_tools.rs`、`computer_tools.rs`、`canvas`、`web/`、`parsers/`、
  Next.js 前端、Tauri 壳。
- 测试文件按需重写，不整块搬。

### License / NOTICE

- 复制文件保留原版权头；新增 `cli-layer/NOTICE.md`，逐项记录
  `[from codeg]` / `[from openab]` / `[from rust-sdk]` 与源 commit。
- Apache-2.0 与 MIT 兼容；`cli-layer/` 自己的许可选 Apache-2.0，与 codeg 一致。

---

## 4. 目标架构

```
AgentOS.app (Swift)
  │  HTTP /api/* + /ws/events
  ▼
cli-layer (Rust sidecar)
  ├─ http/          45 个命令的兼容层
  ├─ acp-host/      官方 agent-client-protocol + copied client skeleton
  ├─ sessions/      turn 状态机、事件流、取消、权限
  ├─ engines/       CLI registry、安装/升级、env 诊断
  ├─ store/         SQLite：folders / conversations / turns / roster
  ├─ roles/         per-role overlay → 注入模型 + 网关地址
  ├─ capabilities/  skills / MCP
  ├─ team/          delegation / leader / 用量
  └─ model_proxy/   ccr_* → model-layer 3458
        │
        ▼
  model-layer (3456 / 3458)
        │
        ▼
  Claude Code / Codex / OpenCode（ACP adapter）
```

- `cli-layer` 是 sidecar，和 `crewchat-model` 一样由 AgentOS 拉起。
- AgentOS 的 HTTP/WS 契约先保持不变；`cli-layer` 兼容 45 个命令。
- 模型层保持独立；`cli-layer` 不持有 provider key。

### 命名

| 名字 | 指什么 |
| --- | --- |
| `cli-layer/` | 整个运行时的目录/仓库名 |
| `acp-host` | `cli-layer` 内的 ACP 宿主模块（我们作为 client，spawn agent） |
| `agent-client-protocol` | 官方协议 crate（依赖，不 fork） |
| `acp-adapter` | **CLI 侧**适配器（claude-code-acp / codex-acp），不是我们写的 |
| `acp-bridge` | 把 ACP 接到 Discord/微信等传输的桥；不是我们的场景 |

一句话：**我们是 `acp-host`，CLI 是 agent，adapter 在 CLI 那边。**

---

## 5. 数据模型与迁移

新库（`cli-layer.sqlite`）最小表：

| 表 | 来源 | 说明 |
| --- | --- | --- |
| `folders` | codeg `folder` | id/path/name/alias/sort_order/is_open |
| `conversations` | codeg `conversation` | workspace、title、agent_type、status、external_id |
| `turns` / `messages` | codeg turn/message | 新会话写 ACP 事件归约后的 turn；旧数据导入为只读 |
| `agent_roles` | codeg `custom_agent`（kind='role'） | body/provider/model/skills/mcp/permission |
| `events` | 无 | 新库从切换点开始记录；不迁 transcript parser |
| `usage` | codeg `usage_events` | 可后置 |

迁移规则：

- 旧 `codeg.db` 只读保留；导入 folders/conversations/turns/roster；
- **不迁 66k 行 parser**；旧历史以导入时的快照为准，不再解析 CLI transcript；
- 新会话只记录 ACP `session/update` 事件与 turn 结果；
- 切换期单写入者：`cli-layer` 写新库，codeg 只读。

---

## 6. 分阶段计划

### Phase 0：契约冻结与 spike（3–5 天）

- 把 `AgentOS/engine-api.md` 的 45 个命令按上表映射到模块；
- 拉官方 `rust-sdk` 的 `v2_one_shot_client.rs` + OpenAB 的 `acp` 三文件，做一次编译 spike；
- 用 Claude Code 跑通一条 ACP turn，录 fixture（text / tool / permission / cancel）；
- 决定新库 schema 与 `ccr_*` 代理形态。

**Gate**：一个独立 Rust 测试进程能 spawn `claude-agent-acp`，完成 prompt → 流式 →
tool → permission → cancel；不接 AgentOS。

### Phase 1：cli-layer 骨架 + Claude（2–3 周）

- 新 crate `cli-layer/`：HTTP/WS + SQLite + 进程管理；
- `health`、`acp_list_agents`、`acp_connect`、`acp_prompt`、`acp_cancel`、`/ws/events`；
- `ccr_*` 薄代理到 model-layer；
- 只支持 Claude Code。

**Gate**：AgentOS 指向 `cli-layer`，一个 Claude 会话完整跑通；模型页正常。

### Phase 2：Codex + OpenCode + 花名册（2–3 周）

- 引擎 registry 扩展到 15 个，per-role overlay 已接 10 个；
- 花名册 `agent_roles_*` + `agent_role_model_face`；
- 权限审批 `/ws/events` + `acp_respond_permission`。

**Gate**：10 个可用引擎各一个 Agent，各自跑不同模型；权限批准/拒绝两条路径。

### Phase 3：数据迁移 + 切换（2 周）

- 导入 folders / conversations / turns / roster；
- 技能/MCP/引擎安装（`acp_*_skill`、`mcp_*`、`acp_prepare_npx_agent` 等）；
- AgentOS 默认指向 `cli-layer`，codeg 只读回退。

**Gate**：旧会话可读；`lsof` 没有 codeg；技能/MCP 页正常。

### Phase 4：团队/委派/用量 + 删除 codeg（2 周）

- `delegation/` 复制并接团队；
- 用量、错误展示；
- 删除 `codeg/`、切换脚本与文档。

**Gate**：3 人团队跑通；委派可见；一周无回归。

**总工期 5–8 周**；Phase 1 是唯一必须按顺序的关键路径。

---

## 7. 可直接 copy 的清单

### 7.1 从官方 rust-sdk

| 源 | 目标 |
| --- | --- |
| `examples/v2_one_shot_client.rs` | `cli-layer/src/acp/host.rs`（spawn + initialize + prompt + 事件循环） |
| `examples/v2_session_coordination.rs` | `cli-layer/src/acp/session.rs`（会话协调/lifecycle） |

### 7.2 从 OpenAB（MIT）

| 源 | 目标 |
| --- | --- |
| `crates/openab-core/src/acp/connection.rs` | `cli-layer/src/acp/connection.rs`（替换 `pick_best_option` 自动批准） |
| `crates/openab-core/src/acp/pool.rs` | `cli-layer/src/sessions/pool.rs` |
| `crates/openab-core/src/acp/protocol.rs` | `cli-layer/src/acp/protocol.rs` |

### 7.3 从 codeg（Apache-2.0）

| 源 | 目标 |
| --- | --- |
| `acp/session_state.rs` | `sessions/state.rs` |
| `acp/event_stream.rs` | `sessions/events.rs` |
| `acp/question.rs` | `sessions/question.rs` |
| `acp/plan_approval.rs` | `sessions/approval.rs` |
| `acp/delegation/` | `team/` |
| `agent_host/overlay.rs` / `role_home.rs` / `faces.rs` | `roles/` |
| `agent_host/gateway.rs` | `roles/gateway.rs`（改成 model-layer URL/ID 解析） |
| `db/entities/{custom_agent,folder,conversation,message}.rs` | `store/entities/` |
| `commands/{folders,conversations,agent_roles,mcp,skills,engines}.rs` | `http/`（只取 handler 形状，传输层重写） |
| `acp/registry.rs` / `binary_cache.rs` / `latest_release.rs` / `mcp_scope.rs` | `engines/`、`capabilities/` |

### 7.4 从 cc-switch（MIT）

| 源 | 目标 |
| --- | --- |
| `src-tauri/src/gemini_config.rs` | `engines/config_writers/gemini.rs` |
| `src-tauri/src/grok_config.rs` | `engines/config_writers/grok.rs` |
| `src-tauri/src/hermes_config.rs` | `engines/config_writers/hermes.rs` |
| `src-tauri/src/kimi_config.rs` | `engines/config_writers/kimi.rs` |
| `src-tauri/src/openclaw_config.rs` | `engines/config_writers/openclaw.rs` |
| `src-tauri/src/codex_config.rs` | 已有，参考其 provider/catalog 处理 |
| `src-tauri/src/opencode_config.rs` | 已有，参考其 JSONC 处理 |
| `src-tauri/src/agy_config.rs` | `engines/config_writers/antigravity.rs`（`--gemini_dir`） |
| `src-tauri/src/pi_config/` | `engines/config_writers/pi.rs` |

复制时把「写全局 `~/.xxx`」改成「写 `<data>/roles/<role>/xxx`」，保留
`[from cc-switch]` 与 MIT 声明。

### 7.5 不 copy

- `acp/connection/mod.rs`、`acp/manager/mod.rs`（耦合太深）；
- `acp/browser_tools.rs`、`computer_tools.rs`、`canvas`、`web/`、`parsers/`；
- Next.js/Tauri 前端与桌面壳；
- `codex-acp`（许可证未明确，先作为外部二进制）。

---

## 8. 风险与对策

| 风险 | 影响 | 对策 |
| --- | --- | --- |
| OpenAB 的 ACP 池自动批准权限 | 无法做审批 UI | fork 后把 `pick_best_option` 换成挂起 + 等待 `acp_respond_permission` |
| 官方 rust-sdk 示例只覆盖基础流 | 工具/权限/取消要补 | 用 codeg 的 `question/plan_approval` 逻辑补齐 |
| codeg 的 roster/store 与 web/AppState 耦合 | copy 后编译不过 | 只 copy entity + 纯函数，service 重写 |
| 多 CLI quirk | Codex/OpenCode 起不来 | v1 只 Claude；Codex/OpenCode 逐个接，保留 codeg 回退 |
| 数据迁移 | 旧会话丢失 | codeg.db 只读备份；先导入再切换；单写入者 |
| 双 runtime | 会话库双写 | 切换期 `cli-layer` 唯一写入，codeg 只读 |
| 模型页 `ccr_*` | AgentOS 改造成本 | Phase 1 先做薄代理，后续再评估直连 model-layer |
| 许可证 | 不能合入 | 只 copy Apache-2.0/MIT；`codex-acp` 不 vendor |

---

## 9. 不做什么

- 不原地重命名 `codeg/`。
- 不迁 Next.js 工作台、Tauri 壳、浏览器、computer-use、canvas、pets、office/science。
- 不迁 66k 行 transcript parser；只写一次性 importer。
- 不把模型层塞进 `cli-layer`。
- 不 fork 整个 OpenAB（chat-platform 假设）或整个 codeg。
- **不自己写每个 CLI 的配置模板**：优先移植 `cc-switch` 的 MIT 实现，按 per-session 根目录改路径。
- Phase 1 不做团队/委派/技能/MCP。

---

## 10. 第一刀（Phase 0 具体任务）

1. 建 `cli-layer/` crate（`crewchat-cli`），加 `agent-client-protocol = "2.2"`。
2. Copy 官方 `v2_one_shot_client.rs` + OpenAB `connection/pool/protocol` 到 `src/acp/`，
   跑通 Claude 一条 turn。
3. 录 fixture：text / tool / permission / cancel。
4. 把 45 个命令映射表落成 `cli-layer/src/http/routes.md`（或代码里的路由表）。
5. 决定 `cli-layer.sqlite` schema 与 `codeg.db` 导入字段。
6. 产出 go/no-go：如果 OpenAB 池 + 官方 SDK 能在 2 天内跑通 Claude，按本计划；
   否则退回 codeg 的 `connection` 参考，时间 +1–2 周。

**Gate**：一个独立测试进程完成 Claude turn；AgentOS 不动。

---

## 11. 进展（2026-10-23）

`cli-layer/` 已落地 Phase 0/1/2 的可运行版本，编译、单测和真实 ACP 会话均通过。

已实现：

- `config` / `store`：folders、conversations、turns、agent_roles、events 的 schema 与基础 CRUD。
- `engines`：15 个引擎注册表（npx / binary），`AcpAgentConfig` + per-session env；
  10 个引擎带 per-role overlay。
- `roles`：10 种引擎的 per-role home / 原生配置；Claude 注入 gateway / model /
  `apiKeyHelper`。
- `acp/host`：spawn、initialize、`session/new`、prompt、cancel、permission broker，
  并把 `session/update` 转成 `content_delta` / `tool_call` / `turn_complete` envelope。
- `http`：45 命令兼容面；skills / MCP / engine install / delegation 暂返回 501。
- `ws`：attach / replay / event。
- `model_proxy`：`ccr_*` 薄代理到 model-layer。
- `NOTICE.md`：rust-sdk / OpenAB / codeg 的来源与许可。

未做：

- Hermes / OpenClaw 的真实会话。
- 技能 / MCP / 委派 / 打包。

**Phase 0 Gate 不变**：`cargo check` 通过 + Claude 一条 turn。

### 验证（2026-10-22）

- `cargo check` / `cargo test`（5 个）/ `cargo clippy -D warnings` 通过。
- 假 ACP：connect → `content_delta` → `turn_complete`；权限 request → `allow_once` → 工具继续。
- 真实 Claude（`claude-agent-acp@0.89.1`，经 model-layer）：
  - 基础回复 `ok`、`turn_complete end_turn`；
  - `Read File` 工具 → `tool_call_update completed` → `probe-ok`；
  - Bash `echo permission-ok` → completed → `permission-ok`；
  - per-role `deepseek-v4.1-flash`：会话 `_claude/model` 正确，回复 `role-ok`；
  - 模型页代理 `getConfig`：3 provider / 38 models。
- `/ws/events`：ready → replay → attached → event。
- role home：`apiKeyHelper` 是顶层设置，补了 plugins 与 `~/.claude` 共享目录 symlink。

未做：AgentOS 切换（Phase 3）、Codex / OpenCode 实聊（Phase 2）、
skills / MCP / engine install、数据迁移、打包。

### Phase 2 验证（2026-10-23）

- 真实 Codex（`codex-acp@2.2.2`）：connect → `codex-ok` → usage → turn_complete。
- 真实 OpenCode（`opencode acp`）：thinking → `opencode-ok` → usage → turn_complete。
- 三个引擎都通过 per-role overlay 使用 `OpenCode Go/deepseek-v4.1-flash`。
- 旧数据导入 `--import-codeg`：1 folder / 5 conversations / 8 roles；
  provider id 与显示名混用（`opencode-go-chat-completions`、`75f800be`）时，
  按「provider 名/id → 模型唯一命中」归一化，会话模型标签正确。
- 生产 `cli-layer.db` 已预置（AgentOS/cli），codeg 仍在运行，未切换。

### 模型注入验证（2026-10-23）

- 新增 7 个 per-session writer：Grok / Hermes / Kimi / OpenClaw / Pi / Cline /
  DeepSeek；`cargo test` 18 passed，`cargo clippy --all-targets -D warnings` 通过。
- 真实 ACP session（同一 `local-gateway` key + `OpenCode Go/mimo-v2.6-flash`）：
  - Grok `grok-ok`
  - Hermes `hermes-ok`
  - Kimi `kimi_code-ok`
  - OpenClaw `openclaw-ok`
  - Pi `pi-ok`
  - Cline `cline-ok`
  - DeepSeek `deepseek-ok`
- OpenClaw 使用全局已安装二进制 + 每 role 独立 gateway 验证；Hermes 使用全局
  已安装二进制验证。两者都不依赖本次 npm 安装成功。
- 未支持：Gemini / Antigravity（Gemini 协议）、CodeBuddy / Qoder（自有登录或目录）、
  Cursor（Cursor 账号 + 固定端点）。

剩余：MCP / delegation、旧 transcript 导入、打包。

### Phase 3 切换（2026-10-23）

- AgentOS 默认走 cli-layer：`CoreBinaryLocator.isCliLayerEnabled()` 默认 true，
  `AGENTOS_CLI_LAYER=0` / `AGENTOS_CODEG_CORE=1` 是显式回退。
- AgentOS 启动顺序：model-layer → skill-layer → cli-layer；skill-layer 只作为
  本地 daemon，不阻塞聊天。
- cli-layer 已加 skill 代理：`skill_list` / `skill_read` / `skill_install` /
  `skill_check` / `skill_update` / `skill_remove` / `skill_link_status` /
  `agent_skills_set` / `agent_skill_status`；旧 `acp_list_agent_skills` /
  `acp_read_agent_skill` 继续兼容现有 Skills 页。
- `acp_connect` 会在 role 有技能时调用 skill-layer `materialize`，把选中的技能
  链到 role home 的 `skills/`；技能缺失会让连接失败，而不是静默跳过。
- 旧技能目录一次性导入 skill-layer registry（81 个技能；gstack 因 3519 文件/70MB 超限未导入），
  `~/.agents/skills` 已转为 current 软链。
- 已验证：重启 AgentOS 后 `3080` 无 codeg，`3081` 为 cli-layer；Skills 页
  通过旧命令读取 81 个技能；Claude 使用全局 `claude-agent-acp` 连接成功。
- 未完成：MCP / delegation 仍 501；`session/load` 尚未实现；前端新技能命令
  （`skill_*` / `agent_skills_set`）未接。

### AgentOS 开发开关（2026-10-23）

`CoreSupervisor` 已加 `AGENTOS_CLI_LAYER=1` / `cli/required` 分支：启用时先拉模型层，
再拉 `crewchat-cli`，把壳的 `CoreEndpoint` 指向 3081，跳过 codeg。
scheme 里 `AGENTOS_CLI_LAYER_BINARY` 默认关闭；未启用时行为与现在完全一致。
技能/MCP/引擎安装/委派仍是 501，所以这个开关只用于聊天/模型/花名册验证，
Phase 3 再默认打开。

### CLI 管理后端（2026-10-23）

六个 501 已补齐：

- `acp_update_agent_preferences`：`engine_settings` 表存 enabled/env；`acp_list_agents`
  回填；env 实测能进入子进程。
- `acp_fetch_agent_latest_release`：npx 走 `npm view`，OpenCode 走 GitHub releases；
  实测 Claude 0.94.0 / OpenCode 1.19.4 / Pi 0.0.34。
- `acp_prepare_npx_agent`：安装到 `<data>/engines/<agent>`，本地 `.bin` 优先于 npx；
  Claude 0.89.1、Codex 2.2.2、Pi 0.0.34 已安装并成功 `acp_connect`。
- `acp_download_agent_binary`：OpenCode 1.18.35 下载/解压/`chmod`，本地二进制可
  `acp_connect`（需带 `acp` 参数）。
- `acp_install_pi_binary`：路由已实现（全局 npm），本机未执行。
- `acp_env_diagnostics`：Node/npm/adapter/数据目录四段检查。

前端未动；AgentOS 的 CLI 页接线后即可用。

### 全量引擎注册（2026-10-23）

按 codeg `acp/registry.rs` 的 pin 补齐 15 个内置引擎：

- npx：Claude、Codex、Gemini、OpenClaw、Cline、Hermes、CodeBuddy、Kimi、Pi、
  Grok、DeepSeek、Qoder；
- binary：OpenCode、Cursor、Antigravity（Cursor/Antigravity 整树解压，
  入口分别是 `dist-package/cursor-agent` 与 `agy_acp_server.par`）；
- `acp_list_agents` 返回 15 行；`agent_role_model_face` 对非 overlay 引擎返回
  `supported:false`，避免 Agent 页绑定一个注入不了的模型。

限制：

- per-role 模型注入已覆盖 10 个引擎；Gemini / Antigravity / CodeBuddy / Qoder /
  Cursor 仍用 CLI 自己的全局配置（原因见 §12.2）。
- Hermes / OpenClaw 未真跑 session（安装体积/网络）；其余 5 个未支持。
- launch spec 来自 codeg 的已验证 pin；Gemini / Cursor / Antigravity 等未逐个登录验证。
- Antigravity 需要 `$GEMINI_HOME/antigravity-acp/settings.json` 的 auth；
  当前没有自动写该文件。
- Cursor / Antigravity 没有稳定的 latest 端点，`acp_fetch_agent_latest_release`
  返回 null；OpenCode 走 GitHub、npx 走 npm。

---

## 12. 模型注入：现状、误区与开源复用

### 12.1 先澄清：ACP 适配 ≠ 模型注入

- **ACP 适配**：CLI 自己（或官方/社区 adapter）说的 JSON-RPC；我们只注册 launch spec，
  不写 adapter。现在已注册 15 个。
- **模型注入**：让**同一个 CLI 的不同会话**跑不同模型。这是 CLI 自己的配置问题，
  ACP 协议没有统一的“设置模型”字段。
- 所以“15 个适配都装好了”不等于“15 个都能一个会话一个模型”。目前 **10 个**
  做了 per-role 注入（Claude / Codex / OpenCode / Grok / Hermes / Kimi / OpenClaw /
  Pi / Cline / DeepSeek）；其余 5 个仍读各自 CLI 的全局配置。

一句话：**Adapters 是别人的；注入胶水是我们的。胶水可以复用开源，不必自己写。**

### 12.2 各引擎的注入机制（已确认/待确认）

| 引擎 | ACP 来源 | 每会话模型机制 | 现状 |
| --- | --- | --- | --- |
| Claude Code | `claude-agent-acp` | `CLAUDE_CONFIG_DIR/settings.json` 的 `ANTHROPIC_MODEL` | ✅ 已实现 |
| Codex | `codex-acp` | `CODEX_HOME/config.toml` 的 `model` | ✅ 已实现 |
| OpenCode | 官方 `opencode acp` | `OPENCODE_CONFIG` 的 `model: crewchat/<model>` | ✅ 已实现 |
| Grok | 官方 `grok agent stdio` | `GROK_HOME/config.toml` 的 `[models]` / `[model.*]` | ✅ 已实现，真实会话通过 |
| Hermes | `hermes acp` | `HERMES_HOME/config.yaml` 的 `model` + `custom_providers` | ✅ 已实现，真实会话通过 |
| Kimi Code | `kimi acp` | `KIMI_CODE_HOME/config.toml` 的 provider/model | ✅ 已实现，真实会话通过 |
| OpenClaw | `openclaw acp` | 每会话 `OPENCLAW_CONFIG_PATH`/`OPENCLAW_STATE_DIR` + 独立 gateway | ✅ 已实现，真实会话通过 |
| Cline | `cline --acp` | `CLINE_DIR`/`CLINE_DATA_DIR` + `providers.json`/`models.json` + `CLINE_*` env | ✅ 已实现，真实会话通过 |
| DeepSeek | `deepseek-acp` | `DSH_HOME/settings.yaml` + `DEEPSEEK_ACP_PROVIDER/MODEL` | ✅ 已实现，真实会话通过 |
| Pi | `pi-acp` | `PI_CODING_AGENT_DIR` + `models.json`/`settings.json` | ✅ 已实现，真实会话通过 |
| Gemini CLI | 官方 `gemini --acp --skip-trust` | 需要 Gemini `generateContent` 端点 | ❌ 当前 model-layer 不支持 |
| Antigravity | 官方 `agy_acp_server` | `--gemini_dir` + Gemini `settings.json` | ❌ 当前 model-layer 不支持 |
| CodeBuddy | `codebuddy --acp` | 腾讯登录/自有 API，未发现自定义 base URL | ❌ 不做 |
| Qoder | `qoder --acp` | BYOK 只在 `/model` 向导里选目录内 provider，无自定义 base URL | ❌ 不做 |
| Cursor | `cursor-agent acp` | Cursor 账号 / `CURSOR_API_KEY`，端点固定为 Cursor 后端 | ❌ 不做 |

### 12.3 复用 `cc-switch`，不自己写模板

[`cc-switch`](https://github.com/farion1231/cc-switch) 是 MIT、Rust/Tauri、147k★、2026-10 仍活跃，
已经实现了这些 CLI 的配置写入与路径解析：

| cc-switch 文件 | 可复用什么 |
| --- | --- |
| `src-tauri/src/gemini_config.rs` | `~/.gemini/.env`、`GEMINI_MODEL`、`settings.json` |
| `src-tauri/src/grok_config.rs` | `~/.grok/config.toml` 的 model/provider/env_key |
| `src-tauri/src/hermes_config.rs` | `HERMES_HOME`、`config.yaml` 的 model/provider |
| `src-tauri/src/kimi_config.rs` | `KIMI_CODE_HOME`、`config.toml` 的 provider/models |
| `src-tauri/src/openclaw_config.rs` | `openclaw.json`（JSON5）的 provider/env |
| `src-tauri/src/codex_config.rs` | `CODEX_HOME/config.toml`、model provider |
| `src-tauri/src/opencode_config.rs` | `OPENCODE_CONFIG`、provider/model |
| `src-tauri/src/agy_config.rs` | `--gemini_dir` 私有目录、`settings.json`（**原生 per-session**） |
| `src-tauri/src/pi_config/` | Pi 配置/路径 |
| `src-tauri/src/claude_*` | Claude settings / MCP / plugin |

用法：**copy + 参数化根目录**，不要直接依赖它的 Tauri/Provider 类型。

- 把“写全局 `~/.xxx`”改成“写 `<data>/roles/<role>/<xxx>`”；
- 把“设置页面切换”改成“spawn 前写一次”；
- 保留 `[from cc-switch]` 注释，`cli-layer/NOTICE.md` 记录 MIT 与源 commit。

### 12.4 注入模型抽象

实现落在 `cli-layer/src/roles.rs`：`OverlayKind` 在 `engines.rs` 里声明，
`prepare()` 按 kind 分派到各自的 writer。每个 writer 只做三件事：

1. 在 `<data>/roles/<role>-<uuid>/` 下写 CLI 原生配置；
2. 把配置目录 / key 环境变量放进 `RoleOverlay.env`；
3. 返回实际模型标签。

没有为每个引擎引入新的抽象层；新增一个 CLI 只是多一个 `OverlayKind` 和一个
writer 函数。这比先设计通用 `ModelInjection` 枚举更直接，也更容易逐个真跑。

最终落地：

- **已实现**：Grok / Hermes / Kimi / OpenClaw / Pi / Cline / DeepSeek；
  加上原有 Claude / Codex / OpenCode，共 10 个。
- **不做**：Gemini / Antigravity（当前 model-layer 无 Gemini 协议）、
  CodeBuddy / Qoder（自有登录或目录）、Cursor（Cursor 账号 + 固定端点）。
- **不追求 15 个全部注入**：一个会话一个模型的前提是 CLI 能接受 per-session 配置；
  接受不了的，不假装支持。

### 12.5 长尾的官方来源

- 官方 ACP agent 注册表：`https://cdn.agentclientprotocol.com/registry/v1/latest/registry.json`
  （当前 43 个，带 npx/binary、args/env、archive、版本）。
- 下一步可做动态 registry：启动时拉取 + 缓存，`acp_list_agents` 合并 15 个内置 + 动态条目；
  安装/启动复用现有 `prepare`/`download`/`connect`。
- 动态条目默认 `overlay: None`，模型注入仍按上表逐个补。

### 12.6 结论

- **15 个 ACP 适配已注册，不等于 15 个都支持一个 Agent 一个模型。**
- 现在 **10 个**完成 per-role 注入：Claude / Codex / OpenCode / Grok / Hermes / Kimi /
  OpenClaw / Pi / Cline / DeepSeek。
- **5 个不做**：Gemini / Antigravity（缺 Gemini 协议）、CodeBuddy / Qoder（自有登录/目录）、
  Cursor（Cursor 账号 + 固定端点）。
- **不自己写配置模板**：配置格式来自 `cc-switch`（MIT）和各家官方文档；实现只做
  “写 per-session 文件 + 注入 env”这层胶水。
- 下一刀：把未支持项写进界面提示；Hermes / OpenClaw / 其余 5 个已真实跑通。

### 12.7 API 优先：能走 API 的就不走登录

原则：**CLI 支持自定义 base URL + API key 的，统一指向 `model-layer` 的 `:3456`，
用 `local-gateway` caller key。** 不触发 OAuth，也不需要每家厂商的 API key。

| 引擎 | 协议 | 能用 model-layer | 测试成本 | 结论 |
| --- | --- | --- | --- | --- |
| Claude Code | Anthropic Messages | ✅ | 已有 | 已做 |
| Codex | OpenAI Responses | ✅ | 已有 | 已做 |
| OpenCode | OpenAI Chat | ✅ | 已有 | 已做 |
| Hermes | OpenAI / Anthropic custom provider | ✅ | 一个 gateway key | 已做，真实会话通过 |
| Kimi Code | `openai` / `openai_responses` / `anthropic` | ✅ | 一个 gateway key | 已做，真实会话通过 |
| OpenClaw | `models.providers` + `api` | ✅ | 一个 gateway key | 已做，真实会话通过（独立 gateway） |
| Grok | OpenAI Responses + `env_key` | ✅ | 一个 gateway key | 已做，真实会话通过 |
| Pi | `models.json` / `settings.json` | ✅ | 一个 gateway key | 已做，真实会话通过 |
| Cline | OpenAI-compatible | ✅ | 一个 gateway key | 已做，真实会话通过 |
| DeepSeek Harness | OpenAI-compatible via `llm-pi-ai` | ✅ | 一个 gateway key | 已做，真实会话通过 |
| CodeBuddy | 腾讯登录/自有 API | ❌ 无自定义 base URL | 需账号 | 不做 |
| Qoder | 自有 BYOK 目录 | ❌ 无自定义 base URL | 需账号 | 不做 |
| Cursor | Cursor 账号 | ❌ 无自定义端点 | 需账号 | 不做 |
| Gemini CLI | Gemini `generateContent` | ❌ model-layer 没有 Gemini 协议 | 需 Gemini key | 不做 |
| Google Antigravity | Gemini `generateContent` | ❌ | 需 Gemini key | 不做 |

**关键：不需要 15 个 API key。** Tier A 的 CLI 都可以用同一个
`local-gateway` key + 同一个模型（如 `OpenCode Go/mimo-v2.6-flash`）做冒烟。

### 12.8 接入与测试顺序

**Tier A（现在就能测，建议先做）**

1. **Grok**：每会话 `GROK_HOME`，`config.toml` 写 `[models]` / `[model.crewchat]`
   的 `base_url` / `api_backend = "responses"` / `env_key`。
2. **Hermes**：每会话 `HERMES_HOME`，`config.yaml` 的 `custom_providers` 指向
   `:3456`。
3. **Kimi**：每会话 `KIMI_CODE_HOME`，`config.toml` 的 provider 用
   `type = "openai"`，指向 `:3456`。
4. **OpenClaw**：每会话 `OPENCLAW_CONFIG_PATH`，`openclaw.json` 的
   `models.providers` + `agents.defaults.model` 指向 `:3456`。
5. **Pi**：每会话 `PI_CODING_AGENT_DIR`，`models.json` / `settings.json` 指向
   `:3456`。

**Tier B（已调研）**

- **Cline**：`CLINE_DIR`/`CLINE_DATA_DIR` + `providers.json`/`models.json`，
  `CLINE_PROVIDER` / `CLINE_MODEL` / `CLINE_API_KEY`；自定义 provider 走
  `openai-compatible`。
- **DeepSeek**：`DSH_HOME/settings.yaml` 的 `llm-pi-ai.providers`，`DEEPSEEK_ACP_PROVIDER`
  / `DEEPSEEK_ACP_MODEL`；`apiKeyEnv: CODEG_CCR_GATEWAY_KEY`。
- **CodeBuddy**：文档只有腾讯登录与 `--model`，没有自定义 base URL；不做。
- **Qoder**：BYOK 只能在 `/model` 的 Custom 向导里选目录内 provider，不能填任意
  base URL；不做。
- **Cursor**：`CURSOR_API_KEY` 是 Cursor 账号 key，`-e` 指向 Cursor 后端，不是
  OpenAI 兼容端点；不做。

**Tier C（需要 Gemini 协议）**

- Gemini CLI / Antigravity 要的是 Gemini `generateContent`，不是 OpenAI。
- 要么给 model-layer 加一个 Gemini 兼容端点（OpenAI → Gemini 翻译），
  要么用厂商 Gemini key；当前两个都没有，所以先不承诺。
- 这也意味着它们不需要现在测。

### 12.9 测试策略（没有很多 API key 也能测）

- **一个 key 测所有 API-mode CLI**：`model-layer` 的 `local-gateway` key；
  上游用现有 `OpenCode Go` 或 `OpenRouter` 任意一个模型。
- 每个 CLI 的 smoke：临时 home + 指向 `:3456` 的配置 + `acp_connect` →
  prompt `Reply with exactly: <engine>-ok` → `turn_complete`。
- 不碰登录：配置里走“custom provider / API key”分支，不走 OAuth。
- 2026-10-23 实测通过：Grok / Hermes / Kimi / OpenClaw / Pi / Cline / DeepSeek
  均返回 `<engine>-ok`，共用一个 `local-gateway` key 和
  `OpenCode Go/mimo-v2.6-flash`。
- OpenClaw 的 ACP 是 Gateway bridge，不是独立 agent；host 会为每个 role 启动
  独立 `openclaw gateway run`（独立 port/config/state），session 结束随连接回收。
- 不能测的（Gemini/Antigravity/Cursor/CodeBuddy/Qoder）：
  - 只做 config writer 的单元测试（temp dir 写/读）；
  - 在 `acp_list_agents` 里保留行，但 `agent_role_model_face` 返回
    `supported:false`，并注明原因。
- 不在没 key 的情况下假装跑过；文档和界面都显示“未验证”。

### 12.10 结论

- **登录类 CLI 不是不能接，而是没必要优先接**：我们的 provider 是 API，
  API-mode 的 CLI 统一指向 model-layer，一个 key 就能测。
- **Tier A 五个**（Grok / Hermes / Kimi / OpenClaw / Pi）与 **Tier B 两个**
  （Cline / DeepSeek）全部实现，且 7 个新增都已真跑通。
- **Gemini / Antigravity** 需要 Gemini 协议，**Cursor / CodeBuddy / Qoder** 只有
  自家登录或目录；先不承诺，保留行并标 `supported:false`。

### 12.11 实现记录（2026-10-23）

代码：

- `cli-layer/src/engines.rs`：`OverlayKind` 扩展到 10 个；`EngineSpec.overlay`
  指向对应 writer；未支持的 5 个仍是 `None`。
- `cli-layer/src/roles.rs`：新增 Grok / Hermes / Kimi / OpenClaw / Pi / Cline /
  DeepSeek 的 per-session writer；OpenClaw 额外分配独立 gateway port。
- `cli-layer/src/acp/host.rs`：连接 OpenClaw 前启动 per-role gateway，ACP 命令追加
  `--url ws://127.0.0.1:<port>`，连接关闭时随 `ConnectionHandle` 回收。
- `cli-layer/src/engine_manager.rs`：npx 安装加 `--legacy-peer-deps` /
  `--include=optional`；DeepSeek 额外 `--force` 以跨过 `0.1.5-rc.1` /
  `-rc.3` 的 peer 图。
- `agent_role_model_face` 不再对这些引擎返回 `supported:false`。

验证：

- `cargo test`：18 passed（新增 7 个 writer 单测 + 1 个参数转义单测）。
- `cargo clippy --all-targets -- -D warnings`：通过。
- 真实 ACP session（同一 `local-gateway` key + `OpenCode Go/mimo-v2.6-flash`）：
  - Grok `grok-ok`
  - Hermes `hermes-ok`
  - Kimi `kimi_code-ok`
  - OpenClaw `openclaw-ok`（独立 gateway）
  - Pi `pi-ok`
  - Cline `cline-ok`
  - DeepSeek `deepseek-ok`
- 未支持：Gemini / Antigravity / CodeBuddy / Qoder / Cursor（协议或登录不支持）。

文档依据：

- Grok Build settings：`https://docs.x.ai/build/settings`（`GROK_HOME`、
  `[models]` / `[model.*]`、`env_key`）。
- Kimi Code configuration：`https://moonshotai.github.io/kimi-code/en/configuration/config-files.html`
  （`KIMI_CODE_HOME`、`providers` / `models`）。
- Hermes configuration：`https://hermes-agent.nousresearch.com/docs/user-guide/configuration`
  （`HERMES_HOME`、`model`、`custom_providers`）。
- OpenClaw custom providers：`https://docs.openclaw.ai/concepts/model-providers/custom-providers`
  （`models.providers`、`${VAR}`、`agents.defaults.model`）。
- Pi models：`https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/models-configuration.md`
  （`models.json`、`$ENV`、`PI_CODING_AGENT_DIR`）。
- Cline CLI / config：`https://docs.cline.bot/cli/cli-reference`、
  `https://docs.cline.bot/getting-started/config`（`CLINE_DIR`、`providers.json`、
  `models.json`）。
- DeepSeek ACP：`https://github.com/xintaofei/deepseek-acp`（`DSH_HOME`、
  `DEEPSEEK_ACP_PROVIDER/MODEL`、`llm-pi-ai` provider）。
- Qoder custom models：`https://docs.qoder.com/cli/custom-models`（仅目录内 BYOK）。
- Cursor ACP：`https://cursor.com/docs/cli/acp`（`cursor_login` / `CURSOR_API_KEY`）。
