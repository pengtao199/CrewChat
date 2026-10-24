# 对核心的调用登记表

AgentOS 调用核心的**唯一清单**。新增调用必须在同一次改动里登记；未登记的调用视为越界（见 `AGENTS/agentos-boundary.md`）。

## 契约事实

| 项 | 值 |
| --- | --- |
| Base | `http://127.0.0.1:<port>` |
| 命令 | `POST /api/<command>`，命令名 = Tauri command 名（snake_case） |
| 事件 | `GET /ws/events`，attach 协议（`snapshot` / `replay` / `event` / `detached` / `pong`） |
| 鉴权 | HTTP `Authorization: Bearer <token>`；WS `Sec-WebSocket-Protocol: crewchat-events, crewchat-token.<base64url(token)>` |
| 错误 | `{ code(snake_case), message, detail?, i18n_key?, i18n_params? }` |

## 已登记命令

| 命令 | 用途 | 端口 | 注意点 |
| --- | --- | --- | --- |
| `health` | 核心存活与版本探测；启动与重连前判活 | `CoreLifecycle` | `POST /api/health`，返回 `{"status":"ok","version":"0.35.1"}`；同样需要 token。壳以 200 作为「已连接」的判据，并显示 `version`（2026-10-19 实测：`127.0.0.1:3080` 返回 200） |
| `get_delegation_settings` | 读委派开关与深度 | `CoreSettings` | 无参数；核心默认 `enabled=false`（2026-10-19 实测） |
| `set_delegation_settings` | 写委派设置（整对象） | `CoreSettings` | body `{settings:{enabled,depth_limit,agent_defaults,completed_cache_max_mb}}`；壳做读-改-写，只翻 `enabled`，其余原样回传（实测：写入后核心 `app_metadata` 四键齐全，`depth_limit` 未被改动） |

