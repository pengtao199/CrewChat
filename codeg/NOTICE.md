# Vendored: Codeg

本目录是 [Codeg](https://github.com/spacering-net/codeg) 的**完整应用源码**，
按需固化进本仓库，作为协作工作台的二次开发基座，**不依赖外部克隆**。

- **Upstream**：`spacering-net/codeg`（Apache-2.0）
- **Vendored commit**：`e21eb6b38eea9405cdfa3b2d99df216d101d1344`（v0.35.1，2026-10-11）
- 许可见 `LICENSE`（原样保留）。
- 本目录的 `AGENTS.md` / `CLAUDE.md` 是**上游仓库自己的**开发指引（描述本目录的构建、检查方式），不是 CrewChat 的规则文件。

## 取舍

| 内容 | 处理 | 原因 |
| --- | --- | --- |
| 应用源码（`src/` Next.js、`src-tauri/` Rust/Tauri、`browser-agent/`、`public/`、`scripts/`、配置文件、`docs/` 文档） | 固化 | 二次开发的全部对象 |
| `.git/`（上游历史） | 不搬 | 与 cc-switch 的固化方式一致；升级用上游快照 diff 重放 |
| `docs/images/`（约 48MB 演示截图与 gif） | 不搬 | 与开发无关的体积 |
| 构建产物（`node_modules/`、`src-tauri/target/`、`.next/`、`out/`、`dist/`、`public/vs`、`src-tauri/binaries/`） | 由本目录自带的 `.gitignore`（含 `src-tauri/.gitignore`）忽略 | 产物不入库 |
| 测试夹具 `src-tauri/tests/fixtures/**/*.db` | 例外入库 | 是测试的组成部分；已在根 `.gitignore` 用 `!` 放行 |

## 二次开发的定位（记录当前决策）

- 本目录用于**替换 CrewChat 现有的 `ui/` 外壳**：协作界面采用 Codeg 的 `@` 委派、
  并行子会话、Sub-agents 面板、worktree 任务等形态，不再继续补齐 AionUi fork 的细节。
- **模型统一管理：UI 在本目录内、内核在 `ccr/`**。设置页「模型」按 Codeg 风格重做
  （`[crewchat]`），数据与动作全部经后端薄代理 `ccr_call` 访问 CCR；Codeg 自身
  **不写入**任何 CLI 的模型配置（对应 Agent 的模型一律留 Default）。
- **核心花名册：`[crewchat]` 新增「Agents」页**：定义角色（`Soul.md` 人格 / `Agents.md` 规则）
  并绑定身体（CLI）、模型、本地 Skill 与 MCP。v1 只做**定义与持久化**
  （`<data dir>/agent-roles.json`，ID 为服务端生成的 UUID）；按角色启动会话 / 委派时
  消费这些配置是下一步。
- 本仓库 Python 侧（`src/crewchat/`）的编排与网关内核暂时保留；若后续成为 Codeg 的
  自定义 ACP agent 或旁路服务，另立计划。

## 我们对上游做的改动

全部带 `[crewchat]` 标记，`rg '\[crewchat\]' codeg/` 可列出，便于升级时重放：

> **语言集已收窄（2026-10-17）**：界面语言只保留 `en` / `zh-CN`。下表里早先写作
> 「`src/i18n/messages/*.json`（10 语言）」的行，**重放时只落到 `en` / `zh-CN`**
> 两个文件；其余 8 个语言文件已删除。收窄本身见表末几行。

| 文件 | 改动 | 原因 |
| --- | --- | --- |
| `src/test-setup.ts` | 文件末尾追加 `[crewchat]` 兜底：Node ≥ 25 下为 `localStorage` / `sessionStorage` 提供可用的 `Storage` 实例 | 本机 Node 26 上 Node 自带 Web Storage 全局与 vitest/jsdom 冲突，751 个用例失败（Node 24 基线全绿）；上游 CI 固定 Node 24，暂未处理 |
| `src-tauri/src/commands/ccr.rs`（新增） | CCR 管理 RPC 薄代理：`ccr_call` / `ccr_health`，转发 `POST /api/ccr/rpc` | 模型层唯一内核是 CCR；Codeg 只做转发、不做模型写入 |
| `src-tauri/src/web/handlers/ccr.rs`（新增） | web 模式处理器（`/api/ccr_health`、`/api/ccr_call`） | 同上（Codeg 双模式规范） |
| `src-tauri/src/commands/mod.rs`、`src-tauri/src/web/handlers/mod.rs`、`src-tauri/src/web/router.rs`、`src-tauri/src/lib.rs` | 注册上述命令与路由 | 同上 |
| `src/lib/ccr.ts`（新增） | CCR 类型与调用层（getConfig / saveConfig / getGatewayStatus / start-stop-restart） | S2 模型页的数据层 |
| `src/components/settings/ccr-model-settings.tsx`（重写）＋ `src/components/settings/ccr/`（新增：helpers / gateway-card / provider-card / provider-dialog） | 新「模型」页：网关卡（可折叠）＋ 供应商卡（凭据概览 / 订阅用量 / 逐行模型开关）＋ 添加/编辑弹窗（预设、凭据池、双栏选模型、测试连接） | 模型管理 UI 以 CCR 为内核、Codeg 风格重做；单文件拆分为模块 |
| `src/lib/ccr-model-memory.ts`（新增） | 本地记录被停用的模型（CCR 无 per-model 停用字段） | 「关闭模型」后仍能保留在清单里并可再开 |
| `src/app/settings/model-providers/page.tsx` | 路由改指向 `CcrModelSettings` | 同上 |
| `src/components/settings/model-provider-settings.tsx`、`add-model-provider-dialog.tsx`、`edit-model-provider-dialog.tsx`、`model-provider-dialogs.fable-model.test.tsx` | **删除**（上游文件） | 旧模型管理退役；模型数据不再存 Codeg |
| `src/i18n/messages/*.json`（10 语言） | 新增 `CcrModelSettings` 命名空间；导航 `model_providers` 文案改短 | 同上 |
| `src-tauri/src/commands/agent_roles.rs`（新增） | Agent 角色花名册存储：`agent_roles_list` / `agent_roles_save` / `agent_roles_delete`；ID 为服务端 UUID、`model_provider` 与模型成对、JSON 落 `<data dir>/agent-roles.json`、原子写入 | 「Agents」页：角色（人格 / 规则）绑定身体、模型（提供商＋模型）、本地 Skill 与 MCP |
| `src-tauri/src/web/handlers/agent_roles.rs`（新增） | web 模式处理器（`/api/agent_roles_*`） | 同上（Codeg 双模式规范） |
| `src-tauri/src/commands/mod.rs`、`src-tauri/src/web/handlers/mod.rs`、`src-tauri/src/web/router.rs`、`src-tauri/src/lib.rs` | 注册上述命令与路由 | 同上 |
| `src/lib/agent-roles.ts`（新增） | 角色花名册类型与调用层 | 同上 |
| `src/app/settings/agent-roles/page.tsx`、`src/components/settings/agent-roles/`（新增：settings / role-editor / search-multi-select / helpers / helpers.test） | 新「Agents」页：左右分栏（对齐 mcp 页）＋ 编辑器（人格 / 规则 / 身体 / 模型 / 本地 Skill / MCP 可搜索多选） | 本仓库的核心花名册 |
| `src/components/settings/settings-shell.tsx` | 设置壳二次开发：导航**分组重排**（Agents / 能力 / 交互 / 系统）、标题栏新增**「返回工作区」**按钮、新增 `agent_roles` 项 | 导航以本仓库花名册为核心组织；web 模式设置是独立窗口，需要返回入口 |
| `src/components/settings/settings-shell.test.tsx` | 组顺序断言适配（原「协作在常规下」→ 逐组校验） | 同上 |
| `src/components/settings/collaboration-settings.test.tsx` | 标题查询限定 `level: 2`（页面标题与区块标题同名后的唯一化） | 协作页标题对齐「多智能体协同」后与委托区块标题同名 |
| `src/components/settings/collaboration-settings.test.tsx` | 标题查询限定 `level: 2`（页面标题与区块标题同名后的唯一化） | 协作页标题对齐「多智能体协同」后与委托区块标题同名 |
| `src/components/settings/acp-agent-settings.tsx` | CLI 智能体列表二次开发：按「已安装 / 未安装」分组（未安装区可折叠）、「只看已安装」筛选（本地记忆）、行内徽章改为安装态（已安装 / 可更新；检查异常降为提示图标）、未安装行加「安装」快捷按钮；另**隐藏** codex / gemini / claude 面板的「模型供应商」auth 模式入口（残留机制 2；完整拆除推迟，见下） | 平铺列表混排、检查态徽章语义混乱；「模型供应商」是死链（provider 行 0 条、无创建入口）且与『模型层=CCR』冲突 |
| `src/lib/agent-list-prefs.ts`（新增） | 「只看已安装」筛选的本地记忆 | 同上 |
| `src/components/settings/acp-agent-settings.provider-rebind.test.tsx` | **删除**（上游文件） | 专测「模型供应商」模式的 rebind 往返——入口已隐藏，行为不可达 |
| `src/components/settings/acp-agent-settings.fable-model.test.tsx` | 摘除 bound-provider 用例并清理其专用助手（`provider` / `choose`） | 同上；保留的直接编辑用例不受影响 |
| `src/i18n/messages/*.json`（10 语言） | 新增 `AgentRoles` 命名空间与导航 `agent_roles` 文案；`SettingsShell` 增分组头 / 返回键，改名（智能体→CLI 智能体、协作→多智能体协同，en/zh 系）；设置页内文案对齐（Agents 页标题改「CLI 智能体」、协作/外观页标题对齐、浏览器导航简化、终端描述勘误，en/zh 系）；`AcpAgentSettings` 增 `list` 子键（分组 / 徽章 / 提示，10 语言） | 同上 |
| `src/components/settings/agent-readiness-card.tsx`（新增） | CLI 页就绪卡：安装 / 启用 / 模型三行状态 + CCR 绑定行（已绑定 / 可绑定 / 暂不支持 / 不可达）＋ 重新应用 / 绑定到 CCR / 重试 ＋「要换模型？去 Agents 页」链接 ＋ 高级折叠开关；四态与安装/升级/折叠均有独立测试 | 把「生效链路」从藏着变成亮着（就绪卡是 CLI 页默认视图） |
| `src/components/settings/use-agent-ccr-binding.ts`（新增） | CCR 绑定 hook：读 profile 列表；三种写——建缺失 profile（`model` 留空、`scope: ccr`，与 CCR UI 新建一致）、apply、失败重试 | 绑定逻辑从 1.28 万行的上游页面文件里拆出，缩小升级重放面 |
| `src/components/settings/acp-agent-settings.tsx` | 详情面板改为「就绪卡 ＋ 高级折叠」：默认视图只留就绪卡；原 checks / 环境变量 / 各 CLI 配置管理 / 原生配置编辑器收进默认收起的「高级」区（`hidden` 切换，原内容与草稿状态零重排地保留）；就绪卡渲染 `AgentReadinessCard`、数据用 `useAgentCcrBinding` | 「三处都能碰模型」的迷路：CLI 页只回答「能不能跑 ＋ 是否已绑定 CCR」；模型字段退居高级、以 CCR profile 为准 |
| `src/lib/ccr.ts` | 增 `buildCcrProfileForKind` / `pickCcrProfileForKind` / `uniqueCcrProfileId` / `CCR_PROFILE_DISPLAY_NAMES`（profile 形状与 CCR UI 的规范化一致：`scope: ccr`、`model` 留空） | 绑定卡的「建缺失 profile」写入 |
| `src/lib/ccr.test.ts`、`src/components/settings/agent-readiness-card.test.tsx`（新增） | profile 构造 / 选择 / id 去重，以及就绪卡四态、安装/升级按钮、日志与折叠交互的用例 | 同上 |
| `src/i18n/messages/*.json`（10 语言） | `AcpAgentSettings` 增 `readiness` 子键（就绪卡状态 / CCR 绑定 / 高级与操作文案；中英繁实翻，其余语言沿用英文） | 同上 |
| `src-tauri/src/agent_host/`（`mod.rs` / `gateway.rs` / `role_home.rs`；新增，原单文件 `agent_host.rs` 2026-10-17 拆分） | Agent Host 配方（Phase 0.3，**只剩路由面**）：会话启动时从 CCR 读网关地址，注入 env（BASE_URL 三件套 / discovery / compat / NO_PROXY）；`CODEG_AGENT_HOST_INJECT=0` 完全回退。**模型注入通道已于 3.1.2 删除**——`AgentHostRecipe` 不再有 `config_options` | codeg 以绝对路径拉起真实 CLI，绕过 CCR 的 wrapper；按角色定模型由 3.1 隔离 home 承担（claude 运行期切模型确认不了，见 3.1.2） |
| `src-tauri/src/acp/manager.rs`、`src-tauri/src/commands/acp.rs`、`src-tauri/src/lib.rs` | 注册 `agent_host` 模块；`spawn_agent` 的 config 值只来自 caller 显式值（3.1.2 起配方不再提供模型项）；`build_runtime_env_for_agent` 合并配方 env（主会话与委派子代理共用） | 同上（实测笔记见 `docs/plans/agents-core-collaboration.md`） |
| `src-tauri/src/acp/delegation/team.rs`（新增） | Phase 1.3 团队解析：花名册 role id → 成员（名字 / 职责[取 soul 首行] / `agent_type`）；`TeamLookup` 注入 broker（生产 `DbTeamLookup` = 会话 `team` 列 + `agent-roles.json`）；读取失败降级为「无成员（不约束）」并记 warning | 团队门禁与团队帧共用一个解析源 |
| `src-tauri/src/acp/delegation/broker/lifecycle.rs`、`types.rs` | Phase 1.3 团队门禁：父会话带团队时 `delegate_to_agent` 只接受团队成员，越界返回 `not_in_team` 并列出可用成员（`名字 (agent_type)`）；无团队 / 不可解析 = 不约束。新增 `DelegationError::NotInTeam`（wire code `not_in_team`） | 越界必须清晰报错（体验关键），管理员照名单重规划 |
| `src-tauri/src/acp/agent_mentions.rs` | Phase 1.3 团队帧：第二种内部帧 `codeg_internal_team`（在场成员 / 职责 / 可派 `agentType`），与路由帧共用分隔符、字节级身份与剥离管线（查找器泛化为多帧 kind，标题截断同样命中）；帧只在委派开启的连接追加 | 管理员上下文含「在场成员」帧 |
| `src-tauri/src/acp/connection/mod.rs`、`src-tauri/src/app_state.rs`、`src-tauri/src/acp/delegation/broker/mod.rs` | 会话循环按 `conversation_id` 解析成员并在 `prepare_agent_bound_prompt` 追加团队帧；broker `with_team_lookup` 接线 `DbTeamLookup` | 接线（上游文件只留小集成点） |
| `src/components/chat/team-member-strip.tsx`（新增）＋ `src/lib/team-member-status.ts`（新增） | Phase 1.4 会话头成员条：团队会话头部一行成员 chip（排队/运行/等权限/完成/失败/已取消/待命；悬停任务预览）。状态解析 = 子会话行（DB，刷新安全）× 实时 delegation binding（running 由快照重播，终态回落子行）；「等权限」由 chip 订阅子连接 `pendingPermission` 升级 | 成员实时状态可视、刷新不丢 |
| `src/components/conversations/conversation-detail-header.tsx` | 标题行下挂 `TeamMemberStrip`（非团队会话与草稿不渲染） | 会话头成员条落点 |
| `src/contexts/delegation-context.tsx` | `listBindings()` —— 导出全部实时 binding（聚合视图消费） | 成员条按 `agent_type` 聚合实时状态 |
| `src/i18n/messages/*.json`（10 语言） | 新增 `TeamMemberStrip` 命名空间（中英繁实翻，其余沿用英文） | 同上 |
| `src-tauri/src/db/migration/m20261012_000002_custom_agent_role_fields.rs`（新增）＋ `db/entities/custom_agent.rs` | Phase 2.1：`custom_agent` 增 8 列（kind / body / instructions / rules / brain_model / brain_provider / skills_json / mcp_json；旧行默认 `kind='acp'`，行为不变） | 花名册入自定义 agent 注册表（开工拍板 #4） |
| `src-tauri/src/db/service/agent_role_service.rs`（新增）＋ `src-tauri/src/models/agent_role.rs`（新增） | 角色 CRUD（`kind='role'` 行，与 ACP agent 共 id 空间、冲突拒绝）＋ 一次性遗留迁移（`agent-roles.json` 先备份再导入、`.data/agents/*.jsonc` 去注释导入、重名以 Codeg 名册优先跳过、`app_metadata` 标记、非破坏可回滚） | 单一写入方、单一真相源；旧文件存储退役 |
| `src-tauri/src/db/service/custom_agent_service.rs` | `list_launchable_defs`（发射面过滤 role 行；hydrate / 自定义 agent 列表 / delete 全部限定 kind） | 角色不会被当作可启动的 ACP agent |
| `src-tauri/src/commands/agent_roles.rs`、`web/handlers/agent_roles.rs`、`acp/delegation/team.rs`、`db/mod.rs`、`commands/config_sync/{domains,snapshot}.rs` | 命令层改 DB 后端（async + State；命令名与 wire 形状不变）；team 查询直连服务；启动点挂迁移；配置同步跳过 role 行 | 接线（前端零改动） |
| `src-tauri/src/acp/agent_mentions/{routes,team,role,strip}.rs`（目录化） | Phase 2.2 角色帧 `codeg_internal_role`：角色名 / instructions / rules 以单行 JSON 落帧（三行体例，复用既有查找与剥离管线）；注入在最终边界且**不受委派开关限制**；标题截断路径同样命中 | 人格注入链路（拍板 #2/#4） |
| `src-tauri/src/acp/delegation/team.rs`、`spawner.rs`、`broker/{lifecycle,mod}.rs` | `TeamLookup::conversation_role`（会话 → 角色解析，无团队/失败降级）；`TeamMember.role_id`（serde skip——团队帧字节不变）；门禁解析出的成员把 `role_id` 写进 `DelegationLink.member_role_id` | 成员会话人格来源；"会话 `leader_role_id` = 本会话运行的角色"（子行写成员角色） |
| `src-tauri/src/db/service/conversation_service/mod.rs`、`src-tauri/src/models/conversation.rs` | `create_with_delegation` 将成员角色写入子行 `team` 快照（`member_role_ids` 空）；`ConversationTeam` 语义注释补 2.2 读法 | 同上 |
| `src-tauri/src/acp/connection/mod.rs` | `prepare_agent_bound_prompt` 增角色帧（先于路由/团队帧；人格与 codeg-mcp 无关），循环按 `conversation_id` 解析角色 | 注入点（上游文件小集成） |
| `src-tauri/src/acp/mcp_scope.rs`（新增） | Phase 2.3 角色 MCP 收窄：纯函数 `scope_servers_to_role`（空选择=继承 agent-type 全集；缺失项上报）+ 4 用例 | 分层：可测的收窄规则独立成模块 |
| `src-tauri/src/acp/session_state.rs`、`connection/mod.rs`、`manager/mod.rs` | `SessionState::role_mcp`（spawn 期注入、不入快照）；`spawn_agent_connection`/`spawn_agent` 增 `role_mcp` 参数；`load_mcp_servers_for_agent` 按角色收窄（INFO 记录 kept/missing） | 成员会话 `session/new` 只挂角色选中的 MCP |
| `src-tauri/src/acp/delegation/spawner.rs`、`broker/lifecycle.rs`、`delegation/team.rs` | `ConnectionSpawner::spawn` 增 `role_mcp`；门禁解析的成员表带 `mcp`（serde skip，团队帧字节不变）；broker 把成员角色的 MCP 名单带进子会话 spawn 并记 INFO | 角色选择从名册流到会话 |
| `src-tauri/src/acp/agent_mentions/role.rs` | 角色帧 v2：新增 `skills`（去重/截断）+ 条件化技能提示句；版本号 1→2（旧帧不再剥离，预发布策略同路由帧） | 成员被告知该用哪些技能（技能本体由 CLI 自身发现） |
| `src-tauri/src/agent_host/role_home.rs`（原 `agent_host.rs`） | Phase 3.1 角色模型 home：`build_claude_role_home`（**每子会话一个全新** `CLAUDE_CONFIG_DIR`：路由 env + 网关切片的精确虚拟模型 ID + 继承 apiKeyHelper/enabledPlugins + `projects/plugins/todos/…` 软链回共享目录，parser 照常可读）+ `prune_stale_role_homes` 启动清扫 24h 旧目录 | 成员会话按角色大脑模型启动（claude） |
| `src-tauri/src/agent_host/gateway.rs` | 网关模型目录与大脑映射：`provider/model` → 精确虚拟 ID（`h<hex>`，优先 `[1m]`）；provider 接受 id 或显示名（按 CCR `Providers[]` 映射后编码）；省略 provider 时逐个 provider 尝试，仍无果则在**唯一** provider 下按模型名 hex 后缀匹配；解析不到一律 `None`。3.1.2 起配套的 `claude_model_option_id` 与 profile key 查找已删（唯一调用方是已删的配方模型通道），密钥查找收敛为 `local_gateway_api_key` | 花名册里的旧 provider（如 `75f800be`）此前会让隔离 home 写下 claude 确认不了的 `ANTHROPIC_MODEL`，会话一起即死（`Couldn't confirm model`）；放宽形状后旧 provider + 有效模型名可救回，无 provider 也能定位 |
| `src-tauri/src/acp/delegation/spawner.rs`、`broker/lifecycle.rs`、`delegation/team.rs` | `RoleSpawnContext`（role_id/model/provider/mcp/model_applied）贯通 spawn；spawner 建 home 注入 `CLAUDE_CONFIG_DIR`，manager 在角色模型已绑定时跳过配方模型项；`team::pick_member` 同 CLI 多成员按 task 中的角色名消歧（最早提及优先），无提及退回首个 | 角色事实从名册流到会话；同名 CLI 成员可分别命中各自模型 |
| `src-tauri/src/acp/session_state.rs`、`acp/manager/mod.rs`、`db/mod.rs` | `SessionState::role_mcp`；`spawn_agent*` 增角色参数（非委派调用传 None）；启动点清扫旧角色 home | 接线 + 运维 |
| `src/hooks/use-child-usage.ts`（新增）＋ `src/components/message/delegation-card-row.tsx`、`delegated-sub-thread.tsx` | Phase 3.2 按成员用量：委派卡在子回合终态时按次拉取子会话 `session_stats`（与「会话详情」同口径：`Folder.statusBar.tokens` 文案 + `formatTokenCount`），显示 输入 / 输出 tokens 与时长；按子会话缓存，失败静默 | 同一任务的各成员用量可横向对比 |
| `src/i18n/messages/*.json`（10 语言） | `Folder.chat.delegation` 增 `usageTitle`（中英繁实翻，其余英文） | 同上 |
| `src/components/message/delegated-sub-thread.tsx` | Phase 4.1 卡内内联成员流：卡可展开子会话实时流（`LiveTranscriptView`，默认收起、固定高度）；与抽屉**互斥**——任一方式打开即让另一方退出（`viewerHost?.close?.()`），因为两者挂同一个实时桥（同一子会话两个桥会互相清空） | 不开抽屉也能看成员进度，不影响现有卡与抽屉（4.1） |
| `src/components/message/delegation-card-row.tsx` | 新增可选 `trailingAction` 槽（标签块与「查看会话」之间，承载内联开关）；其余用此行的卡（`ResumedDelegationCard`）仍是两动作形态 | 同上 |
| `src/components/message/session-viewer-host-context.ts`、`session-viewer-host.tsx` | `SessionViewerHostValue` 增可选 `close?()`（内联流打开时请抽屉让位；不提供该方法的 host 依旧可用） | 同上 |
| `src/components/message/delegated-sub-thread.inline.test.tsx`（新增） | 内联流 8 例：默认单行、展开指向子会话与子连接、可折叠、无子会话无开关、与抽屉互斥（双向）、同一时刻只挂一个桥、卡自身行不变 | 同上 |
| `src/i18n/messages/*.json`（10 语言） | `Folder.chat.delegation` 增 `expandDetail` / `collapseDetail`（中英繁实翻，其余英文） | 同上 |
| `src/lib/hermes-providers.test.ts`、`src/lib/tab-view-order.test.tsx` | 源码路径断言改指结构拆分后的落点（`src-tauri/src/commands/acp/mod.rs`、`src/components/conversations/conversation-detail-panel/index.tsx`） | 结构拆分（第一批 / 第四批）漏改这两处，全量测试一直红；同批其他源码级测试已改 |
| `src/components/conversations/conversation-detail-header.test.tsx`、`conversation-detail-header-tags.test.tsx` | `@/stores/app-workspace-store` fake 补 `subscribe`（同 `team-member-strip.test.tsx` 既有写法） | Phase 1.4 的 `TeamMemberStrip` 把 `acp-connections-context → notification-session → tab-store`（模块级 `subscribe`）带进会话头的导入图，只带 `getState` 的 fake 会让整个 suite 在收集期崩 |
| `src-tauri/src/acp/delegation/meta_writer.rs`、`broker/{lifecycle,resume,cancel,pending,lookups}.rs`、`delegation/listener.rs`、`commands/conversations/mod.rs` | Phase 4.1.1 委派卡成员名：委派 meta 增 `member_role_id`（running / terminal / cancel / resume 四处写入；刷新路径 `build_historical_delegation_meta` 从子行团队快照补，重建后的卡也带名字）；`ChildResumeContext` / `RunningTask` 增同名成员字段并贯通 resume / cancel 路径；**`DelegationLink.member_role_id` 改用 `pick_member` 的解析结果**（原「按 `agent_type` 首个匹配」在同一个 CLI 有多个成员时会指错人） | 卡要显示「谁做的」，而实时 binding 只带父子 transport 映射、不带角色；meta 里放稳定的 role id，显示名前端按名册解析（改名 / 删角色即时生效）。历史 meta 构造器按职责移入 `meta_writer`（`historical_delegation_meta`），`conversations` 只留走 turn 的注入胶水 |
| `src/lib/delegation-card.ts`、`src/lib/agent-roles.ts` | `parseDelegationMeta` 读出 `member_role_id`（空串视同缺失，不当查找键）；名册增模块级缓存 `listAgentRolesCached`（失败不缓存，避免一次瞬时错误把全场卡片钉在回落标签上；`agent_roles_save` / `agent_roles_delete` 后失效），并留 `resetAgentRolesCache` 测试缝 | 一次会话里卡片很多，名册只取一次；编辑后立即可见 |
| `src/hooks/use-member-role-name.ts`（新增）＋ `src/hooks/use-delegation-card-model.ts`、`src/components/message/delegation-card-row.tsx` | 成员名解析 hook（role id → 名册名；名册读不到或角色已删回落 `null`，不显示占位）；卡模型暴露 `memberName`；行渲染主标签 = 成员名、次要标记 = CLI 名（无成员时与原来完全一致） | 成员名从 meta 流到卡面 |
| `src/components/message/delegated-sub-thread.tsx`、`resumed-delegation-card.tsx` | 从卡模型取 `memberName` 传给 `DelegationCardRow` | 同上 |
| `src/lib/delegation-card.test.ts`、`src/components/message/delegation-card-row.test.tsx`（新增） | meta 解析 2 例（读出 `member_role_id` / 空串与缺失同为 `null`）＋ 行渲染 2 例（成员名 + CLI 次要标记；无成员回落 CLI 名） | 同上 |
| `src-tauri/src/ccr/`（`mod.rs` / `client.rs` / `supervisor.rs`；新增） | Phase 5.1 CCR 托管：`client.rs` 收拢管理面**传输**（`CCR_WEB_URL` / `CCR_WEB_AUTH_TOKEN` 契约、RPC 转发，从 `commands/ccr.rs` 移出）；`supervisor.rs` 探活（TCP，不看 token）/ 接管（可达即采纳）/ 拉起（仅回环 + token 已固定 + 能解析 `ccr` CLI 时 `ccr start`）/ 守护（20→300s 退避封顶）/ `CcrDaemonStatus`；判据与退避是纯函数（13 用例） | 管理进程以前要用户手动起；进程管理仍交给 CCR 自带的 `ccr start`（daemon + `service.json`），Codeg 不重造 |
| `src-tauri/src/commands/ccr.rs` | 传输移出后只剩命令适配：`ccr_health` / `ccr_call` / `ccr_supervisor_state` / `ccr_supervisor_start` | 一个契约一个归属；命令层只管适配 |
| `src-tauri/src/web/handlers/ccr.rs`、`src-tauri/src/web/router.rs` | 新增 `POST /api/ccr_supervisor_state` / `/api/ccr_supervisor_start`（[crewchat] 自有 `/api/*`，与 `api-contract.md` 的 Python `/api/v1` 契约无关） | Codeg 双模式规范 |
| `src-tauri/src/app_state.rs`、`src-tauri/src/lib.rs`、`src-tauri/src/bin/codeg_server.rs`、`src-tauri/src/web/mod.rs` | `AppState.ccr_supervisor`（两模式同一实例；桌面 setup 与 server 各起一次守护，网页服务复用 Tauri 已 manage 的那个）；`CODEG_CCR_SUPERVISE=0` 关托管 | 接线 |
| `src/lib/ccr.ts` | 增 `CcrDaemonStatus` / `ccrSupervisorState` / `ccrSupervisorStart` | 5.1 状态卡数据层 |
| `src/components/settings/ccr/ccr-daemon-card.tsx`（新增）＋ `ccr-daemon-card.test.tsx`（新增，5 用例） | 「CCR 服务」卡：状态徽标 + 由 Codeg 启动 / Codeg 之外启动 + pid + CLI 路径 + 无法自启原因 + 上次错误 + 启动 / 重试 / 重新探活；**只起不停** | 托管状态要看得见；与 `GatewayCard`（网关本体）上下相邻：卡管宿主，网关卡管代理 |
| `src/components/settings/ccr-model-settings.tsx` | 顶部挂 `CcrDaemonCard` | 落点（一行） |
| `src/i18n/messages/*.json`（10 语言） | `CcrModelSettings` 增 13 个 `daemon*` 子键（中英繁实翻，其余语言实翻） | 同上 |
| `scripts/verify.mjs`（新增，`pnpm verify`） | Phase 5.1.1 按改动派生最小验证：从 `git diff HEAD` 分类改动文件 → Rust 按该文件自己的 `#[cfg(test)] mod <name>` 精确过滤（无内联测试 = 接线，只编译）、`src/bin/**` 补 `--bins`、前端跑同名测试文件（无则只 `tsc`）、locale JSON 跑键位对齐、Python 跑 `import`；`--dry-run` 只打印计划 | 一轮改动不该付全量测试的代价（用户 2026-10-17 拍板）；`cargo test --lib` 仍编译整个 crate，省的是「跑什么」与「一次构建而不是 check+test 两次」 |
| `src/i18n/messages/*.json`（10 → 2 语言） | **界面语言只保留 `en` / `zh-CN`**：删除 `zh-TW` / `ja` / `ko` / `es` / `de` / `fr` / `pt` / `ar` 八个文案文件；`Language` 命名空间与 `ChatChannelSettings.language` 各自去掉退役语言的语言名 | 产品决定：界面只提供中英文（`AGENTS.md` 的语言一节已同步） |
| `src/lib/types.ts`、`src/lib/i18n.ts`、`src/i18n/messages.ts`、`src/i18n/global.d.ts` | `AppLocale` / `IntlLocale` 收窄为 `en`+`zh_cn` / `en`+`zh-CN`；映射表与 `isIntlLocale` 同步；`mapLocaleTagToAppLocale` 去掉其余语言分支，**`zh` 系统标签（含 `zh-TW` / `zh-Hant` / `zh-HK`）一律落到 `zh_cn`**，不再有 `zh_tw` | 同上；中文只发简体，繁体系统标签仍应读中文而不是掉到英文 |
| `src/lib/date-fns-locale.ts`、`src/components/i18n-provider.tsx`、`src/components/conversations/search-command-dialog.tsx` | date-fns 映射只留 `enUS` / `zhCN`；`i18n-provider` 删掉 `dir="rtl"` 分支（已无 RTL 语言）；搜索弹窗的日历 locale 同步收窄 | 同上 |
| `next.config.ts` | next-intl 预编译 `experimental.messages.locales` 收窄为 `["en", "zh-CN"]`（不删会因文案文件已不存在而构建失败） | 同上 |
| `src/components/settings/system-network-settings.tsx`、`src/components/settings/channel-other-tab.tsx` | 应用语言选择器与「消息语言」下拉只列中英文（含各自的文案表与 `<SelectItem>`） | 同上 |
| `src-tauri/src/models/system.rs` | `AppLocale` 收窄为 `En` / `ZhCn`；**手写 `Deserialize`**：`"zh_cn"` → `ZhCn`，其余（含已退役语言）→ `En`，使旧 `system_language_settings` 行仍能读出而不是报解析失败 | 同上；`load_system_language_settings` 解析失败是硬错误，旧库必须容忍 |
| `src-tauri/src/commands/windows.rs` | `window_titles_for` 只留中英文窗口标题 | 同上 |
| `src-tauri/src/chat_channel/i18n.rs`、`src-tauri/src/commands/chat_channel.rs` | `Lang` 枚举收窄为 `En` / `ZhCn`，约 90 处 `match` 去掉退役语言分支；`from_str_lossy` 只认中英文；`set_chat_message_language` 允许列表改 `["en", "zh-cn"]` | 同上；`Lang::from_str_lossy` 对未知值本就有 En 兜底 |
| `src/i18n/messages.test.ts`、`src/components/settings/system-network-settings.test.tsx`、`src/components/settings/release-notes.test.tsx`、`src/components/shared/directory-browser.test.tsx` | 键位对齐 / ICU / 本地化用例收窄到两种语言；原「本地化 + RTL」用例改用 `zh-CN`（已无 RTL 语言可测） | 同上 |
| `src/lib/agent-brain-options.ts`（新增） | Agent「大脑」判定与拆分：`isBrainConfigOption`（模型按 `isModelConfigOption`、推理按 id / ACP `thought_level` 类别识别）、`splitAgentConfigOptions`（大脑 vs 可调）、`brainOptionCurrentLabel`（走 `modelListGroups` 解析当前值，保留 `provider/` 前缀剥离） | 大脑=模型+推理，只读；按稳定 id / category 判定而非显示文案 |
| `src/components/chat/agent-brain-chip.tsx`（新增） | 只读 chip：复用 `buttonVariants({variant:"ghost",size:"xs"})` 度量，`Lock` 图标替代下拉箭头，`SelectorTooltip` 说明为何不可改（用 `span` 而非 `disabled` Button，否则 `disabled:pointer-events-none` 会吞掉 tooltip 的 hover） | 「可见但不可调」需要一个既保留既有 chip 观感、又不假装可点的替身 |
| `src/components/chat/message-input.tsx` | composer 接线：config option 拆为 `brainConfigOptions` / `switchableConfigOptions`；inline 行先渲染只读 brain chip，再渲染可调项；折叠 cog 面板顶部加 brain chip 行、其 rail 只保留可调项。`hasConfigOptions` 仍按**全部**选项推导，加载态与 mode 互斥语义不变 | 会话中不允许切换模型；大脑不进「选值」面板 |
| `src/components/chat/message-input.test.tsx` | 断言模型下拉的两个用例删除（行为已不可达），折叠面板用例改用一个可调 select；新增「大脑只读」三条：模型/推理渲染为不可点 chip 且显示真实值、cog 面板 rail 不含大脑 | 同上 |
| `src/components/conversations/conversation-detail-panel/conversation-tab-view.tsx`、`src/components/chat/team-selector.tsx`（+ `.test.tsx`）、`src/i18n/messages/*.json` 的 `TeamSelector` 命名空间 | 会话起点「Agent 优先」：Agent（花名册角色）排到 CLI 选择器之前；选中的 Agent 决定引擎（`team.leader_role_id` → 角色的 body），CLI 选择器随即只读并带一行说明。按钮改为显示 Agent 名——原来只选 leader 时显示「Team · 1」，看不出是谁在跑；`aria` / `leaderLabel`（Leader→Agent）/ 清除项文案一并改为 Agent 说法 | 会话页从「以 CLI 为核心」改为「以 Agent 为核心」；「花名册角色即本会话运行的 Agent」正是后端 `team` 列的语义（delegation child 就是这么落的），空成员表本来就被所有消费方当作「无成员」 |

## 结构拆分登记（2026-10-12 起；规则见 `AGENTS/upstream.md`）

拆分 = 文件→目录 / 内嵌测试外移，行为与公共 API 不变（全量测试绿）。升级时把上游对新版单文件的改动应用到下表对应结构。

| 上游文件 | 本仓库结构 | 拆分内容 |
| --- | --- | --- |
| `src-tauri/src/acp/connection.rs` | `acp/connection/mod.rs` + `acp/connection/tests.rs` | 内嵌测试外移（16k 行 tests.rs） |
| `src-tauri/src/commands/acp.rs` | `commands/acp/mod.rs` + `tests.rs` | 尾部测试模块外移（6.6k 行）；中部 `diagnostics_tests` 留原位 |
| `src-tauri/src/acp/delegation/broker.rs` | `acp/delegation/broker/mod.rs` + `broker/{pending / reports / tool_calls / lifecycle / resume / cancel / status / lookups}.rs` + `tests.rs` | 内嵌测试外移（5.2k 行）；mod.rs 再按架构拆成 8 个子模块（4.6k → 全部 <1k；子模块项升 `pub(crate)`，`broker::…` 表面经 glob re-export 不变；307 用例绿） |
| `src-tauri/src/acp/custom_registry.rs` | `acp/custom_registry/mod.rs` + `{def / error / validate / meta / registry}.rs` + `tests.rs` | 按职责拆分（1.5k → mod 47 / def 261 / error 68 / validate 137 / meta 210 / registry 242 / tests 607）；`custom_registry::…` 表面经 glob re-export 不变；跨文件私有助手升 `pub(super)`（`effective_version` / `split_archive_entry`）；21 用例 + 相关 2104 用例绿） |
| `src-tauri/src/acp/agent_mentions.rs` | `acp/agent_mentions/mod.rs` + `{routes / team / role / strip}.rs` + `tests.rs` | 按帧 kind 拆分（1.5k → mod 35 / routes 210 / team 173 / role 168 / strip 358 / tests 598）；`agent_mentions::…` 表面经 `pub(crate)` glob re-export 不变；2724 用例绿） |
| `src-tauri/src/acp/manager.rs` | `acp/manager/mod.rs` + `tests.rs` | 内嵌测试外移（5.3k 行） |
| `src-tauri/src/commands/conversations.rs` | `commands/conversations/mod.rs` + `tests.rs` | 内嵌测试外移（4.0k 行） |
| `src-tauri/src/db/service/conversation_service.rs` | `db/service/conversation_service/mod.rs` + `tests.rs` | 内嵌测试外移（2.0k 行） |
| `src-tauri/src/acp/delegation/companion.rs` | `acp/delegation/companion/mod.rs` + `tests.rs` | 内嵌测试外移（3.1k 行） |
| `src-tauri/src/acp/delegation/tool_schema.json` | `acp/delegation/companion/tool_schema.json` | 随 companion 目录化移动（`include_str!` 相对路径） |
| `src/components/settings/acp-agent-settings.tsx` | `acp-agent-settings/index.tsx` + `agent-reorder-item.tsx` + `acp-agent-settings/helpers/*`（types / context / env / config-text / claude / gemini / opencode / cline / openclaw / grok / codex / codex-toml / hermes / deepseek / important-config / draft / version / checks） | 前端最大文件拆分（12.9k → 页面组件 8.7k + 18 个 helpers，全部 <1k；测试零改动 157 用例绿） |
| `src/components/conversations/conversation-detail-panel.tsx` | `conversation-detail-panel/index.tsx` + `conversation-tab-view.tsx` + `helpers.ts` + `split-strip.tsx` | 巨型面板目录化（3.2k → 面板 670 / 视图 2.4k / 纯函数 62 / 角条 48）；6 个源码级测试改指新路径（含一个双源拆分），语义零改动。视图的 hook 级二次拆分列为后续批次 |
| `src-tauri/src/commands/conversations/mod.rs`（职责外移，非目录化） | `acp/delegation/meta_writer.rs`（`historical_delegation_meta`）+ `commands/conversations/mod.rs` 只留走 turn 的注入胶水 | 历史委派 meta 构造器按「一个形状一个归属」移入 meta 模块，与 `build_delegation_meta` 并列（两者必须同形状）；`commands/conversations/mod.rs` 2.8k → 2.76k，新函数在原地有 2 个直接用例；115 + 7 用例绿 |
| `src-tauri/src/commands/ccr.rs`（职责外移） | `ccr/client.rs`（管理面传输：URL / token 解析、RPC 转发、地址解析与回环判据）+ `commands/ccr.rs` 只留 `#[tauri::command]` 适配 + `ccr/supervisor.rs`（托管） | 原来「怎么连 CCR」和「CCR 起没起」都要写进命令层；拆开后传输唯一归属，命令层 91 → 45 行，托管单独一层（判据是纯函数，可测） |
| `src-tauri/src/parsers/codex.rs`（2026-10-17，批次 A·脚本） | `parsers/codex.rs`（6.86k）+ `parsers/codex/tests.rs`（9.88k） | 内嵌测试外移，`scripts/split-tests.mjs` 执行：文件只放模块 body 字节（缩进不动），原地留 `#[cfg(test)] mod tests;`，过滤器 `parsers::codex::tests` 198 用例绿 |
| `src-tauri/src/parsers/claude.rs`（批次 A·脚本） | `parsers/claude.rs`（3.67k）+ `parsers/claude/tests.rs`（4.39k） | 同上（批次 A 第 2 刀），过滤器 `parsers::claude::tests` 93 用例绿 |
| `src-tauri/src/work_task/engine.rs`（批次 A·脚本） | `work_task/engine.rs`（7.29k）+ `work_task/engine/tests.rs`（5.71k） | 同上（第 3 刀，为 5.2 引擎收口先降上下文负担），过滤器 `work_task::engine::tests` 134 用例绿；3 处 item 级 `#[cfg(test)]` 测试助手按设计留源内 |
| `src-tauri/src/commands/acp.rs`（批次 A·脚本） | `commands/acp/mod.rs`（13.9k）+ `commands/acp/diagnostics_tests.rs`（351 行） | 中部 `mod diagnostics_tests` 外移为同级子文件（第 4 刀），25 用例绿；7409 行起的 Pi 域是源码，按批次另行目录化 |
| `src-tauri/src/commands/acp.rs`（批次 B·5.3 铺路） | 新增 `commands/acp/pi.rs`（255 行）+ `commands/acp/hermes.rs`（1196 行） | Pi 配置/校验/路径探针域与 HermesProvider 全域（env 解析、写盘计划、reconcile）按使用域逐区出列；Tauri `__cmd__` 宏随定义模块绑定，**带命令包装器的模块不迁移**（只搬无 wrapper 的实现）；PiConfigProjection 等以显式 re-export 保持原可见面；commands::acp:: 281 用例 + web::local_address 5 用例全绿。上线 opencode 原生配置仍未拆（区域内混入多个命令包装器，改到时按单个符号顺序分批出列） |
| `src-tauri/src/acp/session_state.rs`（批次 A·脚本） | `acp/session_state.rs`（2.44k）+ `acp/session_state/tests.rs`（2.98k） | 同脚本（第 5 刀），过滤器 `acp::session_state::tests` 92 用例绿 |
| `src-tauri/src/acp/background_watch.rs`（批次 A·脚本） | `acp/background_watch.rs`（1.87k）+ `acp/background_watch/tests.rs`（2.52k） | 同上（第 6 刀），过滤器 `acp::background_watch::tests` 64 用例绿 |
| `src-tauri/src/acp/delegation/listener.rs`（批次 A·脚本） | `acp/delegation/listener.rs`（1.63k）+ `acp/delegation/listener/tests.rs`（2.39k） | 同上（第 7 刀，自有代码），过滤器 `acp::delegation::listener::tests` 41 用例绿 |
| `src-tauri/src/commands/mcp.rs`（批次 A·脚本） | `commands/mcp.rs`（6.10k）+ `commands/mcp/tests.rs`（2.10k） | 同上（第 8 刀），过滤器 `commands::mcp::tests` 51 用例绿 |
| `src-tauri/src/parsers/opencode.rs`（批次 A·脚本，5.3 铺路） | `parsers/opencode.rs`（2.48k）+ `parsers/opencode/tests.rs`（1.85k） | 第 9 刀，过滤器 41 用例绿 |
| `src-tauri/src/parsers/pi.rs`（批次 A·脚本，5.3 铺路） | `parsers/pi.rs`（1.50k）+ `parsers/pi/tests.rs`（1.36k） | 第 10 刀，过滤器 38 用例绿 |
| `src-tauri/src/parsers/gemini.rs`（批次 A·脚本，5.3 铺路） | `parsers/gemini.rs`（1.07k）+ `parsers/gemini/tests.rs`（0.72k） | 第 11 刀，过滤器 22 用例绿 |
| `src-tauri/src/parsers/hermes.rs`（批次 A·脚本，5.3 铺路） | `parsers/hermes.rs`（0.85k）+ `parsers/hermes/tests.rs`（0.18k） | 第 12 刀，过滤器 12 用例绿 |
| `src-tauri/src/acp/question.rs`（批次 A·脚本，4.x 铺路） | `acp/question.rs`（1.79k）+ `acp/question/tests.rs`（1.76k） | 第 13 刀，过滤器 56 用例绿 |
| `src-tauri/src/db/service/work_task_service.rs`（批次 A·脚本，5.2 铺路） | `work_task_service.rs`（2.84k）+ `work_task_service/tests.rs`（2.20k） | 第 14 刀，过滤器 43 用例绿 |
| `src-tauri/src/db/service/token_usage_service.rs`（批次 A·脚本，用量页铺路） | `token_usage_service.rs`（0.75k）+ `token_usage_service/tests.rs`（0.72k） | 第 15 刀，过滤器 18 用例绿 |
| `src-tauri/src/commands/token_usage.rs`（批次 A·脚本，用量页铺路） | `commands/token_usage.rs`（1.22k）+ `commands/token_usage/tests.rs`（0.78k） | 第 16 刀，过滤器 32 用例绿 |
| `src-tauri/src/work_task/engine.rs`（批次 B·目录化） | `engine/mod.rs`（mod.rs 919 行）+ 8 个 impl 分片子模块 + `worktree / outstanding / tools` 归口件 | 第一步：impl TaskEngine 按职责八分（launch/deliver/merge/compaction/progress/events/cleanup/pump）；第二步：53 个自由条目按使用者归口九个子模块；mod.rs 只留类型、boot 入口、emit 与生命周期核心。`compact` 子模块因撞名改 `compaction`；跨块互调的私有方法/字段按 house 惯例升 `pub(super)`，`conversation_title_for_task` 保持 pub(crate)、`CleanupBlocked`/`EngineWorkTaskTools` 保持 pub 供上层再导出；work_task 全域 140 用例全绿 |

## 构建与开发

前置：Node ≥ 22（本机 26.x）、**pnpm 11.9.0**（`package.json` 的 `packageManager` 锁定；
本机全局是 9.15.9，直接 `pnpm install` 会报 `packages field missing or empty`，用
`npx -y pnpm@11.9.0` 或先安装 pnpm 11）、Rust stable（本机 1.96.1）。

```bash
cd codeg
npx -y pnpm@11.9.0 install   # 前端依赖（postinstall 会把 monaco 拷到 public/vs）

# 前端（Next.js 静态导出）
pnpm dev                # 开发服务器
pnpm build              # 静态导出构建
pnpm lint && pnpm test  # 检查与测试

# 桌面应用（Tauri）
pnpm tauri dev          # 开发运行（beforeDevCommand 先起 Next）
pnpm tauri build        # 打包

# 服务器模式
pnpm server:build
pnpm server:dev
```

### [crewchat] server 模式启动（本仓库约定）

`codeg-server` 与 `codeg-mcp` 必须**成对**：多智能体委派（`delegate_to_agent`）由
`codeg-mcp` 伴生进程承载——落在 server 二进制**同级目录**时自动被发现；分离部署
（安装布局与开发构建混用）时用 `CODEG_MCP_BIN` 指到同一源码构建的二进制。
找不到只降级（不注入委派工具 + 一行警告），不影响其他功能。

**构建要点**：先 `cd src-tauri && cargo build --no-default-features --features server-bin --bin codeg-server`。
默认 features（含 tauri-runtime）的 debug 构建**会换用 `codeg-dev.db`**（空库，见 `db/mod.rs`
的 `database_file_name`）——跑过 `cargo test` / `cargo check` 后必须重跑这条 server-bin 构建，
否则静态服务看似正常、数据却"消失"。

```bash
cd codeg && \
  CODEG_PORT=3080 CODEG_HOST=127.0.0.1 CODEG_TOKEN=<token> \
  CODEG_STATIC_DIR=$PWD/out \
  CCR_WEB_URL=http://127.0.0.1:3458 \
  CCR_WEB_AUTH_TOKEN=$(cat ../.data/ccr/dev-token) \
  CCR_BIN=$PWD/../.data/ccr/node_modules/.bin/ccr \
  CODEG_MCP_BIN=$PWD/src-tauri/target/debug/codeg-mcp \
  ./src-tauri/target/debug/codeg-server
```

- `CCR_WEB_URL` / `CCR_WEB_AUTH_TOKEN`：模型页与就绪卡的 CCR 薄代理走这里。
- `CCR_BIN`：5.1 托管用它拉起 CCR（`ccr start`）；不设则从 `PATH` 找 `ccr`，找不到就只报「无法自启」。
  `CODEG_CCR_SUPERVISE=0` 关掉托管（默认开）。
- 委派开关：「设置 → 多智能体协同 → 启用委托」（`delegation.enabled`，默认关）。

Rust 侧检查（在 `codeg/src-tauri/` 下执行）：

```bash
cargo check
cargo test --features test-utils
cargo clippy --all-targets --features test-utils -- -D warnings
cargo check --no-default-features --features server-bin --bin codeg-server
```

## 升级上游

1. 记录新的 upstream commit（`spacering-net/codeg`）；
2. 以相同排除项（`.git/`、`docs/images/`）获取源码快照，与本目录 `diff`；
3. 重放上表所有 `[crewchat]` 改动；
4. 更新本文件的 Vendored commit；
5. 跑 `pnpm lint && pnpm test`（前端）与 `cargo check`（Rust），确认仍可构建。
