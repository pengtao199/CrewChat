# AgentOS 壳（macOS 前端）

Updated: 2026-10-19 (Asia/Shanghai)
Status: In Progress（S0 ✅；S1 ✅；S2 ✅；S3 ✅；S4 ✅（权限审批归 S6））

## Goal（终态）

一个可日常使用的 macOS 原生客户端：用户在 AgentOS 里选工作区与 Agent，发起任务，看见多个 Agent 谁在说、调了什么工具、花了多少，能随时取消与审批。**当前阶段所有真相与执行都在核心（`codeg-server`）**，AgentOS 只渲染与交互。

> 终态是 Swift 自己成为核心、去掉 codeg/CCR；见 [Swift 原生运行时迁移](swift-runtime-migration.md)。本计划的 Non-Goals（不实现 ACP / 不 spawn CLI / 不碰模型）只约束当前「壳 + codeg」阶段。

判定达标：把核心当黑盒，`Features` 里不出现任何核心字段形状（`AGENTS/agentos-layout.md` 的硬约束）；杀掉核心再拉起，界面靠 `snapshot` / `replay` 自愈。

## Non-Goals

- 不做第二套 ACP / 不 spawn CLI（那是核心的事，见 `AGENTS/agentos-boundary.md`）。
- 不在壳里实现设置逻辑：存储、校验、副作用都在核心，壳只做展示与编辑。
- v1 不做内嵌终端 / 浏览器 / computer-use、forge / canvas / pets / office / automation、多语言界面。
- 不改核心的 Rust 行为（本计划只消费它的既有 HTTP/WS 契约）。
- 不做 iOS（本机跑不了 CLI 子进程；将来是另一条产品线）。

## Current Facts（2026-10-19，已核实）

**核心侧（只读事实，来自 `codeg/src-tauri/src`）**

- 二进制 `codeg-server` 由 `--no-default-features --features server-bin` 构建；本机已装一份在 `.data/codeg/bin/codeg-server`（67M），同目录 `web/`（93M）是 Next 静态导出。
- 环境变量：`CODEG_HOST`（**默认 `0.0.0.0`**）、`CODEG_PORT`（默认 3080）、`CODEG_TOKEN`、`CODEG_DATA_DIR`、`CODEG_HOME`、`CODEG_STATIC_DIR`。
- token：`CODEG_TOKEN` 非空即采用（不落库）；否则复用/生成并持久化到 DB。
- 不传 `CODEG_STATIC_DIR` 也能起（`find_static_dir_standalone` 只打 warn）。
- HTTP：`POST /api/<command>`，命令名同 Tauri command（snake_case）；鉴权 `Authorization: Bearer <token>`。
- 错误体：`{ code(snake_case), message, detail?, i18n_key?, i18n_params? }`（`app_error.rs`）。
- WS：`GET /ws/events`，协议头 `Sec-WebSocket-Protocol: codeg-events, codeg-token.<base64url_nopad(token)>`；attach 帧 `snapshot` / `replay` / `event` / `detached` / `pong`，带 `subscription_id`，`event` 带 `envelope.seq`。
- ACP 探测上限 60s → 壳的 HTTP 超时基线 60s。
- 工作区：`list_open_folder_details`（`is_open=true` 且 `kind=regular`，已按 `sort_order` / `last_opened_at` 排序）；添加用 `open_folder {path}`；移除用 `remove_folder_from_workspace {folderId}`（只把 `is_open` 置 false，不删库、不删磁盘）。
- 会话列表：`list_all_conversations {folderIds, …}` → `DbConversationSummary`（`folderId` / `title` / `kind` / `status` / `agentType` / `model` / `messageCount` / `childCount` / `pinnedAt` / 时间戳 / `team?`）。
- 会话详情：用 `get_folder_conversation {conversationId, tailTurns?}`（DB 背书 + live core 合并），不是旧的 `get_conversation`（transcript 解析路径）。
- 委派设置：`get_delegation_settings` / `set_delegation_settings { settings }`；核心默认 `enabled = false`。
- **已装的 `.data/codeg/bin/codeg-server` 是旧构建**：`agent_roles_*` / `agent_role_model_face` 在它上面返回 501；仓库源码构建的二进制才有这些路由（2026-10-19 实测）。开发期应指向 `codeg/src-tauri/target/debug/codeg-server`。
- 设置 CRUD 齐备：模型供应商 `list/create/update/delete_model_provider`；花名册 `agent_roles_list/save/delete` + `agent_role_model_face`；MCP `mcp_*`；技能 `acp_*_agent_skill` / `custom_*_skill`；快捷短语 `quick_messages_*`；语言 `get/update_system_language_settings`。
- **数据隔离（2026-10-20 实测）**：`CODEG_DATA_DIR` 与 `CODEG_HOME` 都固定指向 `~/Library/Application Support/AgentOS/core`；工作区（`folder`）、会话（`conversation`）、花名册（`custom_agent`）都在同一个 `codeg.db`。用临时库验证：`open_folder` + `create_conversation` 后临时库 folder=1 / conversation=1，应用库仍为 0。

