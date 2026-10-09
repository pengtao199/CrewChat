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
| **L2** 执行 | `executors/` | `base` + 每个身体一个文件 | 不知道编排与会话 |
| **L3** 编排 | `orchestration/` | `session`（群聊将新增 `group`） | 会话运行 |
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
│   ├── projects.py             项目（工作区 + agent 目录）
│   ├── diagnostics.py          CLI 与端点的探测
│   ├── cli_overrides.py        手改的 CLI 参数覆盖
│   ├── gateway.py              模型网关端口 + 工厂（ModelGateway / get_gateway）
│   ├── gateway_ccswitch.py     cc-switch 网关适配器（监督/控制/投影/读）
│   ├── engines.py              引擎管理端口 + 工厂（EngineTools / get_engine_tools）
│   └── engines_ccswitch.py     cc-switch 引擎工具适配器（扫描/安装/更新）
├── executors/              L2 身体
│   ├── base.py                 Executor 契约与注册表
│   ├── llm_body.py             openai SDK
│   ├── codex_body.py           openai-codex-sdk
│   ├── claude_body.py          claude-agent-sdk
│   └── opencode_body.py        官方 opencode CLI
├── orchestration/          L3
│   └── session.py              Session、stream_ask、session_for
└── api/                    L4
    ├── app.py                  应用装配、静态托管、501 兜底
    ├── auth.py                 本地单用户鉴权
    ├── hub.py                  WS 广播中心（message.stream 等帧）
    ├── chat.py                 发送/取消/流式转发
    ├── conversations.py        会话与消息
    ├── store.py                SQLite（偏好/会话/消息）
    ├── agents_api.py           上游契约：CLI 身体清单
    ├── assistants_api.py       上游契约：Assistant 投影
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

**单聊不走 crewai 的执行模型；群聊用 crewai 原生会话图。**

| 场景 | 实现 | 理由 |
| --- | --- | --- |
| 单 Agent 对话 | `orchestrator.stream_ask` 直连执行器 | 外层 LLM 对「把活交给 codex」是纯开销：身体自带大脑，再包一层 crewai Agent 只会多一次调用、多一层延迟 |
| 多 Agent 协作 | `crewai.flow.conversational`（router 选发言者 + 流式事件 + human feedback） | 这正是 crewai 擅长的地方：发言路由、多 agent 状态、中途问人 |
| 配置解析 | `crewai.project.json_loader.load_jsonc_file` | JSONC 带注释与尾逗号，标准库不支持 |

因此 `crewai` 出现在依赖里是**有意为之**，但只在上述两处使用。新增对 crewai 其他能力的使用前，先确认它不能被更直接的实现替代。

### 归属判定

- **聚合视图归契约层**：每个端点需要什么形状就自己算（`agents_api` 按身体聚合、`assistants_api` 按 Agent 聚合）。它们的上游契约本来就不同，不硬抽公共"健康"层。
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
- `api/` 中出现的上游用语（`assistant`、`backend`）必须集中在一个文件里翻译，不扩散到 `orchestrator` 或 `executors`。
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

- **`AGENTS/` 与 `agents/` 在 macOS 上同一个目录**（默认大小写不敏感）。规则目录用 `AGENTS/`，而 Agent 定义放在 `config/agents/`。
- 新增任何目录名前，确认它与既有目录仅大小写不同 —— 那会静默合并，且 `git` 会把两边内容混在一起。
- 类似风险：`UI/` 与 `ui/`、`Docs/` 与 `docs/`。

## 禁止项

- 不在 `api/` 里写模型推理、文件写入、Agent 装配等业务逻辑。
- 不在 `executors/` 里读写会话状态或项目配置。
- 不让前端承担"是否可用/是否已安装"的判断（那是 `diagnostics` 的职责）。
- 不新建第四层；确需新层时先更新本文件。
- 不在仓库根新建与 `AGENTS/`、`ui/`、`config/` 仅大小写不同的目录。

## 检查

- 这个新文件属于哪一层？该层的「禁止」列有没有被违反？
- 删掉前端后，后端的这套能力是否仍然完整可用？
- 换一个身体或大脑，是否只影响 `executors/` 或 `providers.py`？