| `model_health` | 模型层控制面探活 | `CoreModel` | 无参数；返回 `{url, tokenConfigured, tokenSource}`。壳用它判断「模型层是否可达」 |
| `model_call`（`getConfig`） | 供应商与凭据 | `CoreModel` | body `{method:"getConfig", args:[]}`；返回 `{Providers:[{id,name,type,api_base_url,models,credentials:[{name,api_key,enabled}]}]}`。**这才是模型页的真相源**（`list_model_providers` 是 旧表，页面已不用） |
| `model_call`（`getProviderAccountSnapshots`） | 每个账号的额度 | `CoreModel` | args `["<provider name>", {}]`（或 `{"forceRefresh":true}`）；返回 `[{credentialLabel,status,meters:[{label,remaining,limit,used,unit,window,resetAt}]}]` |
| `model_call`（`getProviderPresets`） | 供应商预设（编辑弹窗用） | `CoreModel` | body `{method:"getProviderPresets", args:[]}`；返回 `[{id,name,endpoints:[{baseUrl,protocols}],defaultModels}]` |
| `model_provider_save` | 新建 / 更新供应商 | `CoreModel` | body `{originalId?, name, baseUrl, protocol, enabled, models, credentials:[{id?,name,apiKey?,priority?,weight?,limits?,enabled}]}`（camelCase）。核心在模型代理里做 **getConfig → 合并原始 JSON → saveConfig**：凭据按 id → name 合并，未知字段（account/transformer/modelMetadata…）原样保留，旧版单密钥在写入凭据池时移除；409 revision 冲突自动重试一次 |
| `model_provider_delete` | 删除供应商 | `CoreModel` | body `{id}`；返回 bool。被 `profile.profiles[*].providerId/providerName` 引用时拒绝删除并返回档案名，避免留下悬空档案 |
| `model_disabled_models` | 已停用模型清单 | `CoreModel` | 无参数；返回 `{ "<provider id/name>": ["model", …] }`。来自模型层 provider 的 `disabledModels` 字段 |
| `model_provider_set_model` | 启用 / 停用单个模型（模型页行内开关） | `CoreModel` | body `{providerId, model, enabled}`（camelCase）；返回 bool。cli-layer 代理模型层的 getConfig → 改 `models` / `disabledModels` → saveConfig；未知字段原样保留 |
| `model_provider_set_credential` | 启用 / 停用单条凭据 | `CoreModel` | body `{providerId, credentialId, enabled}`；返回 bool。只改该凭据的 `enabled` |
| `model_provider_probe` | 测试供应商并拉模型 | `CoreModel` | body `{baseUrl, apiKey?, models?, forceRefresh?}`；转发 模型层 `probeProvider(mode=models)`，超时 90s。返回 `{models, normalizedBaseUrl, detectedProtocol, …}`；模型编辑页用它同时做「测试连接」和模型目录 |
| `model_supervisor_start` | 模型层探活 | `CoreModel` | 无参数；返回 `{state:"running"}`；模型层由 AgentOS 在启动 cli-layer 前拉起 |
| `agent_roles_list` | 花名册列表 | `CoreRoster` | 无参数；读写 `cli-layer.db` 的 `agent_roles` |
| `agent_roles_save` | 新建 / 更新角色；MCP 绑定变化时同步 mcp-layer | `CoreRoster` | body `{role:{id,name,soul,rules,agent_type,model,model_provider,skills,mcp,permission_mode,auto_approve}}`（snake_case）；`id` 为空=新建，返回带 id 的角色。核心保存角色后若 `mcp` 变化，调用 mcp-layer 同步该 Agent 的 proxy；同步失败会回滚角色并返回错误，前端不要另发 `agent_mcps_set` |
| `agent_roles_delete` | 删除角色 | `CoreRoster` | body `{id}`；返回 bool |
| `agent_role_model_face` | 某引擎可绑定哪些模型 | `CoreRoster` | body `{agentType}`（camelCase）；返回 `{supported, reason?, models:[{provider, provider_id, model}]}`。`supported=false` 说明该引擎不能钉模型；`supported=true` 但为空说明查不到（模型层不可达）。花名册的 `model_provider` **新建/改选存 `provider_id`**；旧数据里的显示名仍能解析，Picker 读取时做一次 id 映射，不会静默改写旧值 |
| `agent_role_permission_face` | 某引擎支持的权限 Session Modes | `CoreRoster` | body `{agentType}`（camelCase）；返回 `{engine, supports_session_mode, supports_auto_approve, modes:[{id,name,description,source}]}`。`id` 是 ACP session mode id，空字符串表示引擎默认。Claude 返回空 id（引擎默认）/acceptEdits/plan/auto/bypassPermissions；Codex 返回 read-only/workspace-write/agent/agent-full-access；OpenCode 返回 build/plan；不支持的引擎只返回默认项 |
| `skill_list` | 全局技能库 | `CoreCapabilities` / `CoreSkills` | 无参数；返回 `{skills:[SkillSummary], collections:[Collection], globalLinks:[GlobalLink]}`。SkillSummary 含 `id/name/description/currentHash/license/sourceKind/sourceUrl/sourceRef/sourceSubdir/hasScripts/sizeBytes/installedAt/updatedAt`；`globalLinks` 是扁平的 `[{skillId,agentType,path,state,linkedAt}]`，前端按 skillId 分组 |
| `skill_read` | 读一个技能的 Markdown | `CoreSkills` | body `{skillId}`；返回 `{skill, content, files:[{path,size}]}` |
| `skill_install` | 安装技能 | `CoreSkills` | body `{path}` 或 `{source, gitRef?, subdir?}`；返回 `{skill}`。skill-layer 安装成功后自动执行 global-links sync（终端裸跑用），失败不回滚安装 |
| `skill_check` / `skill_update` / `skill_remove` | 检查 / 更新 / 删除 | `CoreSkills` | body `{skillId}`；分别返回 `{id,currentHash,latestHash,updateAvailable}` / `{skill}` / `{removed}` |
| `agent_skills_set` | 保存 Agent 技能绑定 | `CoreRoster` | body `{roleId, skillIds, collectionIds}`；返回 `{roleId, skillIds}`（集合会展开进 skillIds） |
| `agent_skill_status` | Agent 技能可用性 | `CoreRoster` | body `{roleId}`；返回 `{skills:[{id,name,available}], missing:[id]}` |
| `mcp_global_sync` | 把全局 MCP 开放给所有 CLI | `CoreMCP` | 无参数；启动全局 proxy 并写入各 CLI 的全局链接，返回 `{endpoint,links}` |
| `mcp_scan` | 扫描本机已安装 MCP | `CoreMCP` | 无参数；读取本机 CLI 配置并导入新条目，返回 `{discovered,imported,skipped,warnings}` |
| `mcp_list` | 全局 MCP 库与健康缓存（会先尽力扫描） | `CoreMCP` / `CoreCapabilities` | 无参数；返回 `{servers:[{id,spec,enabled,synced,hash,createdAt,updatedAt,health}]}`。`spec` 为 camelCase：`name/transport/description/enabled/command/args/env/url/headers/bearerToken/sessionMode`；`health={checking,available,checkedAt,error,toolCount}`。Agents 页只列 `enabled && health.available` 的 server |
| `mcp_upsert` | 新建 / 更新 MCP | `CoreMCP` | body `{serverId, spec:<object>}`；`spec` 同上，写入 mcp-layer，返回 `{server,warnings}`。保存后清空旧健康缓存并触发后台探测，`server.health.checking=true`；托管服务重启 Agent proxy 的失败信息进 `warnings` |
| `mcp_check` | 触发 MCP 健康探测 | `CoreMCP` | body `{serverId?}`；带 id 只探测该 server，不带 id 探测所有启用的 server；立即返回 `checking`，结果通过 `mcp_list.health` 读取。探测为一次性 MCP `initialize` + `tools/list`，超时 30s |
| `mcp_remove` | 删除 MCP | `CoreMCP` | body `{serverId}`；返回 `{removed,warnings}`。cli-layer 删除后同时清理 `agent_roles.mcp_json` 中的引用；mcp-layer 会停止/重建受影响 Agent 的 proxy |
| `acp_list_agents` | CLI 清单与就绪态 | `CoreEngines` | 无参数；返回 `agent_type/name/description/available/enabled/installed_version/registry_version/supports_custom_version/distribution_type/is_acp_adapter/env`（snake_case）。`available` = 现在能直接拉起（cli-layer 本地安装或 PATH 上有 adapter）；**不是「平台支持」**。`installed_version` = cli-layer 管理的安装版本；为 null 不等于不能装，npx/binary 仍可安装 |
| `acp_update_agent_preferences` | 写引擎偏置（env；enabled 仍保留） | `CoreEngines` | body `{agentType, enabled, env}`（**camelCase**；漏写 agentType 会 400）。CLI 页不再暴露启用开关，保存 env 时回传当前 `enabled`；首次加载会把历史 disabled 的内置引擎置回 true |
| `acp_fetch_agent_latest_release` | 检查可更新的新版本 | `CoreEngines` | body `{agentType}`；返回 `{version}` 或 `null`。只返回比核心 pin 新的版本，壳再与本地已装版本比较；网络失败按「无法检查」处理 |
| `acp_prepare_npx_agent` | 安装 / 更新 npx 或 uvx 引擎 | `CoreEngines` | body `{agentType, registryVersion?, version?, cleanFirst, taskId}`（camelCase）；同步等到安装结束，返回解析出的版本字符串。超时按 600s 计（npm/uv 下载可能数分钟）。uvx 不接受 version override |
| `acp_download_agent_binary` | 下载 / 更新二进制引擎 | `CoreEngines` | body `{agentType, version?, taskId}`；同步等到下载解压完成，超时 600s |
| `acp_install_pi_binary` | 安装 Pi 的 `pi` 运行时 | `CoreEngines` | body `{taskId}`；Pi 的 npx adapter（`pi-acp`）之外还要这个二进制，安装 / 更新 Pi 时串行调用 |
| `acp_env_diagnostics` | 引擎环境诊断 | `CoreEngines` | body `{agentType}`；返回 `{verdict:{level,code,summary}, sections:[{title,checks:[{label,value,status,hint}]}], plain_text}`（snake_case）。页面默认折叠，展开才调用 |

