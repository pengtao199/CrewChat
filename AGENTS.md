# CrewChat 项目规则

多 Agent 协作桌面应用。当前运行时：macOS 壳 **AgentOS**（`AgentOS/`，Swift）、
核心 **cli-layer/**（Rust，ACP 宿主 / 会话 / 花名册 / 引擎管理）、模型层 **model-layer/**（Rust，
统一模型管理、本地网关与控制面）、技能层 **skill-layer/**（Rust，全局技能库与会话 materialize）。
`codeg/` 与 `ccr/` 已冻结为只读回退；v1 的 Python 编排层归档在 `archive/`，均不参与当前运行时。

花名册里只有两份预置 Agent 了解本应用：`agent-helper`（改别人的 Agent 配置）和 `crewchat-manager`（群聊 Leader）。其余 Agent 是通用角色，不绑定本仓库。细节见 `AGENTS/agent-model.md`。

## 执行入口

1. 开始任务前先匹配已安装的 skill、插件、MCP，再读下方专项规则；规则优先于通用最佳实践。
2. 改动领域模型（Agent / 大脑 / 身体 / 真相源）前，必须读 `AGENTS/agent-model.md` 与 `AGENTS/truth-sources.md`。
3. 改动 `codeg/` 下的界面前，必须读 `AGENTS/ui.md`；它约束二次开发方式。
4. 新增或修改 `/api/*`、`/ws` 事件前，必须读 `AGENTS/api-contract.md`。
5. 新增「身体」（执行器）前，必须读 `AGENTS/bodies.md`。
6. 声称"已验证"之前必须读 `AGENTS/verification.md` 并实跑对应检查。
7. 改动 `AgentOS/`（macOS Swift 壳）前，必须读 `AGENTS/agentos-boundary.md`，再按下方索引读对应条目；壳只做渲染，真相与执行归 Rust 核心。
8. 新增专项规则放入 `AGENTS/`，使用表达约束主题的英文小写文件名，并在本索引登记唯一适用边界。**规则只有一个系统**：本文件（项目规则 + 索引）+ `AGENTS/` 下的细分规则；不在任何子目录里另开规则目录。
9. 探索代码结构（符号、调用路径、影响面）先用 CodeGraph：本仓库已建索引（`.codegraph/`，`codegraph status` 查看），改动用 `codegraph sync` 增量更新；不要重复 `init`。

## 子规则索引

| 规则文件 | 必须阅读的场景 |
| --- | --- |
| `AGENTS/architecture.md` | 新增目录或模块、判断代码该放哪条腿、调整仓库结构时。 |
| `AGENTS/agent-model.md` | 涉及 Agent、大脑（模型端点）、身体（CLI）、技能、MCP、记忆、规则，或会话与 Agent 的绑定关系时。 |
| `AGENTS/truth-sources.md` | 新增或移动任何持久化数据、决定配置写到哪里、改动 `.data/`、`~/.agents/` 时。 |
| `AGENTS/bodies.md` | 新增或修改执行器时（**产品侧身体在 Codeg 的 ACP agent + `agent_host/overlay`**；本文件描述已冻结的 Python 执行器抽象），或处理 CLI 版本兼容、流式事件、会话记忆时。 |
| `AGENTS/gateway.md` | 改动模型接入、上游端点、CCR profile / 配置，或遇到"某 CLI 不支持某端点"类问题时。 |
| `AGENTS/api-contract.md` | 新增或修改 `/api/*` 路由、WebSocket 事件、消息流式帧，或前端报 404/501 时。 |
| `AGENTS/ui.md` | 改动 `codeg/` 或 `AgentOS/` 下的界面、交互或文案时。 |
| `AGENTS/naming.md` | 新建、移动、重命名目录、文件、类型、模块、规则文件，或使用领域词汇时。 |
| `AGENTS/verification.md` | 完成任何改动、准备汇报或提交之前。 |
| `AGENTS/dependencies.md` | 新增依赖、选择开源实现、升级现有库、遇到网络受限时。 |
| `AGENTS/git.md` | 执行 Git 初始化、暂存、提交、推送、分支操作，或修改 `.gitignore` 时。 |
| `AGENTS/upstream.md` | 改动 `codeg/` 里的上游代码，或维护 `ccr/` 回退基座时。 |
| `AGENTS/agentos-boundary.md` | 改动 `AgentOS/`、判断某能力该不该进壳、或壳与核心的边界/范围/失败降级时。**进壳先读这条。** |
| `AGENTS/agentos-layout.md` | 新增 `AgentOS/` 下的目录或模块、调整壳的分层、给壳引入依赖时。 |
| `AGENTS/agentos-transport.md` | 新增 `AgentOS/` 对核心的调用、订阅事件、处理流式 / 重连 / 取消 / 错误时。 |
| `AgentOS/engine-api.md` | 每次新增 `AgentOS/` 对核心的调用（必须同步登记，它是对核心接口的唯一清单）。 |
