# 数据真相源

## 适用范围

新增或移动任何持久化数据、决定配置写到哪里、改动 `agents/`、`.data/`、`~/.agents/` 时。

## 目标

任何一份数据只有一个写入方和一个可预期的位置，并且用户能知道它在哪。

## 位置清单

| 数据 | 路径 | 格式 | 跨工具共享 | 可提交 |
| --- | --- | --- | --- | --- |
| Agent 定义（全局花名册） | `.data/agents/*.jsonc` | JSONC | 否 | 否 |
| 大脑池（通用 provider） | `.data/providers.jsonc` | JSONC | 否 | **否（含 key）** |
| 网关派生物 + 进程状态 | `.data/gateway/`（`ccsw-proxy` 的 home） | SQLite(schema) + 目录 | 否 | 否 |
| 项目列表（工作区/一等公民） | `.data/projects.json` | JSON | 否 | 否 |
| 界面偏好 | `.data/crewchat.db` | SQLite | 否 | 否 |
| 会话与消息 | `.data/crewchat.db` | SQLite | 否 | 否 |
| 用量统计（经本 app 的 CLI 身体） | `.data/crewchat.db`（`usage_events`） | SQLite | 否 | 否 |
| 技能 | `~/.agents/skills/*/SKILL.md` | Markdown | **是** | — |
| MCP 服务 | `~/.agents/mcp.json` | JSON | **是** | — |
| 插件 | `~/.agents/plugins/*` | 目录 | **是** | — |
| 本地覆盖（CLI 参数等） | `.data/overrides.json` | JSON | 否 | 否 |

## 原则

- **文件优先于数据库**：可被人阅读、被 git 管理、被其他工具消费的数据放文件；纯运行时状态放数据库。
- **跨工具共享的数据不复制**：技能与 MCP 直接读 `~/.agents/`，不导入、不镜像。
- **密钥只落一处**：端点 API key 只存在 `.data/providers.jsonc`（网关库里那份是投影出来的派生物），禁止散落到 agent 文件、代码或前端。
- **大脑池是模型的唯一来源**：`.data/providers.jsonc` 里的通用 provider（`core/providers.py`）是唯一真相源；不存在“默认端点”这类平行配置。CLI 没写 `brain` 时用各自默认模型。
- **网关库是派生物**：网关里按 CLI 分的那几份 provider 由通用 provider **投影**而来（`core/gateway.py` 的 `Store.project_provider()`），删了能重建。改通用 provider 是唯一的写入方式，不改网关库里的单条。
- **核心复用、数据归我们**：网关复用 cc-switch 的核心（`ccsw-proxy`），但它的 home 由 `Settings.gateway_dir` 指定（默认 `.data/gateway/`，经 `CC_SWITCH_HOME` 注入），**绝不写用户本机的 `~/.cc-switch/`**。
- **界面必须标注位置**：任何会写入文件的操作，界面要显示「保存到 → 路径」。
- **Agent 全局，与工作区解耦**：agent 是「公司的人」，定义存 `.data/agents/`（`settings.agents_home`），**不归任何项目**；团队的多个 agent 可在不同工作区协作。旧仓库内 `config/agents` 首次访问时一次性迁入（`.data/.agents-migrated` 标记，只做一次）。
- **会话的工作区可选（cwd 三态）**：绑了工作区 → 用该 `project.path`；没绑 → 每会话一个沙盒目录 `.data/workspaces/<session_key>`（避免并发互踩）。侧栏把未绑定的会话显示在「会话工作区」分组下。
- **项目是一等公民、工作区就是项目目录**：`Project.path`（一个文件夹）既是 agent 的执行 cwd，也是这个项目的协作工作区（agent 定义是全局的，不归项目）。**会话可选归属一个项目**（`conversations.project_id` 可空）：`api/chat.py` 按会话的项目建 Session，未绑定时用每会话沙盒。项目列表存 `.data/projects.json`（经 `core/projects.py` 读写），不在客户端用 `extra.workspace` 当真相源。
- **用量统计归我们、且只算我们发起的**：用量由身体运行时上报（`ExecResult.usage`）、`api/chat.py` 落进 `.data/crewchat.db` 的 `usage_events`。**不读 cc-switch 的库**——它的用量掺了用户在终端里裸跑 CLI 的消耗。只有经本 app 发起的 CLI 身体产生了用量才落库。

## 具体规则

- 修改 Agent 只写 `.data/agents/<id>.jsonc`，不写数据库副本。
- 读取技能/MCP 一律通过 `providers.py` 的 `available_skills()` / `shared_mcp_servers()`，不自行拼路径。
- `.data/` 整体被 gitignore；新增本地状态放进 `.data/`，不要放仓库根目录。
- 新增配置文件时，先判断它是否属于「跨工具共享」（`~/.agents/`）还是「本项目私有」（`.data/`）。
- 会话消息以 SQLite 为准；UI 端的乐观渲染不是真相源。

## 禁止项

- 不在代码里硬编码绝对路径（例如 `/Users/xxx/...`）；一律经 `config` 推导。
- 不把 `~/.agents/` 的内容复制进 `.data/`。
- 不让同一份配置有两个可写入口（例如界面和手改文件同时被支持却不同步）。
- 不把密钥写进 `.data/agents/*.jsonc`、日志或错误信息。

## 检查

- 这份数据重启后从哪读？只有一个答案吗？
- 用户能在界面上知道它存在哪里吗？
- 它是否出现在 `.gitignore` 该覆盖的范围内？
- 另一款工具（codex / claude / opencode）改了它，我们需要感知吗？