| `list_open_folder_details` | 工作区（已打开的文件夹） | `CoreWorkspaces` | 无参数；返回 `FolderDetail[]`（snake_case：`id/name/path/alias/sort_order/kind/...`），核心已按 `is_open + kind=regular` 过滤排序 |
| `open_folder` | 添加工作区 | `CoreWorkspaces` | body `{path}`；返回 `FolderDetail`；只落库+标记打开，不动磁盘 |
| `remove_folder_from_workspace` | 移出工作区 | `CoreWorkspaces` | body `{folder_id}`（**snake_case**）；只把 `is_open` 置 false，不删库不删磁盘；返回 `null` |
| `list_all_conversations` | 工作区内的会话 | `CoreConversations` | body `{folderIds, includeChildren}`（camelCase）；`folderIds` 是 UUID 字符串数组；返回 `DbConversationSummary[]`（snake_case：`folder_id/title/agent_type/status/kind/message_count/child_count/updated_at/...`） |
| `delete_conversation` | 归档（软删除）会话 | `CoreConversations` | body `{conversationId}`（camelCase）；只置 `deleted_at`，列表不再返回，数据仍保留（实测 1 → 0，`deleted_at` 非空） |
| `create_conversation` | 在项目内新建会话 | `CoreConversations` | body `{folderId, agentType, title?, team?}`（**camelCase**；`folderId` 是 UUID 字符串；snake_case 会 400 `missing field folderId`）。`team={leader_role_id, member_role_ids}` 时冻结团队快照、校验 readiness，并在同一事务写成员；返回新会话 id |
| `get_folder_conversation` | 会话详情 | `CoreConversations` | body `{conversationId, tailTurns}`（camelCase；`conversationId` 是 UUID 字符串）；返回 `{summary, turns:[{id,role,speaker_role_id?,timestamp,blocks:[{type,…}]}]}`。团队会话的 `summary` 带 `kind=team`、`leader_role_id`、`team` 冻结快照、`members[].state`、`members[].usage`、`delegations`（含 result `artifacts` / `durationMs`）和会话级 `usage`；`role`=user/assistant/system；`blocks.type` 见代码（text/thinking/tool_use/tool_result/turn_error，其余走 `other` 不报错） |

