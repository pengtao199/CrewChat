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

| `ccr_health` | CCR 管理面探活 | `CoreCCR` | 无参数；返回 `{url, tokenConfigured, tokenSource}`。壳用它判断「CCR 是否可达」 |
| `ccr_call`（`getConfig`） | 供应商与凭据 | `CoreCCR` | body `{method:"getConfig", args:[]}`；返回 `{Providers:[{id,name,type,api_base_url,models,credentials:[{name,api_key,enabled}]}]}`。**这才是模型页的真相源**（`list_model_providers` 是 codeg 的旧表，页面已不用） |
| `ccr_call`（`getProviderAccountSnapshots`） | 每个账号的额度 | `CoreCCR` | args `["<provider name>", {}]`（或 `{"forceRefresh":true}`）；返回 `[{credentialLabel,status,meters:[{label,remaining,limit,used,unit,window,resetAt}]}]` |
| `ccr_supervisor_start` | 拉起 / 接管 CCR | `CoreCCR` | 无参数；已运行时 adopt，不新起进程（实测 before/after 进程数不变）。核心需要 `CCR_BIN` 才能自启；scheme 已注入 `.data/ccr/node_modules/.bin/ccr` |
| `agent_roles_list` | 花名册列表 | `CoreRoster` | 无参数；首启由 `CREWCHAT_AGENTS_DIR` 从 `.data/agents` 一次性导入（实测 8 个），之后读写隔离库。**需要较新的核心二进制**：本机已装 `.data/codeg/bin/codeg-server` 缺这条路由（501），从源码构建立即正常；开发期指向 `codeg/src-tauri/target/debug/codeg-server`（scheme 已改） |
| `agent_roles_save` | 新建 / 更新角色 | `CoreRoster` | body `{role:{id,name,soul,rules,agent_type,model,model_provider,skills,mcp,permission_mode,auto_approve}}`（snake_case）；`id` 为空=新建，返回带 id 的角色 |
| `agent_roles_delete` | 删除角色 | `CoreRoster` | body `{id}`；返回 bool |
| `agent_role_model_face` | 某引擎可绑定哪些模型 | `CoreRoster` | body `{agentType}`（camelCase）；返回 `{supported, reason?, models:[{provider, provider_id, model}]}`。`supported=false` 说明该引擎不能钉模型；`supported=true` 但为空说明查不到（CCR 不可达）。花名册的 `model_provider` 存 `provider`（显示名），兼容旧 id |
| `acp_list_agent_skills` | 某引擎可见的技能 | `CoreCapabilities` | body `{agentType, workspacePath?}`（camelCase）；返回 `{supported, message?, skills:[{id,name,description?,read_only}]}`。实测 claude_code=0（它走插件/工作区）、open_code=82 |
| `mcp_scan_local` | 本机 MCP 服务清单 | `CoreCapabilities` | 无参数；返回 `{servers:[{id,spec,apps}], warnings:[…]}`。花名册的 `mcp` 存 `server.id`（实测 6 个） |
| `acp_list_agents` | 引擎清单与就绪态 | `CoreEngines` | 无参数；返回 `agent_type/name/description/available/enabled/installed_version/distribution_type/is_acp_adapter/env`（snake_case） |
| `acp_update_agent_preferences` | 启用 / 禁用引擎（保留 env） | `CoreEngines` | body `{agentType, enabled, env}`（**camelCase**；漏写 agentType 会 400）。壳先取该引擎当前 `env` 原样回传，避免清空 |

| `list_open_folder_details` | 工作区（已打开的文件夹） | `CoreWorkspaces` | 无参数；返回 `FolderDetail[]`（snake_case：`id/name/path/alias/sort_order/kind/...`），核心已按 `is_open + kind=regular` 过滤排序 |
| `open_folder` | 添加工作区 | `CoreWorkspaces` | body `{path}`；返回 `FolderDetail`；只落库+标记打开，不动磁盘 |
| `remove_folder_from_workspace` | 移出工作区 | `CoreWorkspaces` | body `{folder_id}`（**snake_case**）；只把 `is_open` 置 false，不删库不删磁盘；返回 `null` |
| `list_all_conversations` | 工作区内的会话 | `CoreConversations` | body `{folderIds, includeChildren}`（camelCase）；返回 `DbConversationSummary[]`（snake_case：`folder_id/title/agent_type/status/kind/message_count/child_count/updated_at/...`） |
| `create_conversation` | 在项目内新建会话 | `CoreConversations` | body `{folderId, agentType, title?}`（**camelCase**；snake_case 会 400 `missing field folderId`）；返回新会话 id |
| `get_folder_conversation` | 会话详情 | `CoreConversations` | body `{conversationId, tailTurns}`（camelCase）；返回 `{summary, turns:[{id,role,timestamp,blocks:[{type,…}]}]}`。`role`=user/assistant/system；`blocks.type` 见代码（text/thinking/tool_use/tool_result/turn_error，其余走 `other` 不报错） |

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