**AgentOS 侧**

- S0 已完成：工程、分层骨架、sidecar 生命周期、token、`health`。
- S1 已完成：设置三页（通用 / 核心 / 关于）、外观、自动拉起、核心控制、委派开关（默认开启）、设置入口（菜单 ⌘, + 工具栏齿轮）。
- `AgentOS/engine-api.md` 已登记 `health` / `get_delegation_settings` / `set_delegation_settings`。

## 开工拍板（默认值；无异议按默认执行）

| # | 决策点 | 默认 | 备选 |
| --- | --- | --- | --- |
| 1 | sidecar 来源 | 开发期用路径配置（默认探测 `.data/codeg/bin/codeg-server`，其次自己 `cargo build` 的产物）；发布期随包进 `Contents/MacOS/` | 只认随包一份 |
| 2 | 端口 | **总是自选空闲端口并自拉核心**（3080 空闲就用，否则取临时端口）。不复用已起的核心：它的 token 只在它自己的库里 | 让用户粘贴别人的 token |
| 3 | token | 壳生成（`SecRandomCopyBytes`）→ 写 0600 文件（`AgentOS/core/token`）→ 传 `CODEG_TOKEN` | Keychain（ad-hoc 签名会反复弹窗，否决） |
| 4 | 数据目录 | **固定** `~/Library/Application Support/AgentOS/core`（隔离，设置里只读）；开发期用 `CREWCHAT_AGENTS_DIR` 从 `.data/agents` 一次性导入花名册（暂不考虑发布形态） | 可配置目录（否决） |
| 5 | 错误展示 | 先用核心的 `message` | 解析 `i18n_key` 自己翻（后置） |
| 6 | 界面语言 | 只 `zh-Hans`，String Catalog 登记 | 双语 |
| 7 | 首屏 | 直接工作台（工作区侧栏 + 主对话区） | 欢迎页（否决） |
| 8 | 与 Next 工作台并行 | 不并行；AgentOS 是替代品，核心的 `CODEG_STATIC_DIR` 指向空目录 | 内嵌 WebView 做对照（否决） |
| 9 | 主界面布局 | **两栏**：左侧可折叠工作区（`Section(isExpanded:)`，初始全折叠）+ 右侧主对话区；`.inspector` 默认隐藏、记住上次开合（⌘⌥I） | 三栏固定（否决） |
| 10 | 委派开关 | 放「核心」页；**默认开启**（首次运行一次性写入核心，落「已初始化」标记，之后以核心为准） | 跟随核心默认（false） |
| 11 | 设置页 | 8 个 tab：通用 / 核心 / 模型 / Agents / 技能 / MCP / 引擎 / 关于；每项对应已登记的核心命令 | 只做壳自身设置（否决） |
| 12 | 设置入口 | **窗口内整页切换**：左栏 = 设置项列表，右栏 = 详情；齿轮 / ⌘, 进入，工具栏「返回工作区」退出 | 独立设置窗口 / Settings scene（否决） |
| 13 | 侧栏折叠 | 工作区初始全折叠，折叠状态持久化 | 全展开（否决） |

