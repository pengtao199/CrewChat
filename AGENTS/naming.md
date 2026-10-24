# 命名

## 适用范围

新建、移动、重命名目录、文件、类型、模块、规则文件时。

## 目标

从名字就能判断它属于哪一层、承担什么职责，且与领域模型词汇表一致。

## Python（归档层）

> 该层已冻结在 `archive/python-orchestrator/`，不参与运行时；下表的命名约定只在阅读/维护归档时适用。

| 职责 | 命名 | 示例 |
| --- | --- | --- |
| 领域模型 | 名词，单数 | `AgentSpec`、`Provider`、`Project` |
| 执行器 | `<name>_body.py` | `codex_body.py`、`opencode_body.py` |
| 契约层路由 | `<domain>_api.py` 或 `<domain>.py` | `providers_api.py`、`conversations.py` |
| 运行时对象 | 名词 | `Session`、`ExecRequest`、`ExecResult` |
| 函数 | 动词 + 宾语 | `stream_ask`、`load_specs`、`probe_codex` |
| 常量 | UPPER_SNAKE_CASE | `ACCESS_COOKIE`、`SANDBOX_CHOICES` |

- 文件名小写 + 下划线；不用 `utils` / `helpers` / `common` 作为长期文件名。
- 归档层顶层目录只有四个，与它的分层契约一一对应：`core`（L0+L1）· `executors`（L2）· `orchestration`（L3）· `api`（L4）；检查由 `archive/python-orchestrator/scripts/check_layers.py` 守。
- 类型名表达领域角色，不用 `Data` / `Model` / `Service` 等模糊词。

## 前端

| 职责 | 命名 | 示例 |
| --- | --- | --- |
| 组件文件 | PascalCase | `AgentComposerContent.tsx` |
| 组件类型 | PascalCase | `SettingsCardRow` |
| 目录（分类） | 小写 | `components/`、`hooks/` |
| 目录（业务模块） | PascalCase | `AgentComposer/`、`GroupedHistory/` |
| 目录（平台） | 小写，与后端 `executors/` 对齐 | `codex/`、`claude/` |
| Hook | `use` + 名词 | `useProviders`、`useAgents` |
| 工具函数文件 | camelCase | `formatTime.ts` |

## AgentOS 壳（Swift）

| 职责 | 命名 | 示例 |
| --- | --- | --- |
| 类型 | UpperCamelCase | `CoreSessionSnapshot`、`ConversationRowModel` |
| 成员与函数 | lowerCamelCase | `subscriptionID`、`apply(envelope:)` |
| 功能目录 | UpperCamelCase | `Features/SessionView/` |
| 文件 | 与主类型同名，一文件一主类型 | `SessionView.swift` |

三类类型靠前缀与位置区分，映射只发生在 `Core/`：

| 种类 | 位置 | 职责 |
| --- | --- | --- |
| wire DTO | `Core/Wire/`，`Core` 前缀 | 只做 `Codable` 解码，字段名贴核心线上形状，不二次改名 |
| 领域类型 | `Domain/`，无前缀 | 壳自己的语言；由 `Core` 从 DTO 映射而来；可含纯计算 |
| 展示态 | `Features/<Feature>/` | 选中项、草稿、开合、加载中；不跨功能共享 |

## 规则文件

- 文件名：表达约束主题的英文小写单词，可含连字符。示例：`agent-model.md`、`agentos-boundary.md`。
- 一个文件只覆盖一个边界；边界重叠时合并或拆分，不允许"也都沾一点"。
- 新增规则文件必须同步在根 `AGENTS.md` 的索引表登记，并写明**必须阅读的场景**。
- 规则只有一个系统：根 `AGENTS.md`（项目规则 + 索引）+ 本目录的小写细分规则。不在任何子目录里另开规则目录。

## 领域词汇表（跨层统一）

| 概念 | 使用 | 不用 |
| --- | --- | --- |
| Agent | Agent、AgentSpec | Assistant、Worker、Bot |
| 内置 Agent | `agentos-helper`（改配置）与 `agentos-manager`（群聊 Leader）；无独立类型 | 独立 Assistant 类型 |
| 大脑 | brain、Provider | model（指大脑的一部分时可用）、LLM（指执行方式时不用） |
| 引擎（大脑的执行体） | 代码 `body` / `Executor`；用户可见「引擎」；**不指 Rust 核心** | backend、agent-cli、runner、Runtime |
| 核心 | Core（Rust `crewchat-cli`） | 引擎、后端、服务端 |
| 技能 | skill | capability、plugin |
| MCP | mcp、MCP 服务 | tool（tool 是 MCP 暴露的项） |
| 会话 | conversation、session（记忆键） | chat、thread |
| 轮次 | turn（用户一次输入触发的一轮） | run、job |
| 团队 | team（会话里的成员集合） | group、room |
| 项目 | project | workspace（workspace 是项目的路径属性） |
| 壳 | AgentOS | client、frontend |

## 禁止项

- 不用 `Common` / `Utils` / `Helper` / `Manager` / `Base` / `Misc` 作为长期目录或类型名。
- 不为新含义复用旧名字；含义变了就新建名字并迁移引用。
- 不混用同义词（`chat` 与 `conversation` 只能选一个，见词汇表）。
- 不创建无归属的全局目录（例如再开一个 `services/` 装各类逻辑）。

## 检查

- 新文件名能看出层次与职责吗？
- 目录有明确所有权，而不是临时收纳吗？
- 名称是否在上方词汇表内？表外的新概念应先补进词汇表。
- 规则文件是否已在根 `AGENTS.md` 索引登记？
