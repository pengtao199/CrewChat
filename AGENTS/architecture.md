# 分层与架构

## 适用范围

新增目录或模块、调整分层、判断代码该放 Python 侧还是 UI 侧、改动跨层调用时。

## 目标

保持三层边界清晰：Python 后端拥有全部业务逻辑，UI 只做展示与交互，网关只做协议归一化。

## 三层结构

| 层 | 路径 | 职责 | 禁止 |
| --- | --- | --- | --- |
| 前端外壳 | `ui/` | 渲染、交互、本地视图状态 | 业务规则、直接调用 CLI、持有密钥 |
| 后端 | `src/crewchat/` | 领域模型、编排、执行器、持久化、HTTP/WS | 渲染逻辑、依赖前端实现细节 |
| 网关 | `gateway/` | 协议归一化（Responses / Chat / Messages 互转） | 业务逻辑、Agent 概念 |

## 分层契约

**目录即契约**：顶层目录决定层，依赖只能向下，任何向上依赖都是违反。

| 层 | 目录 | 文件 | 说明 |
| --- | --- | --- | --- |
| **L0** 基础 | `core/` | `config` `models` | 纯数据与配置，不知道任何实现 |
| **L1** 基础设施 | `core/` | `providers` `projects` `diagnostics` `cli_overrides` `gateway` | 大脑池、项目、机器探测、模型网关接入 |
| **L2** 执行 | `executors/` | `base`、`llm_body`、`acp_body` | 不知道编排与会话 |
| **L3** 编排 | `orchestration/` | `session`、`group` | 会话运行；群聊的共享上下文在 `group.py` |
| **L4** 契约 | `api/` | 见下 | 只做请求解析与形状转换 |

L0 与 L1 共用 `core/`（都是"不执行、不通讯"的底层）；层判定按文件：`config`/`models` 属 L0，其余属 L1。

### 强制检查

```bash
uv run python scripts/check_layers.py     # 依赖方向（层由顶层目录推导）
uv run python scripts/check_imports.py    # 相对导入可解析
```

顶层目录必须是 `core` / `executors` / `orchestration` / `api` 之一，否则检查以非零码退出。移动文件后运行 `check_imports`，避免 `ModuleNotFoundError` 只在启动时暴露。

### 实际结构

```
src/crewchat/
├── core/                   L0 + L1
│   ├── config.py               设置（环境变量 / .env）
│   ├── models.py               AgentSpec、resolve_cwd
│   ├── providers.py            大脑池 + ~/.agents 约定 + resolve_brain
│   ├── projects.py             项目/工作区（agent 全局，不归项目）
│   ├── diagnostics.py          CLI 与端点的探测
│   ├── cli_overrides.py        手改的 CLI 参数覆盖
│   ├── gateway.py              模型网关端口 + 工厂（ModelGateway / get_gateway）
│   ├── gateway_ccswitch.py     cc-switch 网关适配器（监督/控制/投影/读）
│   ├── engines.py              引擎管理端口 + 工厂（EngineTools / get_engine_tools）
│   └── engines_ccswitch.py     cc-switch 引擎工具适配器（扫描/安装/更新）
├── executors/              L2 身体
│   ├── base.py                 Executor 契约与注册表
│   ├── llm_body.py             openai SDK，直连大脑池
│   └── acp_body.py             全部 CLI 身体（ACP）
├── orchestration/          L3
│   ├── session.py              Session、stream_ask、session_for
│   └── group.py                群聊共享上下文、显式委派与规划
└── api/                    L4
    ├── app.py                  应用装配、静态托管、501 兜底
    ├── auth.py                 本地单用户鉴权
    ├── hub.py                  WS 广播中心（message.stream 等帧）
    ├── chat.py                 发送/取消/流式转发
    ├── conversations.py        会话与消息
    ├── store.py                SQLite（偏好/会话/消息）
    ├── engines_api.py          上游契约：CLI 身体/引擎清单（/api/engines/management、custom、refresh）
    ├── agent_catalog_api.py    上游契约：Agent 目录投影（/api/agents、/{id}、import）
    ├── providers_api.py        上游契约：模型端点
    ├── skills_api.py           上游契约：技能
    ├── mcp_api.py              上游契约：MCP 服务
    ├── fs_api.py               上游契约：文件/目录浏览与预览（/api/fs/*）
    ├── crewchat_agents.py      自研命名空间 /api/v1/crewchat/agents
    ├── crewchat_gateway.py     自研命名空间 /api/v1/crewchat/gateway（网关状态与控制）
    ├── crewchat_projects.py    自研命名空间 /api/v1/crewchat/projects（项目=工作区 CRUD）
    ├── crewchat_engines.py     自研命名空间 /api/v1/crewchat/engines（引擎扫描/安装/更新）
    └── crewchat_usage.py       自研命名空间 /api/v1/crewchat/usage（身体用量聚合）
```

## crewai 的定位

**只借它的数据模型，不用它的执行引擎。**

