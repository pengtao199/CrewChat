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
| `ccr_call`（`getProviderPresets`） | 供应商预设（编辑弹窗用） | `CoreCCR` | body `{method:"getProviderPresets", args:[]}`；返回 `[{id,name,endpoints:[{baseUrl,protocols}],defaultModels}]` |
| `ccr_provider_save` | 新建 / 更新供应商 | `CoreCCR` | body `{originalId?, name, baseUrl, protocol, enabled, models, credentials:[{id?,name,apiKey?,priority?,weight?,limits?,enabled}]}`（camelCase）。核心在 `ccr::providers` 里做 **getConfig → 合并原始 JSON → saveConfig**：凭据按 id → name 合并，未知字段（account/transformer/modelMetadata…）原样保留，旧版单密钥在写入凭据池时移除；409 revision 冲突自动重试一次 |
| `ccr_provider_delete` | 删除供应商 | `CoreCCR` | body `{id}`；返回 bool。被 `profile.profiles[*].providerId/providerName` 引用时拒绝删除并返回档案名，避免留下悬空档案 |
| `ccr_disabled_models` | 已停用模型清单 | `CoreCCR` | 无参数；返回 `{ "<provider id/name>": ["model", …] }`。落点 `CODEG_DATA_DIR/ccr-disabled-models.json`（核心自有，CCR 的 loader 会丢掉 provider 上的未知字段，所以不能存在供应商配置里） |
| `ccr_provider_set_model` | 启用 / 停用单个模型（模型页行内开关） | `CoreCCR` | body `{providerId, model, enabled}`（camelCase）；返回 bool。核心在 `ccr::provider_toggles` 里 getConfig → 改 `models` → saveConfig（停用同时写 `ccr-disabled-models.json`，供清单继续显示并再开），未知字段原样保留；409 冲突重试一次 |
| `ccr_provider_set_credential` | 启用 / 停用单条凭据（凭据行开关） | `CoreCCR` | body `{providerId, credentialId, enabled}`；返回 bool。只改该凭据的 `enabled`；旧版供应商级单密钥（无 `credentials`）返回 invalid input |
| `ccr_provider_probe` | 测试供应商并拉模型 | `CoreCCR` | body `{baseUrl, apiKey?, models?, forceRefresh?}`；转发 CCR `probeProvider(mode=models)`，超时 90s。返回 `{models, normalizedBaseUrl, detectedProtocol, …}`；模型编辑页用它同时做「测试连接」和模型目录 |
| `ccr_supervisor_start` | 拉起 / 接管 CCR | `CoreCCR` | 无参数；已运行时 adopt，不新起进程（实测 before/after 进程数不变）。核心需要 `CCR_BIN` 才能自启；scheme 已注入 `.data/ccr/node_modules/.bin/ccr` |
| `agent_roles_list` | 花名册列表 | `CoreRoster` | 无参数；首启由 `CREWCHAT_AGENTS_DIR` 从 `.data/agents` 一次性导入（实测 8 个），之后读写隔离库。**需要较新的核心二进制**：本机已装 `.data/codeg/bin/codeg-server` 缺这条路由（501），从源码构建立即正常；开发期指向 `codeg/src-tauri/target/debug/codeg-server`（scheme 已改） |
| `agent_roles_save` | 新建 / 更新角色 | `CoreRoster` | body `{role:{id,name,soul,rules,agent_type,model,model_provider,skills,mcp,permission_mode,auto_approve}}`（snake_case）；`id` 为空=新建，返回带 id 的角色 |
| `agent_roles_delete` | 删除角色 | `CoreRoster` | body `{id}`；返回 bool |
| `agent_role_model_face` | 某引擎可绑定哪些模型 | `CoreRoster` | body `{agentType}`（camelCase）；返回 `{supported, reason?, models:[{provider, provider_id, model}]}`。`supported=false` 说明该引擎不能钉模型；`supported=true` 但为空说明查不到（CCR 不可达）。花名册的 `model_provider` **新建/改选存 `provider_id`**；旧数据里的显示名仍能解析，Picker 读取时做一次 id 映射，不会静默改写旧值 |
| `acp_list_agent_skills` | 某引擎可见的技能 | `CoreCapabilities` / `CoreSkills` | body `{agentType, workspacePath?}`（camelCase）；返回 `{supported, message?, locations:[{scope,path,exists}], skills:[{id,name,scope,layout,path,description?,read_only}]}`。无 `workspacePath` 只读全局；传项目路径会同时返回 `scope=project` 的条目，页面按 scope 过滤。实测 open_code=82、codex=97 |
| `acp_read_agent_skill` | 读一个技能的 Markdown | `CoreSkills` | body `{agentType, scope:"global"|"project", skillId, workspacePath?}`；返回 `{skill, content}` |
| `acp_save_agent_skill` | 新建 / 更新技能 | `CoreSkills` | body `{agentType, scope, skillId, content, workspacePath?, layout?}`；新建传 `layout:"skill_directory"`，更新时后端沿用已有 layout。Web handler 返回 `null`，保存后需重拉列表 |
| `acp_delete_agent_skill` | 删除技能 | `CoreSkills` | body `{agentType, scope, skillId, workspacePath?}`；`read_only` 的内置技能会被后端拒绝 |
| `mcp_upsert_local_server` | 新建 / 更新 MCP 服务 | `CoreMCP` | body `{serverId, spec:<object>, apps:[wire names]}`（camelCase）。`apps` 是**权威集合**：没勾的 CLI 会被移除；`spec` 原样保存由后端 canonicalize |
| `mcp_remove_server` | 删除 MCP 服务 | `CoreMCP` | body `{serverId}`；不传 `apps` = 从所有 CLI 移除，返回 bool |
| `mcp_scan_local` | 本机 MCP 服务清单 | `CoreCapabilities` / `CoreMCP` | 无参数；返回 `{servers:[{id,spec,apps}], warnings:[{app,message}]}`。花名册的 `mcp` 存 `server.id`；MCP 页直接展示 `spec` 与 `apps`（实测 6 个） |
| `acp_list_agents` | 引擎清单与就绪态 | `CoreEngines` | 无参数；返回 `agent_type/name/description/available/enabled/installed_version/distribution_type/is_acp_adapter/env`（snake_case） |
| `acp_update_agent_preferences` | 启用 / 禁用引擎（保留 env） | `CoreEngines` | body `{agentType, enabled, env}`（**camelCase**；漏写 agentType 会 400）。壳先取该引擎当前 `env` 原样回传，避免清空 |

