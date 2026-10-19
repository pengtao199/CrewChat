# Python 编排层契约（v1，已冻结）

> **状态（2026-10-19）**：本层**冻结为参考，不参与运行时**。产品运行时是 `codeg/`（核心）
> 与 `AgentOS/`（壳）。本文件是历史契约存档，路径按移动后的布局写：
> `archive/python-orchestrator/{src,tests,scripts}`。
> 活的分层规则在 `AGENTS/architecture.md`。

## 分层契约

**目录即契约**：顶层目录决定层，依赖只能向下，任何向上依赖都是违反。

| 层 | 目录 | 文件 | 说明 |
| --- | --- | --- | --- |
| **L0** 基础 | `src/crewchat/core/` | `config` `models` | 纯数据与配置，不知道任何实现 |
| **L1** 基础设施 | `src/crewchat/core/` | `providers` `projects` `diagnostics` `cli_overrides` | 大脑池、项目、机器探测、CLI 参数覆盖 |
| **L2** 执行 | `src/crewchat/executors/` | `base`、`acp_body` | 不知道编排与会话 |
| **L3** 编排 | `src/crewchat/orchestration/` | `session`、`group` | 会话运行；群聊的共享上下文在 `group.py` |
| **L4** 契约 | `src/crewchat/api/` | 见下 | 只做请求解析与形状转换 |

L0 与 L1 共用 `core/`（都是「不执行、不通讯」的底层）；层判定按文件：`config`/`models` 属 L0，其余属 L1。

### 强制检查

```bash
python3 scripts/check_layers.py     # 依赖方向（层由顶层目录推导）
python3 scripts/check_imports.py    # 相对导入可解析（跳过 archive/）
```

顶层目录必须是 `core` / `executors` / `orchestration` / `api` 之一，否则检查以非零码退出。

### 实际结构

```
src/crewchat/
├── core/                   L0 + L1
│   ├── config.py               设置（环境变量 / .env）
│   ├── models.py               AgentSpec、resolve_cwd
│   ├── providers.py            大脑池 + ~/.agents 约定 + resolve_brain
│   ├── projects.py             项目/工作区（agent 全局，不归项目）
│   ├── diagnostics.py          CLI 与端点的探测
│   └── cli_overrides.py        手改的 CLI 参数覆盖
├── executors/              L2 身体
│   ├── base.py                 Executor 契约与注册表
│   ├── acp_body.py             全部 CLI 身体（ACP）
│   └── archive/                bespoke 适配器（claude/codex/opencode），已退役
├── orchestration/          L3
│   ├── session.py              Session、stream_ask、session_for
│   └── group.py                群聊共享上下文、显式委派与规划
└── api/                    L4
    ├── app.py                  应用装配、501 兜底
    ├── auth.py                 本地单用户鉴权
    ├── hub.py                  WS 广播中心（message.stream 等帧）
    ├── chat.py                 发送/取消/流式转发
    ├── conversations.py        会话与消息
    ├── store.py                SQLite（偏好/会话/消息）
    ├── engines_api.py          上游契约：CLI 身体/引擎清单
    ├── agent_catalog_api.py    上游契约：Agent 目录投影
    ├── providers_api.py        上游契约：模型端点
    ├── skills_api.py           上游契约：技能
    ├── mcp_api.py              上游契约：MCP 服务
    ├── fs_api.py               上游契约：文件/目录浏览与预览
    ├── crewchat_agents.py      自研 /api/v1/crewchat/agents
    ├── crewchat_projects.py    自研 /api/v1/crewchat/projects（项目=工作区 CRUD）
    └── crewchat_usage.py       自研 /api/v1/crewchat/usage（身体用量聚合）
```

## crewai 的定位

**只借它的数据模型，不用它的执行引擎。**

| 场景 | 实现 | 理由 |
| --- | --- | --- |
| Agent / Task 形状 | `core/models.py` 复用 crewai 的字段命名（role/goal/backstory、Task/context） | 同一份 agent 文件既能被 `crewai run` 读，也能被我们读 |
| 多 Agent 协作 | **自建** `orchestration/group.py` | 见下 |
| 配置解析 | `crewai.project.json_loader.load_jsonc_file` | JSONC 带注释与尾逗号，标准库不支持 |

**为什么协作不用 crewai 的执行引擎**（实测记录在 `docs/plans/archive/multi-agent-collaboration.md`）：

- `crewai.LLM(model='claude')` 不报错但**静默落到 OpenAI**（401）——它只能驱动 chat-completions provider。
- `crewai.flow.conversational` 一次只路由到**一个**响应者，与「多个 Agent 各出一份产出」不符。
- crewai 自带执行循环，会与 CLI 自身的工具循环叠加。

## 归属判定

- **聚合视图归契约层**：每个端点需要什么形状就自己算，不硬抽公共「健康」层。
- **契约层文件名**：对接上游 `/api/*` 用 `<domain>_api.py`；自研 `/api/v1/crewchat/*` 用 `crewchat_<domain>.py`。
- **新的跨层需求先问**：能否下移到更低层？下移不了再考虑放 L3。

## 原则与规则

- **业务逻辑只在后端**：前端拿到的就是可以渲染的结果。
- **契约层薄**：只做「解析请求 → 调后端能力 → 转成 UI 期望的形状」。
- **执行器不感知编排**：只接收 `ExecRequest` 返回 `ExecResult`。
- **单一模型定义**：Agent 形状只在 `models.py` 定义一次。
- 配置读取统一走 `config.get_settings()`；禁止模块级直接读环境变量。
- 新增执行器只需在 `executors/` 加一个文件并 `register`。

## 数据流

```
UI 发送
  └─ POST /api/v1/conversations/{id}/messages
       └─ api/chat.py 建后台任务，立即返回 {msg_id, turn_id, runtime}
            └─ orchestration.Session.stream_ask（单 Agent）或 group_turn（群聊）
                 └─ executors/<body>.run → CLI（各自登录 / 用户配置）
                      └─ ExecEvent 流
                           └─ api/hub.py 广播 message.stream
                                └─ UI 累积渲染
```

## 禁止项

- 不在 `api/` 里写模型推理、文件写入、Agent 装配等业务逻辑。
- 不在 `executors/` 里读写会话状态或项目配置。
- 不让前端承担「是否可用 / 是否已安装」的判断。
- 不新建第五层；确需新层时先更新本文件。
