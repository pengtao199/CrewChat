# AgentOS 壳（macOS 前端）

Updated: 2026-10-19 (Asia/Shanghai)
Status: In Progress（S0 ✅；S1 ✅；S2 待开工）

## Goal（终态）

一个可日常使用的 macOS 原生客户端：用户在 AgentOS 里选工作区与 Agent，发起任务，看见多个 Agent 谁在说、调了什么工具、花了多少，能随时取消与审批。**所有真相与执行都在核心（`codeg-server`）**，AgentOS 只渲染与交互。

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
- 设置 CRUD 齐备：模型供应商 `list/create/update/delete_model_provider`；花名册 `agent_roles_list/save/delete` + `agent_role_model_face`；MCP `mcp_*`；技能 `acp_*_agent_skill` / `custom_*_skill`；快捷短语 `quick_messages_*`；语言 `get/update_system_language_settings`。

**AgentOS 侧**

- S0 已完成：工程、分层骨架、sidecar 生命周期、token、`health`。
- `AgentOS/engine-api.md` 目前登记 `health`。
- 规则：`AGENTS/agentos-boundary.md`、`agentos-layout.md`、`agentos-transport.md`、`AGENTS/ui.md`、`AGENTS/verification.md`。

## 开工拍板（默认值；无异议按默认执行）

| # | 决策点 | 默认 | 备选 |
| --- | --- | --- | --- |
| 1 | sidecar 来源 | 开发期用路径配置（默认探测 `.data/codeg/bin/codeg-server`，其次自己 `cargo build` 的产物）；发布期随包进 `Contents/MacOS/` | 只认随包一份 |
| 2 | 端口 | **总是自选空闲端口并自拉核心**（3080 空闲就用，否则取临时端口）。不复用已起的核心：它的 token 只在它自己的库里 | 让用户粘贴别人的 token |
| 3 | token | 壳生成（`SecRandomCopyBytes`）→ 存 Keychain → 传 `CODEG_TOKEN` | 读核心持久化的 token（否决） |
| 4 | 数据目录 | 默认 `~/Library/Application Support/AgentOS/core`（隔离）；设置里可指到现有 `.data/codeg` 复用真库 | 一律独立目录 |
| 5 | 错误展示 | 先用核心的 `message` | 解析 `i18n_key` 自己翻（后置） |
| 6 | 界面语言 | 只 `zh-Hans`，String Catalog 登记 | 双语 |
| 7 | 首屏 | 直接工作台（工作区侧栏 + 主对话区） | 欢迎页（否决） |
| 8 | 与 Next 工作台并行 | 不并行；AgentOS 是替代品，核心的 `CODEG_STATIC_DIR` 指向空目录 | 内嵌 WebView 做对照（否决） |
| 9 | 主界面布局 | **两栏**：左侧可折叠工作区（`Section(isExpanded:)`，初始全折叠）+ 右侧主对话区；`.inspector` 默认隐藏、记住上次开合（⌘⌥I） | 三栏固定（否决） |
| 10 | 委派开关 | 放「核心」页；**默认开启**（首次运行一次性写入核心，落「已初始化」标记，之后以核心为准） | 跟随核心默认（false） |
| 11 | 设置页 | 6 个 tab：通用 / 核心 / 模型 / Agents / 能力 / 关于；每项对应已登记的核心命令 | 只做壳自身设置（否决） |

## 设置与主界面（S1–S3 的落点）

**设置 6 个 tab**（展示与编辑在壳，逻辑在核心）：

| Tab | 内容 | 核心命令 |
| --- | --- | --- |
| 通用 | 外观、启动行为（自动拉起 / 恢复上次工作区） | 壳本地 |
| 核心 | 二进制路径、数据目录、端口策略、重启、打开日志 / 目录、**委派开关（默认开启）** | 壳本地 + `CoreLifecycle` + `get/set_delegation_settings` |
| 模型 | 供应商增删改、密钥、默认模型 | `list/create/update/delete_model_provider` |
| Agents | 花名册增删改：角色 / 引擎 / 模型 / 技能 / MCP / 权限 | `agent_roles_list/save/delete` + `agent_role_model_face` |
| 能力 | MCP 服务、技能包、快捷短语 | `mcp_*` / `*_skill` / `quick_messages_*` |
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
    ├── Infrastructure/      KeychainStore、Preferences
    └── Features/            Workbench（侧栏 + 主栏）/ Settings / Inspector
