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
| `src-tauri/src/agent_host.rs`（新增） | Agent Host 配方 v1（Phase 0.3）：会话启动时从 CCR 读路由与模型——env 只做路由（BASE_URL 三件套 / discovery / compat / NO_PROXY），模型经 ACP `session/set_config_option` 通道（网关虚拟 ID，查 `/v1/models` 解析）；`CODEG_AGENT_HOST_INJECT=0` 完全回退 | codeg 以绝对路径拉起真实 CLI，绕过 CCR 的 wrapper；per-session 注入是"每角色用不同模型"（3.1）的前置件 |
| `src-tauri/src/acp/manager.rs`、`src-tauri/src/commands/acp.rs`、`src-tauri/src/lib.rs` | 注册 `agent_host` 模块；`spawn_agent` 合并配方 config_options（caller 显式值优先）；`build_runtime_env_for_agent` 合并配方 env（主会话与委派子代理共用） | 同上（实测笔记见 `docs/plans/agents-core-collaboration.md`） |

## 结构拆分登记（2026-10-12 起；规则见 `AGENTS/upstream.md`）

拆分 = 文件→目录 / 内嵌测试外移，行为与公共 API 不变（全量测试绿）。升级时把上游对新版单文件的改动应用到下表对应结构。

| 上游文件 | 本仓库结构 | 拆分内容 |
| --- | --- | --- |
| `src-tauri/src/acp/connection.rs` | `acp/connection/mod.rs` + `acp/connection/tests.rs` | 内嵌测试外移（16k 行 tests.rs） |
| `src-tauri/src/commands/acp.rs` | `commands/acp/mod.rs` + `tests.rs` | 尾部测试模块外移（6.6k 行）；中部 `diagnostics_tests` 留原位 |
| `src-tauri/src/acp/delegation/broker.rs` | `acp/delegation/broker/mod.rs` + `tests.rs` | 内嵌测试外移（5.2k 行） |
| `src-tauri/src/acp/manager.rs` | `acp/manager/mod.rs` + `tests.rs` | 内嵌测试外移（5.3k 行） |
| `src-tauri/src/commands/conversations.rs` | `commands/conversations/mod.rs` + `tests.rs` | 内嵌测试外移（4.0k 行） |
| `src-tauri/src/db/service/conversation_service.rs` | `db/service/conversation_service/mod.rs` + `tests.rs` | 内嵌测试外移（2.0k 行） |
| `src-tauri/src/acp/delegation/companion.rs` | `acp/delegation/companion/mod.rs` + `tests.rs` | 内嵌测试外移（3.1k 行） |
| `src-tauri/src/acp/delegation/tool_schema.json` | `acp/delegation/companion/tool_schema.json` | 随 companion 目录化移动（`include_str!` 相对路径） |
| `src/components/settings/acp-agent-settings.tsx` | `acp-agent-settings/index.tsx` + `agent-reorder-item.tsx` + `acp-agent-settings/helpers/*`（types / context / env / config-text / claude / gemini / opencode / cline / openclaw / grok / codex / codex-toml / hermes / deepseek / important-config / draft / version / checks） | 前端最大文件拆分（12.9k → 页面组件 8.7k + 18 个 helpers，全部 <1k；测试零改动 157 用例绿） |

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
  CODEG_MCP_BIN=$PWD/src-tauri/target/debug/codeg-mcp \
  ./src-tauri/target/debug/codeg-server
```

- `CCR_WEB_URL` / `CCR_WEB_AUTH_TOKEN`：模型页与就绪卡的 CCR 薄代理走这里。
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
