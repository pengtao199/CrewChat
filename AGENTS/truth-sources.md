# 数据真相源

## 适用范围

新增或移动任何持久化数据、决定配置写到哪里、改动 `AGENTS/`、`.data/`、`~/.agents/` 时。

## 目标

任何一份数据只有一个写入方和一个可预期的位置，并且用户能知道它在哪。

## 位置清单

| 数据 | 路径 | 格式 | 跨工具共享 | 可提交 |
| --- | --- | --- | --- | --- |
| Agent 定义（全局花名册） | `~/Library/Application Support/AgentOS/cli/cli-layer.db` → `agent_roles` | SQLite | 否 | 否 |
| 花名册遗留存储（冻结只读） | 旧 `codeg.db` 的 `custom_agent`、`.data/agents/*.jsonc`、`.data/codeg/agent-roles.json` | SQLite / JSONC / JSON | 否 | 否 |
| 大脑池（通用 provider） | `.data/providers.jsonc` | JSONC | 否 | **否（含 key）** |
| 模型供应商 / 路由 / 密钥（模型层） | 元数据与 Claude 目录快照：`~/Library/Application Support/AgentOS/model/`（`model.db`、`claude-models.json`）；密钥：macOS Keychain（service `com.crewchat.model-layer`） | SQLite（引用）+ Keychain + JSON | 否 | 否 |
| 旧 CCR 配置（回退用，冻结） | `~/.claude-code-router/` | SQLite（`config.sqlite`） | 否 | 否 |
| 项目列表（工作区/一等公民） | `.data/projects.json` | JSON | 否 | 否 |
| 界面偏好 | `.data/crewchat.db` | SQLite | 否 | 否 |
| 会话与消息 | `~/Library/Application Support/AgentOS/cli/cli-layer.db`（`conversations` / `turns` / `events`） | SQLite | 否 | 否 |
| 用量统计（经本 app 的 CLI 身体） | `.data/crewchat.db`（`usage_events`） | SQLite | 否 | 否 |
| 技能 | 全局库：`~/.agents/skills`（current 软链）+ `~/.agents/skill-store/<id>/<hash>`，注册表 `~/.agents/skill-layer/registry.sqlite`；Agent 绑定：cli-layer `agent_roles.skills_json`；会话级 materialize：role home 的 `skills/` | Markdown + symlink + SQLite | **是**（内容跨 CLI） | — |
| MCP 服务 | 各 CLI 自己的 MCP 配置（Claude/Codex/Gemini/OpenCode…）+ 共享 `~/.agents/mcp.json` | JSON / TOML | **是** | — |
| 插件 | `~/.agents/plugins/*` | 目录 | **是** | — |
| 本地覆盖（CLI 参数等） | `.data/overrides.json` | JSON | 否 | 否 |

## 原则

- **文件优先于数据库**：可被人阅读、被 git 管理、被其他工具消费的数据放文件；纯运行时状态放数据库。
- **跨工具共享的数据不复制**：技能与 MCP 由各 CLI 自己的目录/配置承载，`~/.agents/` 是共享来源之一；不把它们复制进 AgentOS 的数据库，也不在 `.data/` 里做镜像。
- **密钥只落一处**：模型端点 API key 与 caller key 存在 macOS Keychain（service `com.crewchat.model-layer`），SQLite 只存 `keychain_ref`；编排层自身的大脑池 `.data/providers.jsonc` 同样含 key。两处都禁止散落到 agent 文件、代码或前端。
- **模型统一管理在模型层**：供应商 / 路由 / 额度的真相源是 `model-layer/`（经模型页写入）；Python 编排层保留自己的大脑池 `.data/providers.jsonc`（`core/providers.py`），但身体执行不再注入模型——每个 Agent 的模型由 `agent_host/overlay` 注入，网关按请求路由（见 `AGENTS/gateway.md`）。
- **界面必须标注位置**：任何会写入文件的操作，界面要显示「保存到 → 路径」。
- **权限审批在 cli-layer**：ACP session 的 `session/request_permission` 由 cli-layer 转成事件，AgentOS 批准/拒绝；codeg 不再是运行时审批方。
- **Agent 全局，与工作区解耦**：agent 是「公司的人」，**不归任何项目**；团队的多个 agent 可在不同工作区协作。花名册的**唯一可写入口是 AgentOS 的 Agents 页**（`agent_roles_save` → `~/Library/Application Support/AgentOS/cli/cli-layer.db` 的 `agent_roles` 表）。旧 `codeg.db` 的 `custom_agent`、`.data/agents/*.jsonc` 与 `.data/codeg/agent-roles.json` 是迁移前历史，**冻结只读**，只作回退/导入来源。
- **会话的工作区可选（cwd 三态）**：绑了工作区 → 用该 `project.path`；没绑 → 每会话一个沙盒目录 `.data/workspaces/<session_key>`（避免并发互踩）。侧栏把未绑定的会话显示在「会话工作区」分组下。
- **项目是一等公民、工作区就是项目目录**：`Project.path`（一个文件夹）既是 agent 的执行 cwd，也是这个项目的协作工作区（agent 定义是全局的，不归项目）。**会话可选归属一个项目**（`conversations.project_id` 可空）：`api/chat.py` 按会话的项目建 Session，未绑定时用每会话沙盒。项目列表存 `.data/projects.json`（经 `core/projects.py` 读写），不在客户端用 `extra.workspace` 当真相源。
- **用量统计归我们、且只算我们发起的**：用量由身体运行时上报（`ExecResult.usage`）、`api/chat.py` 落进 `.data/crewchat.db` 的 `usage_events`。不读 CLI 自己的用量库——那里掺了用户在终端里裸跑 CLI 的消耗。只有经本 app 发起的 CLI 身体产生了用量才落库。

## 具体规则

- 修改 Agent 只经 Agents 页写 `cli-layer.db` 的 `agent_roles`；不再写旧 `codeg.db` 或 `.data/agents/*.jsonc`（冻结只读）。
- 读写技能/MCP 一律通过核心命令：技能用 `skill_list` / `skill_read` / `skill_install` / `agent_skills_set` / `agent_skill_status`，旧命令 `acp_list_agent_skills` / `acp_read_agent_skill` 仍作兼容；MCP 用 `mcp_scan_local` / `mcp_upsert_local_server` / `mcp_remove_server`。壳不自行拼路径、不直接改这些文件。
- 技能页由 cli-layer 代理 skill-layer；MCP 页显示 `spec` 与写入的 CLI 集合（核心的 scan 不返回每个 CLI 的配置文件路径）。
- `.data/` 整体被 gitignore；新增本地状态放进 `.data/`，不要放仓库根目录。
- 新增配置文件时，先判断它是否属于「跨工具共享」（`~/.agents/`）还是「本项目私有」（`.data/`）。
- 会话消息以 SQLite 为准；UI 端的乐观渲染不是真相源。

## 禁止项

- 不在代码里硬编码绝对路径（例如 `/Users/xxx/...`）；一律经 `config` 推导。
- 不把 `~/.agents/` 的内容复制进 `.data/`。
- 不让同一份配置有两个可写入口（例如界面和手改文件同时被支持却不同步）。
- 不把密钥写进花名册（`custom_agent` 的角色行）、日志或错误信息。

## 检查

- 这份数据重启后从哪读？只有一个答案吗？
- 用户能在界面上知道它存在哪里吗？
- 它是否出现在 `.gitignore` 该覆盖的范围内？
- 另一款工具（codex / claude / opencode）改了它，我们需要感知吗？
