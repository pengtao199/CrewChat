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

## 后端内部层次

```
src/crewchat/
├── api/             HTTP/WS 契约层：只做协议适配与转发，不写业务
│   ├── app.py           应用装配与静态托管
│   ├── auth.py          本地单用户鉴权
│   ├── hub.py           WS 广播中心
│   ├── chat.py          发送/取消/流式转发
│   ├── conversations.py 会话与消息持久化接口
│   ├── agents_compose.py Agent 编排读写（我们的命名空间）
│   └── *_api.py         上游契约对接（providers / assistants / skills / mcp）
├── executors/       身体实现：每个 CLI 一个文件，统一 Executor 契约
├── orchestrator.py  会话运行：单 Agent 与群聊编排
├── agents.py        AgentSpec → crewai.Agent 工厂
├── models.py        AgentSpec 定义（唯一领域模型）
├── providers.py     大脑池读写 + ~/.agents 约定加载
├── projects.py      项目（工作区 + agent 目录）
├── diagnostics.py   身体/大脑健康探测
└── config.py        设置（环境变量与 .env）
```

## 原则

- **业务逻辑只在后端**：前端不做判断，拿到的就是可以渲染的结果（例如"是否可用"由后端算好）。
- **契约层薄**：`api/` 里的路由只做「解析请求 → 调后端能力 → 转成 UI 期望的形状」，不写领域规则。
- **执行器不感知编排**：`executors/` 不知道谁是群聊、谁在哪个项目，只接收 `ExecRequest` 返回 `ExecResult`。
- **单一模型定义**：Agent 的形状只在 `models.py` 定义一次，其他层引用它。

## 具体规则

- 新增能力先问「属于哪一层」，再问「有没有现成的」。跨层直接调用（前端 → 执行器）一律禁止。
- 前端通过 `/api/*` 与 `/ws` 访问后端；后端不感知前端路由。
- `api/` 中出现的上游用语（`assistant`、`backend`）必须集中在一个文件里翻译，不扩散到 `orchestrator` 或 `executors`。
- 新增执行器只需在 `executors/` 加一个文件并 `register`，不改编排层。
- 配置读取统一走 `config.get_settings()`；禁止模块级直接读环境变量。

## 数据流

```
UI 发送
  └─ POST /api/conversations/{id}/messages
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
