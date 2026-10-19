# AgentOS 壳（macOS 前端）

Updated: 2026-10-19 (Asia/Shanghai)
Status: Ready

## Goal（终态）

一个可日常使用的 macOS 原生客户端：用户在 AgentOS 里选项目与 Agent，发起任务，看见多个 Agent 谁在说、调了什么工具、花了多少，能随时取消与审批。**所有真相与执行都在核心（`codeg-server`）**，AgentOS 只渲染与交互。

判定达标：把核心当黑盒，Action 里不出现任何核心字段形状（`AGENTS/agentos-layout.md` 的硬约束）；杀掉核心再拉起，界面靠 `snapshot` / `replay` 自愈。

## Non-Goals

- 不做第二套 ACP / 不 spawn CLI（那是核心的事，见 `AGENTS/agentos-boundary.md`）。
- v1 不出花名册编辑、模型与密钥配置界面、内嵌终端 / 浏览器 / computer-use、forge / canvas / pets / office / automation、多语言界面。
- 不改核心的 Rust 行为（本计划只消费它的既有 HTTP/WS 契约）。
- 不做 iOS（本机跑不了 CLI 子进程；将来是另一条产品线）。

## Current Facts（2026-10-19，已核实）

**核心侧（只读事实，来自 `codeg/src-tauri/src`）**

- 二进制 `codeg-server` 由 `--no-default-features --features server-bin` 构建；本机已装一份在 `.data/codeg/bin/codeg-server`（67M），同目录 `web/`（93M）是 Next 静态导出。
- 环境变量：`CODEG_HOST`（**默认 `0.0.0.0`**）、`CODEG_PORT`（默认 3080）、`CODEG_TOKEN`、`CODEG_DATA_DIR`、`CODEG_HOME`、`CODEG_STATIC_DIR`。
- token：`CODEG_TOKEN` 非空即采用（不落库）；否则复用/生成并持久化到 DB。→ 壳自己生成即无需读核心数据库。
- 不传 `CODEG_STATIC_DIR` 也能起（`find_static_dir_standalone` 只打 warn）。
- HTTP：`POST /api/<command>`，命令名同 Tauri command（snake_case）；鉴权 `Authorization: Bearer <token>`。
- 错误体：`{ code(snake_case), message, detail?, i18n_key?, i18n_params? }`（`app_error.rs`）。
- WS：`GET /ws/events`，协议头 `Sec-WebSocket-Protocol: codeg-events, codeg-token.<base64url_nopad(token)>`；attach 协议帧 `snapshot` / `replay` / `event` / `detached` / `pong`，带 `subscription_id`，`event` 带 `envelope.seq`（`web/ws_attach.rs`、`web/ws.rs`）。
- ACP 探测上限 60s → 壳的 HTTP 超时基线 60s（TS 侧 `WEB_CALL_TIMEOUT_MS = 60_000`）。
- 参考实现（可读不可依赖）：`codeg/src/lib/transport/*.ts`、`codeg/src/lib/app-error.ts`。

**AgentOS 侧（当前只有规则，没有代码）**

- `AgentOS/` 下只有 `engine-api.md`（调用登记表，目前只登记 `health`）。
- 规则：`AGENTS/agentos-boundary.md`（边界与范围）、`agentos-layout.md`（分层契约）、`agentos-transport.md`（调用与事件）、`AGENTS/ui.md`（界面与文案）、`AGENTS/verification.md`（自检）。
- 参考骨架：`Reference/upstream/agentos-upstream/AgentOS/`（WorkbenchView / AgentRosterView / TeamRoomView / RunInspectorView / ModelServicesView），但它的状态全是 `return false` 的桩，只借布局形状，不借状态模型。

**构建环境**

- macOS / Apple Silicon，rustc 1.96.1；`lld`、`sccache` 均未安装（P0 的链接器加速是可选项，见「风险」）。
- 覆盖规则：`xcodebuild -project AgentOS/AgentOS.xcodeproj -scheme AgentOS build`。

## 开工拍板（默认值；无异议按默认执行）

| # | 决策点 | 默认 | 备选 |
| --- | --- | --- | --- |
| 1 | sidecar 来源 | 开发期用路径配置（默认探测 `.data/codeg/bin/codeg-server`，其次自己 `cargo build` 的产物）；发布期随包进 `Contents/MacOS/` | 只认随包一份 |
| 2 | 端口 | 先探 3080：有活的核心就直接连（复用用户已起的）；没有则选空闲端口并自己拉起 | 一律自己选端口 |
| 3 | token | 壳生成（`SecRandomCopyBytes`）→ 存 Keychain → 传 `CODEG_TOKEN` | 读核心持久化的 token（要读 DB，否决） |
| 4 | 数据目录 | 开发期指向现有 `.data/codeg`（复用真库真花名册）；发布期用 `~/Library/Application Support/AgentOS/core` | 一律独立目录 |
| 5 | 错误展示 | 先用核心的 `message`（英文） | 解析 `i18n_key` 自己翻（后置） |
| 6 | 界面语言 | 只 `zh-Hans`，String Catalog 登记 | 双语 |
| 7 | 首屏 | 直接工作台（项目 + 会话列表 + 会话流） | 欢迎页（否决） |
| 8 | 与 Next 工作台并行 | 不并行；AgentOS 是替代品，核心的 `CODEG_STATIC_DIR` 指向空目录 | 内嵌 WebView 做对照（否决） |

