# 对核心的调用登记表

AgentOS 调用核心的**唯一清单**。新增调用必须在同一次改动里登记；未登记的调用视为越界（见 `AGENTS/agentos-boundary.md`）。

## 契约事实

| 项 | 值 |
| --- | --- |
| Base | `http://127.0.0.1:<port>` |
| 命令 | `POST /api/<command>`，命令名 = Tauri command 名（snake_case） |
| 事件 | `GET /ws/events`，attach 协议（`snapshot` / `replay` / `event` / `detached` / `pong`） |
| 鉴权 | HTTP `Authorization: Bearer <token>`；WS `Sec-WebSocket-Protocol: codeg-events, codeg-token.<base64url(token)>` |
| 错误 | `{ code(snake_case), message, detail?, i18n_key?, i18n_params? }` |

## 已登记命令

| 命令 | 用途 | 端口 | 注意点 |
| --- | --- | --- | --- |
| `health` | 核心存活与版本探测；启动与重连前判活 | `CoreLifecycle` | `POST /api/health`，返回 `{"status":"ok","version":"0.35.1"}`；同样需要 token。壳以 200 作为「已连接」的判据，并显示 `version`（2026-10-19 实测：`127.0.0.1:3080` 返回 200） |
| `get_delegation_settings` | 读委派开关与深度 | `CoreSettings` | 无参数；核心默认 `enabled=false`（2026-10-19 实测） |
| `set_delegation_settings` | 写委派设置（整对象） | `CoreSettings` | body `{settings:{enabled,depth_limit,agent_defaults,completed_cache_max_mb}}`；壳做读-改-写，只翻 `enabled`，其余原样回传（实测：写入后核心 `app_metadata` 四键齐全，`depth_limit` 未被改动） |

| `list_model_providers` | 供应商列表 | `CoreModels` | 无参数；返回 `api_key`（全量）与 `api_key_masked`。壳只用 masked 展示，编辑时必须显式输入新 key |
| `create_model_provider` | 新建供应商 | `CoreModels` | body `{name, apiUrl, apiKey, agentType, model?}`（camelCase）。`model` 语义随引擎：claude_code 必须是 JSON 对象、codex 可 JSON 或模型名、其余为模型名；留空=用引擎默认 |
| `update_model_provider` | 更新供应商 | `CoreModels` | body `{id, name, apiUrl, apiKey?, agentType, model?}`；`apiKey` 省略=不改，`model` 省略=不改、`""`=清空。返回 `{provider, affectedRunningSessions}`，壳忽略后重拉列表 |
| `delete_model_provider` | 删除供应商 | `CoreModels` | body `{id}`；返回 `null` |
| `agent_roles_list` | 花名册列表 | `CoreRoster` | 无参数；首启会带出内置角色（实测 8 个）。**需要较新的核心二进制**：本机已装 `.data/codeg/bin/codeg-server` 缺这条路由（501），从源码构建立即正常；开发期指向 `codeg/src-tauri/target/debug/codeg-server`（scheme 已改） |
| `agent_roles_save` | 新建 / 更新角色 | `CoreRoster` | body `{role:{id,name,soul,rules,agent_type,model,model_provider,skills,mcp,permission_mode,auto_approve}}`（snake_case）；`id` 为空=新建，返回带 id 的角色 |
| `agent_roles_delete` | 删除角色 | `CoreRoster` | body `{id}`；返回 bool |
| `acp_list_agents` | 引擎清单与就绪态 | `CoreEngines` | 无参数；返回 `agent_type/name/description/available/enabled/installed_version/distribution_type/is_acp_adapter/env`（snake_case） |
| `acp_update_agent_preferences` | 启用 / 禁用引擎（保留 env） | `CoreEngines` | body `{agentType, enabled, env}`（**camelCase**；漏写 agentType 会 400）。壳先取该引擎当前 `env` 原样回传，避免清空 |

## 已登记事件

| 事件 / 帧 | 用途 | 端口 | 注意点 |
| --- | --- | --- | --- |
| （待登记） | | | |

## 待接线（v1 顺序，接线时迁入上表）

| 顺序 | 命令 / 通道 | 用途 |
| --- | --- | --- |
| 1 | `list_all_conversations`、`get_conversation` | 会话列表与详情 |
| 2 | `acp_list_agents` | 引擎清单与就绪 |
| 3 | `acp_connect`、`acp_prompt`、`acp_cancel` | 连接与轮次 |
| 4 | `acp_get_session_snapshot_by_conversation`、`/ws/events` attach | 状态恢复与流式 |
| 5 | `acp_respond_permission` | 权限审批 |

## 登记格式

```text
| `命令名` | 一句话用途 | 所属端口 | 超时 / 幂等 / 409 之类的注意点 |
```

「所属端口」指 `Domain/Ports/` 里承载它的协议（起步三个：命令、事件流、进程生命周期）。

## 规则

- 一次改动里新增了调用却没更新本表 = 改动未完成。
- 只登记 AgentOS 实际调用的命令；核心有但壳不用的不进表。
- 本表是壳的接口面，不是核心的接口文档；不要搬运核心的全部路由。
