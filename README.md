# CrewChat

多 Agent 协作桌面应用。用户在一个会话里下达任务，**多个专精 Agent 协作完成** —— 每个 Agent
有自己的模型、CLI 身体、Skill 与 MCP，并且互相能看到、接续彼此的产出。

## 产品定义

### 是什么

一个运行在本机的桌面应用（Codeg 工作台 + Python 后端 + 本地模型网关）。一个会话里可以有多名 Agent：

- **每个 Agent 是一个「人」** —— 有自己的**大脑**（模型端点）、**身体**（CLI：`claude` / `codex` /
  `opencode` / `直连 LLM`）、**能力**（Skill / MCP / tools）与**人格**（role / goal / backstory）。
- **他们在同一个会话里协作** —— 不是把同一条消息群发给几个人，而是**后来者能看到先行者的产出并接着干**。
- **全过程可见** —— 谁在发言、调用了什么工具（含入参与出参）、花了多少 token 与成本。

### 与「并排开几个聊天窗口」的区别

| | 并排多窗口 | CrewChat |
| --- | --- | --- |
| 上下文 | 各自独立，互不可见 | 共享同一工作区与会话上下文 |
| 交接 | 人肉复制粘贴 | Agent 之间直接接续产出 |
| 身份 | 无 | 每个 Agent 独立身份、模型、工具、用量 |
| 任务 | 各问各的 | 面向同一个用户任务分解与收敛 |

### 核心概念

| 概念 | 定义 | 真相源 |
| --- | --- | --- |
| **Agent** | 大脑 + 身体 + 能力 + 人格；全局花名册，不归任何项目。预置只有助手（改配置）和管理员（群聊 Leader），其余是通用角色 | `.data/agents/*.jsonc` |
| **Conversation** | 用户与若干 Agent 的一条工作线程 | `.data/crewchat.db` |
| **Project** | 一个文件夹；决定工作目录与会话归属 | `.data/projects.jsonc` |
| **Workspace** | Agent 实际读写的目录（项目目录，或未绑项目的沙盒） | `.data/workspaces/` |
| **Turn** | 用户一次输入触发的一轮协作 | 内存 + 消息表 |
| 大脑 brain | 模型端点（供应商 / 模型 id） | `.data/providers.jsonc` |
| 身体 body | 实际执行者：一个 CLI，或直连 LLM | `executors/` |

概念细节见 `AGENTS/agent-model.md`，数据落点见 `AGENTS/truth-sources.md`。

## 组合原则：借用，不重造，也不托管

**能借用的成熟开源直接借用并改造；但不把核心的执行循环与真相源交给第三方。**

判定标准一句话：**借「协议形状」和「能力」，不借「执行循环」和「真相源」。**

| 组成 | 借用对象 | 借用方式 | 明确不借的部分 |
| --- | --- | --- | --- |
| 工作台（协作界面） | [Codeg](https://github.com/spacering-net/codeg)（Apache-2.0） | 源码固化进 `codeg/` 后二次开发；借 ACP 会话、`@` 委派与工作区形态 | 它的模型管理（归 CCR） |
| 模型统一管理 | [CCR](https://github.com/musistudio/claude-code-router)（MIT） | 源码固化进 `ccr/`；本地网关 + Agent profiles | —— |
| 网关 / 模型路由 | [cc-switch](https://github.com/farion1231/cc-switch)（MIT, Rust） | 编译成独立进程 `ccsw-proxy` 直接使用 | —— |
| Agent / Task 数据模型 | [crewai](https://github.com/crewAIInc/crewAI) | **只借 Pydantic schema**（role/goal/backstory、Task/context） | 它的执行引擎（驱动不了 CLI） |
| 协作协议形状 | [awslabs/cli-agent-orchestrator](https://github.com/awslabs/cli-agent-orchestrator)、AionUi Team | 借鉴 `handoff` / `assign` / `send_message` 语义 | 它的 tmux runtime 与终端级状态 |
| 执行循环 + 执行器 | **自建** | —— | —— |

**为什么执行循环必须自建**：数据分辨率就在循环里。

- cc-switch 只借协议归一化，业务与 Agent 概念不进网关（见 `AGENTS/architecture.md`）。
- crewai 的 `LLM(model=...)` 只能驱动 chat-completions provider，传 `claude` 会**静默落到 OpenAI**；
  且它自带执行循环，会和 CLI 自身的工具循环打架。所以只借它的数据模型。
- CAO 只有终端级状态（`idle` / `processing` / `completed`），拿不到 per-token delta、工具入参出参、
  token 与成本；且自带第二套 DB 与 session 模型。所以只借它的调度语义。
- 我们的 `executors/` 直接经 SDK 拿结构化事件，是**模型级分辨率**的来源；循环在自建代码里，
  才能随时显示「谁在说」、可取消、可持久化。

## 分层

| 层 | 路径 | 职责 |
| --- | --- | --- |
| 工作台 | `codeg/` | 渲染与交互（Codeg 二次开发基座，Apache-2.0）；ACP 会话与 `@` 委派 |
| 后端 | `src/crewchat/` | 领域模型、编排、执行器、持久化、HTTP/WS |
| 模型层 | `ccr/` | 统一模型管理（CCR 基座，MIT）：本地网关、路由、日志 |
| 网关 | `gateway/cc-switch/` | cc-switch Rust 核心（headless `ccsw-proxy`），协议归一化 |

后端为 `core`（L0 基础 / L1 基础设施）→ `executors`（L2 身体）→ `orchestration`（L3）→
`api`（L4 契约），依赖只向下。详见 `AGENTS/architecture.md`。

## 明确不做（非目标）

- **不把编排托管给第三方**：不跑 CAO 的 `cao-server`，不用 crewai 的执行引擎。
- **不用终端抓屏**（tmux / PTY）驱动 Agent —— 那会丢掉模型级事件。
- **不重复实现**已有成熟件：工作台、模型管理与网关都不自研（见各基座的 `NOTICE.md`）。
- **不做登录与联网暴露**：本应用仅本机（`127.0.0.1`）、无认证、单用户。

## 快速开始

```bash
uv sync                       # 安装后端依赖
uv run crewchat --no-browser  # 启动后端（默认 http://127.0.0.1:8000）
```

协作工作台（`codeg/` 二次开发，桌面 / 服务器一体）：

```bash
cd codeg
npx -y pnpm@11.9.0 install    # pnpm 11（packageManager 锁定）
pnpm tauri dev                # 桌面开发运行；或 `pnpm dev` 只起前端
```

模型统一管理（`ccr/` 二次开发）：

```bash
cd ccr && npm ci              # 桌面构建：npm run build:app:mac
# 或直接用：npm install -g @musistudio/claude-code-router && ccr ui
```

各基座的构建、取舍与升级方式见 `gateway/cc-switch/NOTICE.md`、`codeg/NOTICE.md`、`ccr/NOTICE.md`。

## 自检

```bash
uv run python scripts/check_layers.py    # 依赖方向（分层契约）
uv run python scripts/check_imports.py   # 相对导入可解析
uv run pytest -q                         # 后端测试
```

## 数据与规则

- 数据落点（真相源）见 `AGENTS/truth-sources.md`；`.data/` 为本地私有状态，不入库。
- 改动前请遵循 `AGENTS.md` 的执行入口与子规则索引。
- 进行中的方案见 `docs/plans/`。