```

`Features` 不得 `import Core`（编译期可查）。

## 步骤计划

### S0（✅ 2026-10-19）：工程骨架 + 拉起并连上核心

- Intent：能启动、找到/拉起核心、完成 `health`，界面显示真实连接态。
- 已落地：Xcode 工程（objectVersion 77，文件系统同步组）；非沙盒 + hardened runtime；`Domain/CoreLifecycle` 端口；token（Keychain）+ 端口探测 + sidecar 生命周期 + 60s 轮询 `health`；`Features/Connection`；退出清理。
- Verify（已跑）：`xcodebuild` 通过；`lsof` 只见 `127.0.0.1:3080`；`/api/health` 200；核心库无 `web_service_token`（token 来自壳）；quit 后子进程退出。
- Checkpoint：`Features` 里只有 `CorePhase` / `CoreEndpoint` 两个领域类型。

### S1（✅ 2026-10-19）（✅ 2026-10-19）（✅ 2026-10-19）：设置基建（通用 / 核心 / 关于）

- Intent：⌘, 能改外观与核心配置；委派开关落到核心；关于页说真话。
- 已落地
  - 委派开关：`get/set_delegation_set，改动后提示（启gs` 走 `CoreSettings` 端口； → 重新读设置首次连接时由，三页与主窗口同时生效）；启动时自动拉起核心（`@AppStorage("core.autoStart")`，默认开） `applyDefaultsIfNeeded()` 一次性写入 `enabled=true` 并落 `UserDefaults("core.delegationInitialized")`；之后只写核心。断核时开关禁用并显示原因。
  - 关于页：壳版本 //set build、核心版本（来自 ，改动后提示需重启alth走志。CorStg` 端口；首次连接时由 → 重新读设置 `applyDfaulIfNeeded()`一次性写入`=UseDfaults断核时开关禁用并显示原因。
  - `engine //set biidd记 `get来自 /egaleh走CorStg` 端口；首次连接时由 `applyDfaulIfNeeded()`一次性写入`=UseDfaults断核时开关禁用并显示原因。
  - TERM 转为 / b出iDd记 patc来自 /ocal`h `NSA链接。
  -pTERM.转为优雅退出（tDi 登记 p），c/sethS掉urciS`g al下孤儿核心NAp。
  -pSIGTERMt转为优雅退出（rDimp掉 chSkurc后S孤g儿al的问题NApp.trminae`），修掉 `kll 后留下孤儿核心的问题
- Verify（已跑）（
  - `xcodebuild` 通过。
  - 用独立 bundle id）的副本启动（避与原实例的冲突）：3080 被占 → 自动选 **62499** 并拉起第二个核心`/tmp/agentos-s1-core/codeg.db` 的 `app_metadata` 出现 `delegation.enabled=true`（首次默认开启）`depth_limit=1` / completed_cace_max_agent_dfus` 原样保留。
  - `kill -TERM后测试实例的立即退出，另一个心受影响
    未做i` 窗口的视觉走查（没有屏幕录制许可，无法截图）；三页的表单布局按系统 `Form` 默认样式。