## 设置与主界面（S1–S3 的落点）

**设置 8 个 tab**（展示与编辑在壳，逻辑在核心）：

| Tab | 内容 | 核心命令 |
| --- | --- | --- |
| 通用 | 外观、启动行为（自动拉起 / 恢复上次工作区） | 壳本地 |
| 核心 | 二进制路径、数据目录、端口策略、重启、打开日志 / 目录、**委派开关（默认开启）** | 壳本地 + `CoreLifecycle` + `get/set_delegation_settings` |
| 模型 | 供应商增删改、预设、凭据池、模型选择、额度 | `ccr_call getConfig` + `ccr_provider_save/delete/probe` |
| Agents | 花名册增删改：角色 / 引擎 / 模型 / 技能 / MCP / 权限 | `agent_roles_list/save/delete` + `agent_role_model_face` |
| 技能 | 按引擎管理全局/项目技能，读写 Markdown | `acp_list/read/save/delete_agent_skill` |
| MCP | 扫描/新建/编辑/删除本机 MCP 服务 | `mcp_scan_local` / `mcp_upsert_local_server` / `mcp_remove_server` |
| 引擎 | CLI 启用、环境变量、使用中的 Agent | `acp_list_agents` + `acp_update_agent_preferences` |
| 关于 | 壳版本、核心版本、许可、诊断 | `Bundle` + `health` |

**主界面**：两栏 + `.inspector`。

- 侧栏：每个工作区一个 `Section(isExpanded:)`（初始折叠）；会话行显示标题 + 引擎 + 相对时间；`childCount > 0` 给 chevron；置顶进 Pinned 组；顶部 `.searchable` 本地过滤。
- 主栏：会话标题、消息 / 工具调用、输入框；工具栏放运行 / 取消 / inspector。
- inspector（默认隐藏、记住开合）：最多 3 个 `Section`——会话信息、用量（`sessionStats`）、项目（路径 / 分支）。不放主操作，不重复主栏，跨会话用量趋势不放这里。

## 分层与命名（新增代码的落点）

按 `AGENTS/agentos-layout.md`：

```text
AgentOS/
├── AgentOS.xcodeproj
└── AgentOS/
    ├── App/                 场景、依赖装配、sidecar 接线
    ├── Domain/              领域类型 + Ports/（CoreLifecycle / CoreWorkspaces / CoreConversations / CoreSettings / CoreEvents）
    ├── Core/                端口实现 + Wire/（Core 前缀 DTO）
    │   ├── HTTP/            CoreClient（命令调用、错误解码）
    │   ├── WS/              CoreEventStream（attach、seq、重连）
    │   ├── Process/         SidecarProcess（拉起、探活、退出清理）
    │   └── Wire/            Core* DTO
    ├── Infrastructure/      TokenStore、Preferences
    └── Features/            Workbench（侧栏 + 主栏）/ Settings / Inspector
```

`Features` 不得 `import Core`（编译期可查）。

## 步骤计划

### S0（✅ 2026-10-19）：工程骨架 + 拉起并连上核心

- Intent：能启动、找到/拉起核心、完成 `health`，界面显示真实连接态。
- 已落地：Xcode 工程（objectVersion 77，文件系统同步组）；非沙盒 + hardened runtime；`CoreLifecycle` 端口；token（0600 文件）+ 端口探测 + sidecar 生命周期 + 60s 轮询 `health`；`Features/Connection`；退出清理。
- Verify（已跑）：`xcodebuild` 通过；`lsof` 只见 `127.0.0.1:3080`；`/api/health` 200；核心库无 `web_service_token`（token 来自壳）；quit 后子进程退出。
- Checkpoint：`Features` 里只有 `CorePhase` / `CoreEndpoint` 两个领域类型。

