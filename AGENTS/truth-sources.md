# 数据真相源

## 适用范围

新增或移动任何持久化数据、决定配置写到哪里、改动 `agents/`、`.data/`、`~/.agents/` 时。

## 目标

任何一份数据只有一个写入方和一个可预期的位置，并且用户能知道它在哪。

## 位置清单

| 数据 | 路径 | 格式 | 跨工具共享 | 可提交 |
| --- | --- | --- | --- | --- |
| Agent 定义 | `config/agents/*.jsonc` | JSONC | 否 | 是 |
| 大脑池 | `.data/providers.jsonc` | JSONC | 否 | **否（含 key）** |
| 项目列表 | `.data/projects.jsonc` | JSON | 否 | 否 |
| 界面偏好 | `.data/crewchat.db` | SQLite | 否 | 否 |
| 会话与消息 | `.data/crewchat.db` | SQLite | 否 | 否 |
| 技能 | `~/.agents/skills/*/SKILL.md` | Markdown | **是** | — |
| MCP 服务 | `~/.agents/mcp.json` | JSON | **是** | — |
| 插件 | `~/.agents/plugins/*` | 目录 | **是** | — |
| 本地覆盖（CLI 参数等） | `.data/overrides.json` | JSON | 否 | 否 |

## 原则

- **文件优先于数据库**：可被人阅读、被 git 管理、被其他工具消费的数据放文件；纯运行时状态放数据库。
- **跨工具共享的数据不复制**：技能与 MCP 直接读 `~/.agents/`，不导入、不镜像。
- **密钥只落一处**：API key 只存在 `.data/providers.jsonc`（以及必要的环境变量），禁止散落到 agent 文件、代码或前端。
- **界面必须标注位置**：任何会写入文件的操作，界面要显示「保存到 → 路径」。

## 具体规则

- 修改 Agent 只写 `config/agents/<id>.jsonc`，不写数据库副本。
- 读取技能/MCP 一律通过 `providers.py` 的 `available_skills()` / `shared_mcp_servers()`，不自行拼路径。
- `.data/` 整体被 gitignore；新增本地状态放进 `.data/`，不要放仓库根目录。
- 新增配置文件时，先判断它是否属于「跨工具共享」（`~/.agents/`）还是「本项目私有」（`.data/`）。
- 会话消息以 SQLite 为准；UI 端的乐观渲染不是真相源。

## 禁止项

- 不在代码里硬编码绝对路径（例如 `/Users/xxx/...`）；一律经 `config` 推导。
- 不把 `~/.agents/` 的内容复制进 `.data/`。
- 不让同一份配置有两个可写入口（例如界面和手改文件同时被支持却不同步）。
- 不把密钥写进 `config/agents/*.jsonc`、日志或错误信息。

## 检查

- 这份数据重启后从哪读？只有一个答案吗？
- 用户能在界面上知道它存在哪里吗？
- 它是否出现在 `.gitignore` 该覆盖的范围内？
- 另一款工具（codex / claude / opencode）改了它，我们需要感知吗？
