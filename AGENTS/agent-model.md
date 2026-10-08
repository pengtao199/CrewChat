# 领域模型

## 适用范围

涉及 Agent、大脑（模型端点）、引擎（执行能力）、工具、技能、MCP、记忆、规则，以及会话与 Agent 的绑定关系时。

## 目标

让「Agent 引用三类可复用资源」这件事在代码、配置、界面三处表达一致，避免把引擎、工具、技能混进 Agent 定义里。

## 一、三类资源（可被引用，不由 Agent 拥有）

### 引擎 Engine —— 执行能力

| 类型 | 例子 | 谁提供 | 用户要做什么 |
| --- | --- | --- | --- |
| 本地 CLI | codex / claude / opencode | 用户安装 | 装好并登录；app 只探测 |
| 远程 API | llm（经网关） | app 提供通道 | 只需配模型端点 |
| 应用内置 | 助手引擎 | app 随包自带 | 什么都不用做 |

代码里引擎叫 `body`（大脑的执行体），用户可见名是「引擎」。`AgentSpec.body` 引用引擎 id。

### 工具 Tool —— 可被调用的动作

| 类型 | 例子 | 谁提供 |
| --- | --- | --- |
| 引擎自带 | CLI 的沙箱、命令行、文件读写 | 随引擎，不单独管理 |
| MCP 服务 | playwright / xcodebuild … | 用户配置 |
| 应用内置 | 诊断、读写 Agent、列技能 | app 随包自带 |

### 技能 Skill —— 领域知识包

| 类型 | 位置 | 来源标记 |
| --- | --- | --- |
| 应用内置 | 仓库 `skills/` | `source: 'builtin'` |
| 共享 | `~/.agents/skills` | `source: 'custom'` |

## 二、用户产物：Agent

```
Agent = 大脑 + 引擎 + 技能 + 工具(MCP) + 记忆 + 规则
```

| 组成 | 字段 | 说明 |
| --- | --- | --- |
| 大脑 | `brain` | 引用大脑池 id；可写 `provider/model` 覆盖模型 |
| 引擎 | `body` | 引用引擎 id |
| 技能 | `skills` | 按名字引用技能 |
| 工具 | `mcps` | 按名字引用 MCP 服务 |
| 记忆 | `memory` | 跨会话记住自己的产出 |
| 规则 | `role` / `goal` / `backstory` | 拼接为提示词前缀 |

**Agent 只引用资源，不定义资源。** 新增一个引擎/工具/技能不该改任何 Agent 文件。

## 三、助手 Assistant

**助手不是第四种概念，它是预置的 Agent。**

- 引擎：应用内置（操作本应用）
- 工具：应用内置（诊断、读写 Agent、列技能…）
- 技能：应用内置
- 存在形式：编排页的一个模板；用户创建后就是普通 Agent 文件，可改可删

因此代码里没有 "Assistant" 类型。上游契约里的 `assistant` 是他们的叫法，对应我们的 Agent，只在 `api/assistants_api.py` 内做翻译。

## 四、原则

- **大脑与引擎解耦**：任意大脑可驱动任意引擎，经网关归一化。禁止在引擎实现里写死模型来源。
- **Agent 是独立实体**：不属于任何项目，可被多个项目引用。
- **文件是真相源**：Agent 定义存放 `config/agents/*.jsonc`，界面是编辑器而非存储。
- **CLI 引擎自带大脑是默认**：`codex` / `claude` / `opencode` 不显式配 `brain` 时用各自 CLI 的默认模型；显式配了就用配的。
- **纯模型引擎必须配大脑**：`body: "llm"` 没有内置大脑，缺 `brain` 时该 Agent 不可用。
- **工具是能力而不是身份**：给 Agent 加工具不改变它是谁。

## 五、具体规则

- Agent 字段名与 `crewai.Agent` 保持一致（`role` / `goal` / `backstory` / `skills` / `mcps` / `memory`），同一份文件既能被 CrewChat 加载，也能被 crewai 原生加载。
- 一个会话绑定一个或多个 Agent（`agent_ids`）；单 Agent 时是单聊，多 Agent 时是群聊。
- 会话的模型默认取自 Agent 的大脑；会话级覆盖是例外。
- 新增 Agent 组成维度时，必须同步四处：`core/models.py` 的 `AgentSpec`、读写接口、编排页表单、本文件。

## 六、命名对照

| 概念 | 代码 | 用户可见 | 禁止叫法 |
| --- | --- | --- | --- |
| 引擎 | `body` / `Executor` | 引擎 | backend（上游用语，仅限契约层）、agent、runner |
| Agent | `AgentSpec` | Agent | assistant（上游用语，仅限契约层）、worker、bot |
| 助手 | 预置 Agent 模板 + `builtin` 引擎 | 助手 | Assistant 类型（不存在） |
| 大脑 | `brain` / `Provider` | 模型端点 | model（那是大脑的一部分）、api |
| 技能 | `skills` | 技能 | capability、plugin（plugin 是安装包） |
| 工具 | `mcps` / 应用内置工具 | 工具 | — |

## 七、禁止项

- 不在 Agent 定义里内联引擎/工具/技能的具体实现；只写引用。
- 不新增与「引擎」平行的概念（例如 `BackendKind`、`Runtime`）。
- 不为「助手」单开类型、单开存储或单开接口；它就是 Agent。
- 不让界面暴露上游的 `assistant` / `backend` / `acp` 用语给用户。
- 不把「内置」理解成「用户装不了」：内置只是随包提供，用户可删可改（技能除外，技能是只读资源）。

## 八、检查

- 新增字段属于「Agent 的六个组成」之一，还是属于三类资源？放对了吗？
- 换掉引擎后，Agent 的其他部分是否无需改动即可继续工作？
- 同一个 Agent 是否能在两个不同项目里被引用？
- 界面文案、配置文件、代码类型三处的叫法是否一致（见命名对照表）？
- 这次改动是否需要动 Agent 文件？如果不需要，说明资源层解耦是对的。