### S1（✅ 2026-10-19）：设置基建（通用 / 核心 / 关于）

- Intent：⌘, 能改外观与核心配置；委派开关落到核心；关于页说真话。
- 已落地：
  - 三页设置（通用 / 核心 / 关于）作为窗口内的详情视图；外观（`@AppStorage` + `.preferredColorScheme`，主窗口与设置页同时生效）；启动自动拉起核心（`@AppStorage("core.autoStart")`，默认开）。
  - 设置入口：窗口内整页切换——齿轮或 ⌘, 把左栏换成设置项列表（`List(selection:)`）、右栏换成详情（`SettingsDetail`），工具栏尾部变成「返回工作区」；不使用 `Settings` scene。
  - 核心页：二进制路径与数据目录（`NSOpenPanel`，改动后提示需重启）、端口策略只读、重启（stop → start → 重新读设置）、打开日志 / 数据目录（`NSWorkspace`）。
  - 委派开关：`get/set_delegation_settings` 走 `CoreSettings` 端口；首次连接时 `applyDefaultsIfNeeded()` 一次性写入 `enabled=true` 并落 `UserDefaults("core.delegationInitialized")`；之后一律以核心为准。断核时开关禁用并显示原因。
  - 关于页：壳版本 / build、核心版本（来自 `health`）、许可链接、打开日志。
  - SIGTERM 转优雅退出（`DispatchSourceSignal` → `NSApp.terminate`），修掉 `kill` 后留下孤儿核心。
  - `ENABLE_DEBUG_DYLIB = NO`：单二进制，便于 ad-hoc 签名与复制测试。
- Verify（已跑）：
  - `xcodebuild` 通过。
  - 用独立 bundle id 的副本启动：3080 被占 → 自动选 **62499** 并拉起第二个核心；新数据目录的 `app_metadata` 出现 `delegation.enabled=true`（首次默认开启生效），`depth_limit=1` / `completed_cache_max_mb` 原样保留。
  - `kill -TERM` 后该实例的核心退出，另一个核心不受影响。
  - 未做：设置窗口的视觉走查（没有屏幕录制许可，无法截图）；三页布局按系统 `Form` 默认样式。
- 已知行为：macOS 不会给**同一 bundle id 的第二个实例**创建窗口（进程存在但无场景，`.task` 不触发）。开发时不要让 Xcode 实例与命令行实例同时跑；测试用独立 bundle id 的副本。

### S2（✅ 2026-10-20）：两栏壳（工作区折叠侧栏 + 会话 + 只读主栏）

- Intent：真实以文件夹为工作区；工作区内可见会话；点开能读。
- 已落地：`WorkbenchViewModel` + `WorkbenchSidebar` + `ConversationPane` / `ConversationView`；左侧 `List` + 每个工作区一个 `DisclosureGroup`（初始折叠），右侧只读渲染 turns（文本 / 思考 / 工具调用 / 工具结果 / turn_error，未知块降级为类型名不报错）；未连接时仍显示连接页。
- 侧栏布局（2026-10-20 优化，参考 Codeg 左栏）：
  - 顶部「项目」+ `...` + `+`；`...` 菜单展示「整理侧边栏（按项目 / 在一个列表中）」与「聊天排序方式（优先级 / 最近更新 / 手动排序）」——**只展示，未接能力**。
  - 项目行：折叠箭头 + 文件夹 + 名称；悬停出现 `...` 与「新建会话」（Menu 选引擎 → `create_conversation`，创建后展开并选中）。
  - 项目 `...` 菜单：置顶项目 / 在 Finder 中显示 / 创建永久工作树 / 编辑项目 / 归档聊天 / 移除本地项目；**只有「在 Finder 中显示」「归档聊天」「移除本地项目」接通**，其余置灰。
  - 会话行：悬停/选中出现「置顶」（置灰）与「归档」（`delete_conversation` 软删除）；右键同菜单。
  - 所有行用灰色圆角背景（悬停 12%、选中 22%），侧栏底 `windowBackgroundColor`，保证一致性；底部固定「设置」入口；窗口工具栏不再放 + 与齿轮。