| 场景 | 实现 | 理由 |
| --- | --- | --- |
| Agent / Task 形状 | `core/models.py` 复用 crewai 的字段命名（role/goal/backstory、Task/context） | 同一份 agent 文件既能被 `crewai run` 读，也能被我们读；加字段不用教两套 schema |
| 多 Agent 协作 | **自建** `orchestration/group.py`（共享上下文 + 路由 + 委派 + 确定性循环） | 见下 |
| 配置解析 | `crewai.project.json_loader.load_jsonc_file` | JSONC 带注释与尾逗号，标准库不支持 |

**为什么协作不用 crewai 的执行引擎**（实测，记录在 `docs/plans/archive/multi-agent-collaboration.md`）：

- `crewai.LLM(model='claude')` 不报错但**静默落到 OpenAI**（401）—— 它只能驱动 chat-completions
  provider，驱动不了我们的 CLI 身体。
- `crewai.flow.conversational` 一次只路由到**一个**响应者，与「多个 Agent 各出一份产出」不符。
- crewai 自带执行循环，会与 CLI 自身的工具循环叠加；而数据分辨率（delta / 工具入参出参 / usage）
  只存在于**我们自己的循环**里。

因此 `crewai` 出现在依赖里是**有意为之**，但只用于数据模型与 JSONC 解析。新增对 crewai 其他能力的
使用前，先确认它不能被更直接的实现替代。

### 归属判定

- **聚合视图归契约层**：每个端点需要什么形状就自己算（`engines_api` 按身体聚合、`agent_catalog_api` 按 Agent 聚合）。它们的上游契约本来就不同，不硬抽公共"健康"层。
- **契约层文件名**：对接上游 `/api/*` 的用 `<domain>_api.py`；自研 `/api/v1/crewchat/*` 的用 `crewchat_<domain>.py`；其余（app/auth/chat/conversations/hub/store）是自研实现。
- **新的跨层需求先问**：能否下移到更低层？下移不了再考虑放 L3。


## 原则

- **业务逻辑只在后端**：前端不做判断，拿到的就是可以渲染的结果（例如"是否可用"由后端算好）。
- **契约层薄**：`api/` 里的路由只做「解析请求 → 调后端能力 → 转成 UI 期望的形状」，不写领域规则。
- **执行器不感知编排**：`executors/` 不知道谁是群聊、谁在哪个项目，只接收 `ExecRequest` 返回 `ExecResult`。
- **单一模型定义**：Agent 的形状只在 `models.py` 定义一次，其他层引用它。

## 具体规则

- 新增能力先问「属于哪一层」，再问「有没有现成的」。跨层直接调用（前端 → 执行器）一律禁止。
- 前端通过 `/api/v1/*` 与 `/ws` 访问后端；后端不感知前端路由。
- `api/` 中出现的上游用语（`backend`）必须集中在一个文件里翻译，不扩散到 `orchestrator` 或 `executors`。
- 新增执行器只需在 `executors/` 加一个文件并 `register`，不改编排层。
- 配置读取统一走 `config.get_settings()`；禁止模块级直接读环境变量。

## 数据流

```
UI 发送
  └─ POST /api/v1/conversations/{id}/messages
       └─ api/chat.py 建后台任务，立即返回 {msg_id, turn_id, runtime}
            └─ orchestrator.Session.stream_ask（单 Agent）或 group_turn（群聊）
                 └─ executors/<body>.run → CLI 或网关
                      └─ ExecEvent 流
                           └─ api/hub.py 广播 message.stream
                                └─ UI 累积渲染
```

## 文件系统陷阱

- **`AGENTS/` 与 `agents/` 在 macOS 上同一个目录**（默认大小写不敏感）。规则目录用 `AGENTS/`；Agent 定义是全局花名册，放在数据目录 `.data/agents/`（`settings.agents_home`）。
- 新增任何目录名前，确认它与既有目录仅大小写不同 —— 那会静默合并，且 `git` 会把两边内容混在一起。
- 类似风险：`UI/` 与 `ui/`、`Docs/` 与 `docs/`。

## 禁止项

- 不在 `api/` 里写模型推理、文件写入、Agent 装配等业务逻辑。
- 不在 `executors/` 里读写会话状态或项目配置。
- 不让前端承担"是否可用/是否已安装"的判断（那是 `diagnostics` 的职责）。
- 不新建第四层；确需新层时先更新本文件。
- 不在仓库根新建与 `AGENTS/`、`ui/`、`config/` 仅大小写不同的目录。
- **无认证、仅本机**：本应用作为本地桌面 app（`Settings.host=127.0.0.1`）运行，**没有登录**，每个请求都是唯一的本地用户。禁止把它暴露到 LAN / 公网 —— 那等于把本机的 CLI / Agent 交给任何人。

## 检查

- 这个新文件属于哪一层？该层的「禁止」列有没有被违反？
- 删掉前端后，后端的这套能力是否仍然完整可用？
- 换一个身体或大脑，是否只影响 `executors/` 或 `providers.py`？