| `acp_connect` | 为会话拉起 CLI 连接 | `CoreSession` | body `{agentType, workingDir?, roleId?, conversationId?, sessionId?}`（camelCase）。`sessionId` 传会话的 `external_id` 时走 ACP `session/load` 续接历史；新会话成功后核心把 `session_id` 写回 `conversations.external_id`；返回 `connection_id`。首次 initialize 可能 8–10s，超时按 60s |
| `acp_prompt` | 发送一轮消息 | `CoreSession` | body `{connectionId, blocks:[{type:"text",text}], folderId, conversationId}`（camelCase；folder/conversation 均为 UUID 字符串）。立即返回，流式结果走 `/ws/events` |
| `acp_cancel` | 取消当前轮次 | `CoreSession` | body `{connectionId}`（camelCase） |

## 已登记事件

| 事件 / 帧 | 用途 | 端口 | 注意点 |
| --- | --- | --- | --- |
| `/ws/events` attach | 订阅连接或团队会话的流式事件 | `CoreSession` | 握手用 `Authorization: Bearer <token>` 即可（实测；同时带 `Sec-WebSocket-Protocol: crewchat-events`）。客户端帧 `{action:"attach", subscription_id, connection_id? 或 conversation_id?, since_seq?}`；服务端先发 `{"channel":"__ready__"}`（忽略），随后 `snapshot` / `replay` / `event` / `detached` / `pong`。团队会话可用 `conversation_id` 订阅；事件 envelope 带 `conversation_id` / `speaker_role_id` |
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