- 新增端口 `CoreWorkspaces` / `CoreConversations`（同文件 `CoreWorkbench.swift`）。
- Verify（已跑，用 `.data/codeg` 的**备份库**起核心）：`list_open_folder_details` 1 个工作区；`list_all_conversations` 3 条（codex/claude_code）；`get_folder_conversation` 4 个 turn，`role=user/system`、`blocks.type=text/turn_error` 与解码一致；应用自拉核心后工作区为空（隔离库），xcodebuild 通过。
- 已知：隔离库没有他们的历史会话，所以首屏是「添加工作区」；要看到历史会话需要一次性导入 `.data/codeg`（未做，待定）。
- 单核心保护（2026-10-20）：启动时先读 `core.json` 探测已有核心，健康则**采用**（不新起），否则清理后自拉；退出只终止自己拥有的核心。实测：已有核心时启动第二个壳，核心进程数不变。
- 数据落点：所有业务数据（工作区 / 会话 / 消息 / 花名册）都在固定隔离库；设置 → 核心显示数据库完整路径；`CODEG_HOME` 也钉在同一目录，上传/宠物不会跑偏。
- Files：`Domain/{Workspace,Conversation,ConversationKind,ConversationStatus}.swift`、`Domain/Ports/{CoreWorkspaces,CoreConversations}.swift`、`Core/Wire/{CoreFolderDetail,CoreConversationSummary}.swift`、`Core/HTTP/CoreClient+Workspaces.swift`、`Core/HTTP/CoreClient+Conversations.swift`、`Features/Workbench/{WorkbenchView,WorkbenchViewModel,WorkspaceSidebar,ConversationDetailView}.swift`。
- Steps：
  - 侧栏：`List(selection:)` + 每个工作区一个 `Section(isExpanded:)`（`@SceneStorage` 记住折叠，初始折叠）；工具栏「添加工作区」→ `NSOpenPanel` → `open_folder`；右键「在 Finder 中显示」「从工作区移除」（`confirmationDialog` 写明只移出列表）。
  - 会话：一次 `list_all_conversations` 拉全量，按 `folderId` 分组；`.searchable` 本地过滤；空态 `ContentUnavailableView`。
  - 主栏：`get_folder_conversation` → 只读渲染 turns（用户 / Agent 文本 + 工具名）；没选会话时的空态。
  - inspector 骨架：`.inspector` + 工具栏切换 + `@AppStorage` 记住开合；先放会话信息与项目路径。
  - 登记 `engine-api.md`：`list_open_folder_details` / `open_folder` / `remove_folder_from_workspace` / `list_all_conversations` / `get_folder_conversation`。
- Verify：选真实文件夹 → 看到该文件夹会话；折叠状态重启后保留；移除后列表消失但目录仍在；空工作区、无工作区、加载失败三种态都有可操作提示。
- Checkpoint：`Features` 里没有 `snake_case`；枚举未知值走 `unknown` 分支。

### S3（✅ 2026-10-19）：设置页——模型 / Agents / 引擎