## 分层与命名（本计划新增代码的落点）

按 `AGENTS/agentos-layout.md`，本轮只落这四个位置：

```text
AgentOS/
├── AgentOS.xcodeproj
└── AgentOS/
    ├── App/                 场景、依赖装配、sidecar 生命周期接线
    ├── Domain/              领域类型 + Ports/（三个端口协议）
    │   └── Ports/           CoreCommands / CoreEvents / CoreLifecycle
    ├── Core/                端口实现 + Wire/（Core 前缀 DTO）
    │   ├── HTTP/            CoreClient（命令调用、错误解码）
    │   ├── WS/              CoreEventStream（attach、seq、重连）
    │   ├── Process/         SidecarProcess（拉起、探活、退出清理）
    │   └── Wire/            Core* DTO
    ├── Infrastructure/      KeychainStore、PreferencesStore、Log
    └── Features/            SessionList / SessionView / Inspector / Settings
```

`Features` 不得 `import Core`（编译期可查）。

## 步骤计划

### Slice 0：工程骨架 + 拉起并连上核心

- Intent：AgentOS 能启动、找到/拉起核心、完成 `health`，界面显示真实连接态。
- Files：`AgentOS.xcodeproj`、`App/`、`Domain/Ports/CoreLifecycle.swift`、`Core/Process/SidecarProcess.swift`、`Core/HTTP/CoreClient.swift`、`Infrastructure/KeychainStore.swift`、`Features/Settings/`（地址与 token 展示）。
- Steps：
  - 建 macOS App 工程（**非沙盒**，hardened runtime）；Info.plist 只加必要项。
  - `KeychainStore`：读/写/删 `core.token`；首次运行生成随机 token。
  - `SidecarProcess`：探 3080 → 活则连；否则选空闲端口，以 `CODEG_HOST=127.0.0.1`、`CODEG_PORT`、`CODEG_TOKEN`、`CODEG_DATA_DIR` 拉起，stdout/stderr 落日志；进程退出时清理。
  - `CoreClient.health()`：`POST /api/health`，60s 超时，错误解成壳内错误类型。
  - 界面：连接态（未启动 / 连接中 / 已连接 / 失败+可操作原因）。
  - 登记 `AgentOS/engine-api.md`：`health` → 端口 `CoreLifecycle`。
- Verify：`xcodebuild` 通过；启动壳，核心未跑时显示可操作错误，点「启动」后 `health` 返回并显示已连接；`lsof -i :<port>` 确认只监听 `127.0.0.1`。
- Checkpoint：确认没有把任何核心字段形状带进 `Features/`。

### Slice 1：会话列表与进入

- Intent：看见真实会话并可进入。
- Files：`Core/Wire/CoreConversationSummary.swift`、`Core/HTTP/CoreClient+Conversations.swift`、`Domain/Conversation.swift`、`Features/SessionList/`。
- Steps：`list_all_conversations` / `get_conversation` → 映射成 `Domain/Conversation`；侧栏渲染列表（标题、引擎、时间、状态）；选中进入详情。
- Verify：起核心（开发期指向 `.data/codeg` 真库），列表条数与核心返回一致；选中的会话详情字段对得上；空库不崩。
- Checkpoint：`Features` 里没有 `snake_case` 字段名。

### Slice 2：连接与一轮对话（流式 + 工具 + 取消）

- Intent：核心体验跑通——发一句，看见流式、工具调用、用量，并能取消。
- Files：`Domain/Ports/CoreCommands.swift`、`Core/HTTP/CoreClient+Acp.swift`、`Domain/Ports/CoreEvents.swift`、`Core/WS/CoreEventStream.swift`、`Domain/RuntimeEnvelope.swift`、`Features/SessionView/`。
- Steps：
  - `CoreEventStream`：WS 连接（带 token 协议头）、`attach`/`detach`、按 `subscription_id` 分派、按 `seq` 去重、`snapshot` 覆盖本地、`detached` 重 attach、指数退避重连（1s→32s）。
  - `acp_connect` → `acp_prompt`；工具调用 / 用量 / 状态事件渲染成卡片。
  - `acp_cancel` 接线；`turn_in_progress`（409）当状态处理，不弹错误。
  - 登记 `engine-api.md`：`acp_connect` / `acp_prompt` / `acp_cancel` + `/ws/events`。