| `list_open_folder_details` | 工作区（已打开的文件夹） | `CoreWorkspaces` | 无参数；返回 `FolderDetail[]`（snake_case：`id/name/path/alias/sort_order/kind/...`），核心已按 `is_open + kind=regular` 过滤排序 |
| `open_folder` | 添加工作区 | `CoreWorkspaces` | body `{path}`；返回 `FolderDetail`；只落库+标记打开，不动磁盘 |
| `remove_folder_from_workspace` | 移出工作区 | `CoreWorkspaces` | body `{folder_id}`（**snake_case**）；只把 `is_open` 置 false，不删库不删磁盘；返回 `null` |
| `list_all_conversations` | 工作区内的会话 | `CoreConversations` | body `{folderIds, includeChildren}`（camelCase）；返回 `DbConversationSummary[]`（snake_case：`folder_id/title/agent_type/status/kind/message_count/child_count/updated_at/...`） |
| `delete_conversation` | 归档（软删除）会话 | `CoreConversations` | body `{conversationId}`（camelCase）；只置 `deleted_at`，列表不再返回，数据仍保留（实测 1 → 0，`deleted_at` 非空） |
| `create_conversation` | 在项目内新建会话 | `CoreConversations` | body `{folderId, agentType, title?}`（**camelCase**；snake_case 会 400 `missing field folderId`）；返回新会话 id |
| `get_folder_conversation` | 会话详情 | `CoreConversations` | body `{conversationId, tailTurns}`（camelCase）；返回 `{summary, turns:[{id,role,timestamp,blocks:[{type,…}]}]}`。`role`=user/assistant/system；`blocks.type` 见代码（text/thinking/tool_use/tool_result/turn_error，其余走 `other` 不报错） |

| `acp_connect` | 为会话拉起 CLI 连接 | `CoreSession` | body `{agentType, workingDir?, roleId?, sessionId?}`（camelCase）。`sessionId` 传会话的 `external_id` 以续接历史；返回 `connection_id`。首次 initialize 可能 8–10s，超时按 60s |
| `acp_prompt` | 发送一轮消息 | `CoreSession` | body `{connectionId, blocks:[{type:"text",text}], folderId, conversationId}`（camelCase）。立即返回，流式结果走 `/ws/events` |
| `acp_cancel` | 取消当前轮次 | `CoreSession` | body `{connectionId}`（camelCase） |

## 已登记事件

| 事件 / 帧 | 用途 | 端口 | 注意点 |
| --- | --- | --- | --- |
| `/ws/events` attach | 订阅某连接的流式事件 | `CoreSession` | 握手用 `Authorization: Bearer <token>` 即可（实测；同时带 `Sec-WebSocket-Protocol: codeg-events`）。客户端帧 `{action:"attach", subscription_id, connection_id, since_seq?}`；服务端先发 `{"channel":"__ready__"}`（忽略），随后 `snapshot` / `replay` / `event` / `detached` / `pong` |
| `event.envelope` | 单条事件（flatten 后 `type` 就在 envelope 顶层） | `CoreSession` | 壳处理 `content_delta`（增量文本）、`thinking`、`tool_call` / `tool_call_update`、`turn_complete`、`status_changed`、`detached`；其余忽略。`turn_complete` 后重新拉会话详情并清空 live 缓冲 |


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