- Intent：把核心已有的 CRUD 做成原生表单，不重造逻辑。
- 已落地：
  - 模型：**走 CCR（`ccr_call`）**，不是 codeg 的旧 `model_provider` 表——右侧依次是基本信息（名字 / 类型 / 地址 / 每个账号的密钥，带眼睛显示与隐藏）、**账号与额度（可折叠）**、**模型逐行（可折叠）**、CCR 状态；工具栏刷新额度；CCR 不可达时给出启动入口。只读，编辑模型留到后续。
  - Agents：`agent_roles_list/save/delete`；列表显示名字 / 引擎；编辑器含身份（名字、引擎、供应商、模型）、人格、能力、权限。
    - 供应商 / 模型改成 Picker：数据来自 `agent_role_model_face`（按引擎给 provider → model），不再手写；不支持的引擎锁住并显示原因；旧值不在面上时保留为可选项，不静默替换。
    - 人格与规则：Markdown 编辑 / 预览切换；预览走 `AttributedString(markdown:)`（行内样式 + 保留换行，块级标题/列表未做原生渲染）。
    - 技能 / MCP：多选（DisclosureGroup + Toggle），来源 `acp_list_agent_skills` / `mcp_scan_local`；已选但当前清单里没有的条目会保留展示。
  - 引擎：`acp_list_agents` + `acp_update_agent_preferences`；显示版本、可用状态、启用开关；切换时把该引擎当前 `env` 原样回传，避免清空配置。
  - 新增端口 `CoreModels` / `CoreRoster` / `CoreEngines`；`CoreClient` 抽出 `sendRaw` / `sendIgnoring`（`delete` 返回 `null` 也能处理）。
  - 设置侧栏扩到 6 项：通用 / 核心 / 模型 / Agents / 引擎 / 关于。
  - 三页统一主从骨架（`MasterDetailLayout`：`HSplitView` 左窄右宽）：左列表选中即编辑，右侧详情底部统一「保存 / 删除」；`+` 在工具栏尾部。
  - 显示修复：错误态与空态互斥（Agents 之前同时显示 501 与空态）；引擎状态收进详情，列表只留名字与版本。
  - 开发期核心路径：scheme 的 `AGENTOS_CORE_BINARY` 改指 `codeg/src-tauri/target/debug/codeg-server`（已装二进制是旧构建，缺 `agent_roles_*`）。
  - 数据目录固定为 `~/Library/Application Support/AgentOS/core`（设置里只读，不再提供选择）；开发期 scheme 注入 `CREWCHAT_AGENTS_DIR=$(SRCROOT)/../.data/agents`，核心首次启动把 8 个角色导入隔离库（`app_metadata` marker 已写）。
  - Agents 的编辑保存走 `agent_roles_save`：实测改名后重新 list 已落库，恢复原名后无测试残留。
- Verify（已跑，源码新构建的核心 + 临时数据目录 + 临时端口）：
  - `list_model_providers` → `[]`；`create_model_provider`（open_code + 普通模型名）成功、`list` 可见、`update` 返回 `{provider, affectedRunningSessions}`、`delete` 返回 `null`。
  - `agent_roles_list` → 内置 8 个角色；`agent_roles_save` → 带 UUID 的角色；`agent_roles_delete` → `true`。
  - `acp_list_agents` → 完整引擎清单；`acp_update_agent_preferences` → `0`（camelCase；先按 snake_case 写时 400 `missing field agentType`，已修）。
  - 未做：三页的视觉走查（无屏幕录制许可）；模型面 picker 未接，模型 / 供应商仍是文本输入。
- Checkpoint：设置页没有一条校验规则是壳自己发明的（`model` 的引擎差异也由核心报错）。

### S3b：模型面 picker 与引擎安装

- `agent_role_model_face {agentType}` → `{supported, reason, models:[{provider, provider_id, model}]}`，Agent 编辑器按它做级联 Picker（引擎 → 供应商 → 模型）；`supported=false` 时锁住并显示原因（核心已提供，实测 claude_code 返回可用清单）。
- 引擎页的安装 / 升级 / 卸载 / CCR 绑定（`acp_download_agent_binary` / `acp_uninstall_agent` / `acp_fetch_agent_latest_release` / `ccr_*`）与就绪卡。
- 能力页（MCP / 技能 / 快捷短语）的 CRUD。

### S4（✅ 2026-10-20）：对话（草稿页 + 发消息 + 流式 + 工具 + 取消）