- Verify：真实跑一轮（最小提示"用一句话回答"）；见流式增量与至少一个工具卡；取消后状态由核心回传为准；拔网/杀核心再拉起，界面自愈。
- Checkpoint：`seq` 去重与 reconnect 逻辑有可复现的手动验证记录。

### Slice 3：状态恢复与会话快照

- Intent：任何时刻进会话都能拿到完整状态，不靠界面猜。
- Files：`Core/HTTP/CoreClient+Snapshot.swift`、`Domain/SessionSnapshot.swift`。
- Steps：`acp_get_session_snapshot_by_conversation` 作为进入会话的权威状态；与 attach 的 `snapshot` / `replay` 对齐（同一映射函数）。
- Verify：会话进行中切走再切回，内容一致；重启壳后重进同一会话，内容一致。
- Checkpoint：快照与事件走同一个 `Domain` 映射，不存在两套解析。

### Slice 4：多 Agent 可见性与审批

- Intent：团队会话里看得见谁在说、谁被委派、卡在哪，能批准/拒绝。
- Files：`Domain/TeamMember.swift`、`Domain/ApprovalRequest.swift`、`Core/HTTP/CoreClient+Permission.swift`、`Features/SessionView/DelegationCard.swift`、`Features/Inspector/`。
- Steps：委派事件渲染成卡片（成员名 / 任务 / 状态）；阻塞原因与修复路径就近显示；`acp_respond_permission` 接线（就近弹，不做全局模态）。
- Verify：真机跑一个含 2 个成员的团队会话，能看到委派与成员身份；发一次权限请求并批准，观察后续事件；拒绝路径也走一遍。
- Checkpoint：审批与错误都挂在对应会话/工具调用上（`AGENTS/ui.md`）。

### Slice 5：花名册只读呈现 + 壳设置 + 打包

- Intent：只读看清 Agent 是谁；壳能自己配置；能出一个可分发的 app。
- Files：`Core/HTTP/CoreClient+Agents.swift`、`Features/Roster/`、`Features/Settings/`、`App/Packaging.md`（打包与签名步骤）。
- Steps：`acp_list_agents` → 花名册（名字、角色、引擎、模型、就绪态）；设置页：核心地址、token 重生成、数据目录、外观；sidecar 进 `Contents/MacOS/`，Developer ID 签名 + hardened runtime + 公证。
- Verify：`codesign -dv --verbose=4` 与 `spctl -a -vv` 通过；在一台干净机器（或新用户）上首次启动能拉起 sidecar 并跑通一轮对话。
- Checkpoint：花名册页只读，没有编辑入口（v1 边界）。

## Risks

- **非沙盒是硬约束**：核心要 spawn CLI 并读 `~/.claude` 等，App Sandbox 直接断链 → 只能 Developer ID + 公证，上不了 App Store。Slice 5 的打包必须在真机验证，不能只看构建成功。
- **60s 首连**：有的引擎握手 8–10s，核心探测上限 60s。壳的超时必须 ≥60s，否则表现为"没反应"。
- **attach 协议细节**：帧的精确形状要以 `web/ws_attach.rs` 的 `ClientMsg` / `ServerMsg` 为准（本计划的形状来自阅读，落地时逐字段对齐）。
- **sidecar 生命周期**：壳退出/崩溃要带走 sidecar，否则留下占用端口的孤儿进程；端口探测要能识别"已有活的核心"而不是盲目再拉一个。
- **链接器加速是可选项**：`lld` / `sccache` 本机未安装；装它们要动网络与工具链，且 macOS 上 `-fuse-ld=lld` 有兼容风险。收益不确定，**不阻塞本计划**；要做就单独做并在 `codeg/NOTICE.md` 记录。
- **删除 Next 前端需要顺序**：P1（退掉 Tauri + Next）必须等 AgentOS 覆盖到 Slice 4 之后，否则丢能力。

## Done Criteria

- `AgentOS/` 有可构建的工程，`Features` 里不出现核心字段形状。
- 真实跑通：选项目 → 选 Agent（或团队）→ 发任务 → 看见流式 / 工具 / 用量 / 委派 → 能取消 → 能审批。
- 杀掉核心再拉起，界面靠 `snapshot` / `replay` 自愈；重启壳后状态一致。
- 每次新增调用都在 `AgentOS/engine-api.md` 登记；`AGENTS/verification.md` 的对应项都跑过。
- 出一个签名并公证的非沙盒 app，能在干净环境首次启动。