- 已知行为：macOS 不会给**同 bundle id 的第二实例**创建窗口（进程存在但无场景，`.task` 不触发）。开发时不要让 Xcode 里的实例与行实例同时跑；测试用独立 bundle id 副
  - 用独立 bundle id）的副本启动（避与原实例的冲突）：3080 被占 → 自动选 **62499** 并拉起第二个核心`/tmp/agentos-s1-core/codeg.db` 的 `app_metadata` 出现 `delegation.enabled=true`（首次默认开启）`depth_limit=1` / completed_cace_max_agent_dfus` 原样保留。
  - `kill -TERM后测试实例的立即退出，另一个心受影响
    未做i` 窗口的视觉走查（没有屏幕录制许可，无法截图）；三页的表单布局按系统 `Form` 默认样式。
- 已知行为：macOS 不会给**同 bundle id 的第二实例**创建窗口（进程存在但无场景，`.task` 不触发）。开发时不要让 Xcode 里的实例与行实例同时跑；测试用独立 bundle id 副
  - 用独立 bundle id 的副本启动（避开与原实例的冲突）：3080 被占 → 自动选 **62499** 并拉起第二个核心；`/tmp/agentos-s1-core/codeg.db` 的 `app_metadata` 出现 `delegation.enabled=true`（首次默认开启生效），`depth_limit=1` / `completed_cache_max_mb` 原样保留。
  - `kill -TERM` 后测试实例的核心立即退出，另一个核心不受影响。
  - 未做：设置窗口的视觉走查（没有屏幕录制许可，无法截图）；三页的表单布局按系统 `Form` 默认样式。
- 已知行为：macOS 不会给**同一 bundle id 的第二个实例**创建窗口（进程存在但无场景，`.task` 不触发）。开发时不要让 Xcode 里的实例与命令行实例同时跑；测试用独立 bundle id 的副本。

### S2：两栏壳（工作区折叠侧栏 + 会话 + 只读主栏）

- Intent：真实以文件夹为工作区；工作区内可见会话；点开能读。
- Files：`Domain/{Workspace,Conversation,ConversationKind,ConversationStatus}.swift`、`Domain/Ports/{CoreWorkspaces,CoreConversations}.swift`、`Core/Wire/{CoreFolderDetail,CoreConversationSummary}.swift`、`Core/HTTP/CoreClient+Workspaces.swift`、`Core/HTTP/CoreClient+Conversations.swift`、`Features/Workbench/{WorkbenchView,WorkbenchViewModel,WorkspaceSidebar,ConversationList,ConversationDetailView}.swift`。
- Steps：
  - 侧栏：`List(selection:)` + 每个工作区一个 `Section(isExpanded:)`（`@SceneStorage` 记住折叠）；工具栏「添加工作区」→ `NSOpenPanel` → `open_folder`；右键「在 Finder 中显示」「从工作区移除」（`confirmationDialog` 写明只移出列表）。
  - 会话：一次 `list_all_conversations` 拉全量，按 `folderId` 分组；`.searchable` 本地过滤；空态 `ContentUnavailableView`。
  - 主栏：`get_folder_conversation` → 只读渲染 turns（用户 / Agent 文本 + 工具名）；没选会话时的空态。
  - inspector 骨架：`.inspector` + 工具栏切换 + `@AppStorage` 记住开合；先放会话信息与项目路径，用量等 S4 有 `sessionStats` 后接。
  - 登记 `engine-api.md`：`list_open_folder_details` / `open_folder` / `remove_folder_from_workspace` / `list_all_conversations` / `get_folder_conversation`。
- Verify：选真实文件夹 → 看到该文件夹会话；折叠状态重启后保留；移除后列表消失但目录仍在；空工作区、无工作区、加载失败三种态都有可操作提示。
- Checkpoint：`Features` 里没有 `snake_case`；枚举未知值走 `unknown` 分支。

### S3：设置扩展（模型 / Agents / 能力）

- Intent：把核心已有的 CRUD 做成原生表单，不重造逻辑。
- Files：`Domain/Ports/CoreSettings.swift`（扩）、`Core/HTTP/CoreClient+Settings.swift`（扩）、`Features/Settings/{Models,Agents,Capabilities}/`。
- Steps：模型供应商列表 / 编辑 / 删除 + 密钥；花名册列表 / 编辑（角色、引擎、模型、技能、MCP、权限）；MCP 本地扫描与增删；技能列表 / 编辑；快捷短语 CRUD。每项保存后以核心回传值刷新。
- Verify：每个 CRUD 走一遍真实写入并重开设置确认持久；密钥不出现在壳的日志里；删除有 `confirmationDialog`。
- Checkpoint：设置页没有一条校验规则是壳自己发明的。

### S4：对话（发消息 + 流式 + 工具 + 取消）

- Intent：核心体验跑通。
- Files：`Domain/Ports/CoreEvents.swift`、`Core/WS/CoreEventStream.swift`、`Domain/RuntimeTimeline.swift`、`Core/HTTP/CoreClient+Acp.swift`、`Features/Conversation/{Composer,MessageList,ToolCallRow}.swift`。
- Steps：WS attach（token 协议头、`subscription_id`、`seq` 去重、`snapshot` 覆盖、`detached` 重 attach、1s→32s 退避）；`acp_connect` → `acp_prompt`；工具 / 用量 / 状态事件渲染；`acp_cancel`；409 `turn_in_progress` 当状态处理。
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
- **链接器加速是可选项**：`lld` / `sccache` 未安装；收益不确定，不阻塞本计划。
- **删除 Next 前端需要顺序**：P1（退掉 Tauri + Next）必须等 AgentOS 覆盖到 S6 之后，否则丢能力。

## Done Criteria

- `AgentOS/` 可构建；`Features` 里不出现核心字段形状。
- 真实跑通：选工作区 → 选 Agent（或团队）→ 发任务 → 看见流式 / 工具 / 用量 / 委派 → 能取消 → 能审批。
- 杀掉核心再拉起，界面靠 `snapshot` / `replay` 自愈；重启壳后状态一致。
- 每次新增调用都在 `AgentOS/engine-api.md` 登记；`AGENTS/verification.md` 的对应项都跑过。
- 出一个签名并公证的非沙盒 app，能在干净环境首次启动。