- Intent：核心体验跑通。
- 已落地：
  - **设置三页（2026-10-21）**：
    - Agents：未保存守卫（切换角色/设置页/返回工作台/新建/复制前确认）、保存/取消、删除确认、`provider_id` 绑定、模型失效提示、技能/MCP 错误不再被 `try?` 吞掉、复制角色、搜索、权限模式 Picker、Textual 预览。
    - 引擎：重新检测、env 编辑器（增删改 + 保存）、不可用说明、内联错误（不再整页替换）。
    - 模型：供应商弹窗按 CCR 设计重做（预设、名称/地址两列、凭据池含导入 JSON/优先级/权重/限额/排序、模型左右双栏、测试连接）；核心 `ccr_provider_save` 支持 `credentials[]` 与改名同步 profile 引用；模型/凭据行内开关与停用清单；密钥遮罩修复、账号请求去重。
    - 核心单测：`ccr::providers` 3 条（保留未知字段、创建补 id、档案引用时拒绝删除）。
  - **技能 / MCP 管理页（2026-10-22）**：
    - 技能：按引擎列出全局/项目技能，支持搜索、读/写 Markdown、预览、新建、删除；内置只读技能锁定；项目级需要先打开工作区。
    - MCP：扫描本机 15 个 CLI 的 MCP 配置，展示 spec/apps/读取警告；JSON 编辑 + 模板 + 写入 CLI 多选，支持新建/编辑/删除；保存时未勾选的 CLI 会被移除。
    - 核心调用登记到 `AgentOS/engine-api.md`；设置页未保存守卫扩展到技能页。
  - **Markdown**：vendored [Textual 0.5.0](https://github.com/gonzalezreal/textual)（MIT，源码在 `AgentOS/Vendor/Textual/`，改动表见其 `NOTICE.md`）。助手正文用 `StructuredText` 块级渲染（标题 / 列表 / 代码块 / 表格），流式中仍用纯 `Text`，完成后切换。**deployment target 提到 macOS 15**。删掉了 math 支持（避免再引 7.5MB 的 swiftui-math）。
  - 草稿页：`你想让 <项目> 构建什么？` + Agent 多选（第一个为 leader，选中态用 `borderedProminent`）+ 底部统一输入容器（左下 `+` 占位、右下圆形发送箭头；不展示模型/供应商/思考程度）。
  - 会话页：**常显输入容器**（与草稿同一个 `Composer`）；消息按群聊排布——用户在右（头像 + 昵称 + 灰底气泡），Agent 在左（头像 + **角色名** + 气泡：思考与回答同泡，运行中展开、回复完折叠），工具调用在泡内、错误红字；复制 / 编辑重发是**气泡下方的图标**（悬停出现），编辑=回填输入框（不改历史，核心没有单条消息删除）。
  - 及时展示：发送后立即插入本地用户气泡（`pendingUserText`），核心落库后由持久化消息替换。
  - 输入：`NSTextView` 包装——**Enter 发送、Shift+Enter 换行**；发送按钮在运行中切换为红色停止按钮（`acp_cancel`），运行/发送期间禁用再次发送。
  - 发送流程：`create_conversation`（**始终带 `team`**，单角色时 members 为空，用于存下 leader 以便界面显示角色名）→ `acp_connect`（`workingDir` = 项目路径，`roleId` = leader）→ `acp_prompt` → `/ws/events` attach；已有会话跟进时复用连接，换会话则 `acp_connect` 并带 `sessionId = external_id` 续接。
  - 流式：`content_delta` / `thinking` / `tool_call(_update)` / `turn_complete` / `status_changed` / `detached`；`turn_complete` 后重拉详情并清空 live 缓冲；运行中「停止」→ `acp_cancel`，且输入框禁用避免 409。
- Verify（已跑，真实 CLI）：临时库 `open_folder` → `create_conversation` → `acp_connect`（claude_code）→ WS attach → `acp_prompt("只回答一个字：好")`；收到 `snapshot`、`user_prompt_sent`、`user_message`、8×`thinking`、`content_delta:"好"`、`usage_update`、`turn_complete`、`conversation_status_changed`。xcodebuild 通过。
- 未做：权限审批（归 S6）、快照恢复（S5）、多客户端同会话。
- Files：`Domain/Ports/CoreEvents.swift`、`Core/WS/CoreEventStream.swift`、`Domain/RuntimeTimeline.swift`、`Core/HTTP/CoreClient+Acp.swift`、`Features/Conversation/{Composer,MessageList,ToolCallRow}.swift`。
- Steps：WS attach（token 协议头、`subscription_id`、`seq` 去重、`snapshot` 覆盖、`detached` 重 attach、1s→32s 退避）；`acp_connect` → `acp_prompt`；工具 / 用量 / 状态事件渲染；`acp_cancel`；409 `turn_in_progress` 当状态处理。
- 已知形状：`create_conversation` body 是 camelCase `{folderId, agentType, title}`（实测 snake_case 会 400 `missing field folderId`）。
- Verify：真实跑一轮（最小提示）；见流式与至少一个工具卡；取消后以核心状态为准；杀核心再拉起能自愈。
- Checkpoint：`seq` 去重与重连有可复现的手动验证记录。

### S5：状态恢复与会话快照

- Intent：任何时刻进会话都能拿到完整状态。
- Files：`Core/HTTP/CoreClient+Snapshot.swift`、`Domain/SessionSnapshot.swift`。
- Steps：`acp_get_session_snapshot_by_conversation` 作为进入会话的权威状态；与 attach 的 `snapshot` / `replay` 走同一个 `Domain` 映射。
- Verify：进行中切走再切回一致；重启壳后重进一致。

### S6：多 Agent 可见性与审批

- Intent：团队会话里看得见谁在说、谁被委派、卡在哪，能批准/拒绝。
- Files：`Domain/{TeamMember,ApprovalRequest}.swift`、`Core/HTTP/CoreClient+Permission.swift`、`Features/Conversation/DelegationCard.swift`、`Features/Inspector/`。
- Steps：委派卡片（成员 / 任务 / 状态）；阻塞原因就近显示；`acp_respond_permission` 就近弹。
- Verify：2 人团队会话跑一遍；批准与拒绝两条路径都过。

### S7：打包

- Intent：可分发。
- Files：`App/Packaging.md`、sidecar 进 `Contents/MacOS/` 的脚本。
- Verify：`codesign -dv --verbose=4` 与 `spctl -a -vv` 通过；干净环境首次启动能拉起核心并跑通一轮。

## Risks

- **非沙盒是硬约束**：核心要 spawn CLI 并读 `~/.claude` 等，App Sandbox 直接断链 → 只能 Developer ID + 公证，上不了 App Store。S7 必须在真机验证，不能只看构建成功。
- **60s 首连**：有的引擎握手 8–10s，核心探测上限 60s。壳的超时必须 ≥60s。
- **attach 协议细节**：帧的精确形状以 `web/ws_attach.rs` 的 `ClientMsg` / `ServerMsg` 为准。
- **sidecar 生命周期**：壳退出/崩溃要带走 sidecar，否则留下占端口的孤儿进程。
- **一次性默认值**：委派「默认开启」只在首次运行写一次；必须用标记保护，否则会把用户显式关闭的设置又打开。
- **同 bundle id 不能并行两个实例**：第二个进程无窗口、不触发场景任务；开发与测试都要用独立 bundle id 的副本。
- **链接器加速是可选项**：`lld` / `sccache` 未安装；收益不确定，不阻塞本计划。
- **删除 Next 前端需要顺序**：P1（退掉 Tauri + Next）必须等 AgentOS 覆盖到 S6 之后，否则丢能力。

## Done Criteria

- `AgentOS/` 可构建；`Features` 里不出现核心字段形状。
- 真实跑通：选工作区 → 选 Agent（或团队）→ 发任务 → 看见流式 / 工具 / 用量 / 委派 → 能取消 → 能审批。
- 杀掉核心再拉起，界面靠 `snapshot` / `replay` 自愈；重启壳后状态一致。
- 每次新增调用都在 `AgentOS/engine-api.md` 登记；`AGENTS/verification.md` 的对应项都跑过。
- 出一个签名并公证的非沙盒 app，能在干净环境首次启动。
