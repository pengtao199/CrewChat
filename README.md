# CrewChat

多 Agent 协作桌面应用。用户在一个会话里下达任务，**多个专精 Agent 协作完成** —— 每个 Agent
有自己的模型、CLI 身体、Skill 与 MCP，并且互相能看到、接续彼此的产出。

## 运行时

本机五个独立部分：

| 层 | 路径 | 职责 |
| --- | --- | --- |
| macOS 壳 | `AgentOS/` | SwiftUI 客户端：会话、Agents、设置、流式事件与审批；只渲染与交互 |
| 核心 | `cli-layer/` | Rust：ACP 宿主、会话、权限、花名册、引擎管理、持久化 |
| 模型层 | `model-layer/` | Rust：供应商 / 路由 / 密钥 / 额度；本地网关 `:3456` + 控制面 `:3458` |
| 技能层 | `skill-layer/` | Rust：全局技能库、版本、安装/更新、会话 materialize（`:3460`） |
| MCP 层 | `mcp-layer/` | Rust：MCP 注册表、每 Agent 一个本地 proxy 实例（`:3461`） |

`codeg/`、`ccr/` 与 v1 Python 编排层均已移除/归档，不再参与构建或运行。

一个会话里可以有多名 Agent：

- **每个 Agent 是一个「人」** —— 有自己的**大脑**（模型端点）、**身体**（CLI：`claude` / `codex` /
  `opencode` / `gemini` / `hermes`）、**能力**（Skill / MCP / tools）与**人格**（role / goal / backstory）。
- **他们在同一个会话里协作** —— 后来者能看到先行者的产出并接着干。
- **全过程可见** —— 谁在发言、调用了什么工具、花了多少 token 与成本。

### 核心概念

| 概念 | 定义 | 真相源 |
| --- | --- | --- |
| **Agent** | 大脑 + 身体 + 能力 + 人格；全局花名册，不归任何项目 | `cli-layer.db` → `agent_roles` |
| **Conversation** | 用户与若干 Agent 的一条工作线程 | `cli-layer.db` → `conversations` |
| **Project** | 一个文件夹；决定工作目录与会话归属 | `cli-layer.db` → `folders` |
| **Turn** | 用户一次输入触发的一轮协作 | `cli-layer.db` → `turns` |
| **Skill** | 领域知识包 | `~/.agents/skills` + `skill-layer` registry |
| **MCP** | 外部工具服务 | `~/.agents/mcp` + 每 Agent proxy |

概念细节见 `AGENTS/agent-model.md`，数据落点见 `AGENTS/truth-sources.md`。

## 构建

```sh
# 五个 Rust sidecar
cd cli-layer && cargo build
cd model-layer && cargo build
cd skill-layer && cargo build
cd mcp-layer && cargo build

# macOS 壳
cd AgentOS
xcodebuild -project AgentOS.xcodeproj -scheme AgentOS -configuration Debug -sdk macosx build
```

开发时 Xcode scheme 通过 `AGENTOS_CLI_LAYER_BINARY`、`AGENTOS_MODEL_BINARY`、
`AGENTOS_SKILL_BINARY`、`AGENTOS_MCP_BINARY` 指向各 crate 的 debug 产物；打包时把
`crewchat-cli`、`crewchat-model`、`crewchat-skill`、`crewchat-mcp` 放进
`AgentOS.app/Contents/MacOS/`。

## 测试

```sh
cd cli-layer && cargo test
cd model-layer && cargo test
cd skill-layer && cargo test
cd mcp-layer && cargo test
```

真实 CLI 会话、MCP 注入和打包验收见 `AGENTS/verification.md`。

## 规则

- 数据落点：`AGENTS/truth-sources.md`
- 分层与改动去向：`AGENTS/architecture.md`
- Agent 领域模型：`AGENTS/agent-model.md`
- 模型接入：`AGENTS/gateway.md`
- 壳边界：`AGENTS/agentos-boundary.md`
- AgentOS 对核心接口的唯一清单：`AgentOS/engine-api.md`
