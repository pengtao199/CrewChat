# 数据真相源

## 适用范围

新增或移动任何持久化数据、决定配置写到哪里、改动 `AGENTS/`、`.data/`、`~/.agents/` 时。

## 目标

任何一份数据只有一个写入方和一个可预期的位置，并且用户能知道它在哪。

## 位置清单

| 数据 | 路径 | 格式 | 跨工具共享 | 可提交 |
| --- | --- | --- | --- | --- |
| Agent 定义（全局花名册） | Codeg 数据目录的 `codeg.db` → `custom_agent`（`kind='role'`） | SQLite | 否 | 否 |
| 花名册遗留存储（冻结只读） | `.data/agents/*.jsonc`、`.data/codeg/agent-roles.json` | JSONC / JSON | 否 | 否 |
| 大脑池（通用 provider） | `.data/providers.jsonc` | JSONC | 否 | **否（含 key）** |
| 模型供应商 / 路由 / 密钥（CCR） | CCR 的 home（本机 `~/.claude-code-router/`） | SQLite（`config.sqlite`） | 否 | 否 |
| 项目列表（工作区/一等公民） | `.data/projects.json` | JSON | 否 | 否 |
| 界面偏好 | `.data/crewchat.db` | SQLite | 否 | 否 |
| 会话与消息 | `.data/crewchat.db` | SQLite | 否 | 否 |
| 用量统计（经本 app 的 CLI 身体） | `.data/crewchat.db`（`usage_events`） | SQLite | 否 | 否 |
| 技能 | 各 CLI 自己的技能目录（`~/.claude/skills`、`~/.codex/skills`、`~/.config/opencode/skills`、`~/.gemini/skills`…）+ 共享 `~/.agents/skills`；项目级在 `<workspace>/.<cli>/skills` / `.agents/skills` | Markdown（`SKILL.md`） | **是** | — |
| MCP 服务 | 各 CLI 自己的 MCP 配置（Claude/Codex/Gemini/OpenCode…）+ 共享 `~/.agents/mcp.json` | JSON / TOML | **是** | — |
| 插件 | `~/.agents/plugins/*` | 目录 | **是** | — |
| 本地覆盖（CLI 参数等） | `.data/overrides.json` | JSON | 否 | 否 |

## 原则

- **文件优先于数据库**：可被人阅读、被 git 管理、被其他工具消费的数据放文件；纯运行时状态放数据库。
- **跨工具共享的数据不复制**：技能与 MCP 由各 CLI 自己的目录/配置承载，`~/.agents/` 是共享来源之一；不把它们复制进 AgentOS 的数据库，也不在 `.data/` 里做镜像。
- **密钥只落一处**：模型端点 API key 存在 CCR 的供应商配置（模型页写入）；编排层自身的大脑池 `.data/providers.jsonc` 同样含 key。两处都禁止散落到 agent 文件、代码或前端。
- **模型统一管理在 CCR**：供应商 / 路由的真相源是 CCR（经模型页写入）；Python 编排层保留自己的大脑池 `.data/providers.jsonc`（`core/providers.py`），但身体执行不再注入模型——CLI 用哪个模型由 CCR 写进其全局配置（见 `AGENTS/gateway.md`）。
- **界面必须标注位置**：任何会写入文件的操作，界面要显示「保存到 → 路径」。
- **权限审批在 Codeg**：Python 侧没有权限开关与卡片（自跑身体时兜底放行，见 `AGENTS/bodies.md`）；产品面的审批走 Codeg。
- **Agent 全局，与工作区解耦**：agent 是「公司的人」，**不归任何项目**；团队的多个 agent 可在不同工作区协作。花名册的**唯一可写入口是 Codeg 的 Agents 页**（`agent_roles_save` → Codeg 数据目录 `codeg.db` 的 `custom_agent` 表，`kind='role'`）。`.data/agents/*.jsonc` 与 `.data/codeg/agent-roles.json` 是 Phase 2.1 之前的历史存储：已由一次性迁移导入（标记 `app_metadata.crewchat.agent_roles_migrated`），此后**冻结只读**，只作备份保留。
- **会话的工作区可选（cwd 三态）**：绑了工作区 → 用该 `project.path`；没绑 → 每会话一个沙盒目录 `.data/workspaces/<session_key>`（避免并发互踩）。侧栏把未绑定的会话显示在「会话工作区」分组下。
- **项目是一等公民、工作区就是项目目录**：`Project.path`（一个文件夹）既是 agent 的执行 cwd，也是这个项目的协作工作区（agent 定义是全局的，不归项目）。**会话可选归属一个项目**（`conversations.project_id` 可空）：`api/chat.py` 按会话的项目建 Session，未绑定时用每会话沙盒。项目列表存 `.data/projects.json`（经 `core/projects.py` 读写），不在客户端用 `extra.workspace` 当真相源。
- **用量统计归我们、且只算我们发起的**：用量由身体运行时上报（`ExecResult.usage`）、`api/chat.py` 落进 `.data/crewchat.db` 的 `usage_events`。不读 CLI 自己的用量库——那里掺了用户在终端里裸跑 CLI 的消耗。只有经本 app 发起的 CLI 身体产生了用量才落库。

## 具体规则

- 修改 Agent 只经 Agents 页写 `codeg.db` 的 `custom_agent`（角色行）；不再写 `.data/agents/*.jsonc`（冻结只读）。
- 读写技能/MCP 一律通过核心命令：`acp_list_agent_skills` / `acp_read_agent_skill` / `acp_save_agent_skill` / `acp_delete_agent_skill` 与 `mcp_scan_local` / `mcp_upsert_local_server` / `mcp_remove_server`。壳不自行拼路径、不直接改这些文件。
- 技能页显示 `SkillItem.path`；MCP 页显示 `spec` 与写入的 CLI 集合（核心的 scan 不返回每个 CLI 的配置文件路径）。
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
