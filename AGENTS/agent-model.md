# 领域模型

## 适用范围

涉及 Agent、大脑（模型端点）、身体（CLI）、技能、MCP、记忆、规则，以及会话与 Agent 的绑定关系时。

## 目标

让"一个 Agent 是一个可复用的声明式组合体"这件事在代码、配置、界面三处表达一致。

## 核心等式

```
Agent = 大脑 + 身体 + 技能 + MCP + 记忆 + 规则
```

| 组成 | 含义 | 载体 |
| --- | --- | --- |
| 大脑 | 模型端点 + 具体模型 | `brain` 字段，引用大脑池 id；可写 `provider/model` 覆盖模型 |
| 身体 | 实际执行者 | `body` 字段：`llm` / `codex` / `claude` / `opencode` |
| 技能 | 可复用的领域知识 | `skills`，按名字引用 `~/.agents/skills` |
| MCP | 外部工具服务 | `mcps`，按名字引用 `~/.agents/mcp.json` |
| 记忆 | 跨会话记住自己的产出 | `memory` 开关 |
| 规则 | 角色 / 目标 / 背景 | `role` / `goal` / `backstory`，拼接后作为提示词前缀 |

## 原则

- **大脑与身体解耦**：任意大脑可驱动任意身体，经网关归一化。禁止在身体实现里写死模型来源。
- **Agent 是独立实体**：不属于任何项目，可被多个项目引用。禁止把 Agent 定义嵌进项目或会话。
- **文件是真相源**：Agent 定义存放在 `config/agents/*.jsonc`，界面是编辑器而非存储。禁止只写数据库不写文件。
- **CLI 身体自带大脑是默认，不是限制**：`codex` / `claude` / `opencode` 不显式配 `brain` 时使用各自 CLI 的默认模型；显式配了就用配的。
- **纯模型身体必须配大脑**：`body: "llm"` 没有内置大脑，缺 `brain` 时该 Agent 不可用。

## 具体规则

- Agent 的字段名与 `crewai.Agent` 保持一致（`role` / `goal` / `backstory` / `skills` / `mcps` / `memory`），同一份文件既能被 CrewChat 加载，也能被 crewai 原生加载。
- 一个会话绑定一个或多个 Agent（`agent_ids`）；单 Agent 时是单聊，多 Agent 时是群聊。
- 会话的模型默认取自 Agent 的大脑；会话级覆盖是例外，不是常规路径。
- 新增 Agent 组成维度（例如 plugins）时，必须同步四处：`models.py` 的 `AgentSpec`、读写接口、编排页表单、本规则文件。

## 命名对照

| 概念 | 代码中的名字 | 禁止叫法 |
| --- | --- | --- |
| 大脑 | `brain` / `Provider` | `model`（歧义，模型是大脑的一部分）、`api` |
| 身体 | `body` / `Executor` | `agent`（与 Agent 冲突）、`backend`（上游用语，仅限 UI 契约层） |
| Agent | `AgentSpec` | `Assistant`（上游用语）、`Worker` |
| 技能 | `skills` | `plugins`（语义不同，plugins 是安装包） |

## 禁止项

- 不用 `body` 字段表达模型选择，也不用 `brain` 字段表达执行方式。
- 不在会话里复制 Agent 的完整定义；会话只存 `agent_ids` 与快照性覆盖。
- 不为"临时"场景新造平行概念（例如 `QuickAgent`、`LiteAgent`）。
- 不让界面暴露上游的 `backend` / `assistant` 用语给用户，用户看到的是身体和 Agent。

## 检查

- 新增字段是否落在上表六个组成之一？不是则说明概念放错了。
- 换掉身体后，Agent 的其他部分是否无需改动即可继续工作？
- 同一个 Agent 是否能在两个不同项目里被引用？
- 界面文案、配置文件、代码类型三处的叫法是否一致？
