> 归档：2026-10-10。执行以 [../crewchat.md](../crewchat.md) 为准。下文是当时的记录，其中的 Current Facts 与未完成切片不要再当任务做。用量、技能、MCP 的落点以 `AGENTS/truth-sources.md` 为准，不再接 cc-switch 的库。

# 用 cc-switch 核心替换 LiteLLM 执行计划

Updated: 2026-10-09 (Asia/Shanghai)
Status: In Progress（Slice 1–3/5 已落地，Slice 4/6 未完成；模型页为计划外新增，见文末「进度」）

## Goal

把网关从 LiteLLM 换成 cc-switch 的代理核心，编译成**独立进程**（无 GUI 依赖）；并把 cc-switch 已有的
provider / MCP / Skill / Prompt / 用量数据接入我们的 app。我们的三层架构与解耦规则不变。

## Current Facts

目标仓库 `farion1231/cc-switch`：MIT、141k⭐、Rust + Tauri 2、活跃（当日有提交）。

- **代理核心与 Tauri 几乎解耦**：`proxy/handlers.rs`(4185 行)、`provider_router.rs`(642)、`model_mapper.rs`(428)
  对 tauri 的引用均为 0；`server.rs` 只有 2 处 —— `pub app_handle: Option<tauri::AppHandle>`，即**本来就是可选**。
  `proxy/mod.rs` 注释明确写着「公开导出给外部使用」。
- **DB 路径不依赖 Tauri**：`database/mod.rs:98` 注释「数据库文件位于 `~/.cc-switch/cc-switch.db`」，
  经 `dirs::home_dir()` 推导。
- **crate 已有 lib 目标**：`src-tauri/Cargo.toml` 里 `[lib] name = "cc_switch_lib"`，可追加 `[[bin]]`。
- 规模：277 个 `.rs` / 253,217 行 / 59 个依赖（含 tauri）。
- 本机 Rust **1.95.0 已安装**，与 `rust-toolchain.toml` 的 channel 一致。
- 代理端点：`/v1/messages`、`/v1/chat/completions`、`/v1/responses`（含 Codex WebSocket 握手）、
  `/v1/models`、`/health`、`/status`；**无本地鉴权**（只把调用方 auth 转发上游）。
- 现有数据（用户机器）：`providers` 10、`provider_endpoints` 6、`mcp_servers` 5、`skills` 49、
  `prompts` 2、`proxy_request_logs` 16,568、`usage_daily_rollups` 293、`model_pricing` 199。
- 路由粒度：**按 app_type**（每个 CLI 一个当前 provider + 故障转移队列），由
  `provider_router.select_providers(app_type)` 决定。解析链见
  `mode/current.rs::provider_in_use`：代理模式（`live-state.json` 的 `proxy_route`）优先，否则回落直连指针
  （`settings.json` 的 `current_provider`）。**冷启动时两者皆空 → 无路由**（`/v1/models` 空、`active_targets: []`）。
- **切换可脱离 GUI**：`store::AppState` 不持有 `AppHandle`（`AppState::new(db)` 即可构造），
  `pub mod controller` 导出 `enter_with_route` / `set_route` / `switch_route` / `exit` —— 我们可 headless 调用它的官方切换逻辑，
  **不必手写 `live-state.json`**。
- 已验证 headless 可运行：`cargo build --bin ccsw-proxy` 3m16s（debug）；启动后 `/health` 与 `/status` 均正常。
- 镜像实测（`git ls-remote` HEAD 与 GitHub 比对）：
  `gh-proxy.com` 4.2s **且最新** ✅ / 直连 4.3s 最新 / `gitclone.com` 0.8s 但 **缓存滞后** ❌ / `ghfast.top` 5.0s。
- 现有网关：`gateway/litellm.yaml`，监听 4000，由 `uv tool` 独立安装。

## Non-Goals

- 不 fork 它的前端（我们用自己那份 AionUi 前端）。
- 不重写它的代理逻辑；除 headless 必需外不改其源码。
- 不做 CLI 的安装/升级（写用户系统、风险高，且是它的强项 —— 我们只做只读扫描，见 Slice 7）。
- 不追求「每 Agent 独立 provider」：接受 app_type 粒度。`llm` 身体保留我们的大脑池（直连任意端点）。

## Approach

把代理编译成 headless 可执行文件，我们的 Python 后端启动并监督它；CLI 引擎与 `llm` 身体指向它；
六类管理数据**直接读它的 SQLite**（只读，第三方 schema，加校验与降级）。

仓库策略（已修订）：cc-switch 的 Rust 核心**已固化为本仓库 `gateway/cc-switch/src-tauri/`**
（只搬核心、不搬其前端），从仓库内构建，不依赖外部克隆；改动全部带 `[crewchat]` 标记，
升级方式见 `gateway/cc-switch/NOTICE.md`。这与最初「不 vendor」的判断相反 —— 结论是
**自包含、可复现构建优先于仓库体积**（`target/` 已 gitignore，不入库）。

## Work Slices

### Slice 1：fork + headless 二进制

- Intent：得到不依赖 GUI 的 `ccsw-proxy`。
- 位置：`~/Developer/cc-switch-core`（gh-proxy 浅克隆 + 记录 upstream commit）。
- Steps：
  1. `git clone --filter=blob:none --sparse --depth 1 https://gh-proxy.com/https://github.com/farion1231/cc-switch.git`
  2. 追加 `[[bin]] name = "ccsw-proxy" path = "src/bin/ccsw-proxy.rs"`，实现：打开
     `~/.cc-switch/cc-switch.db` → `ProxyServer::new(db, …, None)` → `start()`
  3. `cargo build --release --bin ccsw-proxy`
  4. 在 `ccsw-core.patch.md` 记录：upstream commit、改动文件、升级时如何重放
- Verify：进程起来后 `curl :15721/health` 返回 200；`/v1/messages` 与 `/v1/responses` 各跑一次真实请求。
- Checkpoint：确认「无 GUI 也能代理」后再继续。

### Slice 2：Python 侧监督与健康检查

- Intent：我们的后端负责它的生命周期。
- Files：`src/crewchat/core/`（新增 gateway 客户端与监督器）、`src/crewchat/core/diagnostics.py`。
- Steps：
  1. 启动时探测 `127.0.0.1:15721/health`；不在则按配置路径拉起 `ccsw-proxy` 并等待就绪
  2. 健康状态并入 `diagnostics.snapshot()`，在引擎页与 CLI 页显示
  3. 退出时优雅停止（仅停止我们拉起的实例，不动用户自己开的 app）
- Verify：kill 掉进程后我们能否自动恢复；未安装时给出明确提示而不是静默失败。

### Slice 3：引擎指向代理 + 停用 LiteLLM

- Intent：实际切换网关。
- Files：`src/crewchat/executors/*`、`AGENTS/gateway.md`、`gateway/`（退役）。
- Steps：
  1. `codex`：provider 指向 `http://127.0.0.1:15721/v1`（`wire_api = "responses"`）
  2. `claude`：注入 `ANTHROPIC_BASE_URL=http://127.0.0.1:15721`
  3. `opencode`：provider baseURL 指向代理
  4. `llm` 身体：保留大脑池直连（不经代理）
  5. 退役 `gateway/litellm.yaml` 与相关启动步骤
- Verify：三个 CLI 引擎各跑一轮真实对话；`llm` 身体仍能直连。
- Checkpoint：**先验证再删 LiteLLM**，保留回滚路径。

### Slice 4：读 DB 接六类数据

- Intent：用量/MCP/Skill/Prompt/provider 用真实数据，而非我们自建。
- Files：`src/crewchat/core/ccsw_db.py`（只读客户端）、`api/` 对应路由。
- Steps：
  1. 只读连接 `~/.cc-switch/cc-switch.db`（`mode=ro`），封装 schema 校验（表名与列名）
  2. provider 列表 → 模型页；MCP/Skill/Prompt → 工具页与技能页
  3. `proxy_request_logs` + `usage_daily_rollups` + `model_pricing` → **新增用量页**（按天/按模型/按 provider）
  4. schema 不匹配时降级为「未接入」并提示，不崩
- Verify：用量页数字与 cc-switch GUI 一致（抽查同一天）。

### Slice 5：provider 切换

- Intent：在我们的界面里切当前通道，不用开它的 GUI。
- Steps：
  1. **走它的官方逻辑，不手写 `live-state.json`**：在 `ccsw-proxy` 里加一个薄控制面（子命令或本地 HTTP），
     内部用 `AppState::new(db)` + `mode::controller::{enter_with_route, set_route, exit}`（均已 `pub`、不依赖 Tauri）
  2. 界面上标注「此切换影响该 CLI 的所有 Agent」
- Verify：切换后 `/status` 的 `current_provider` 变化，且同一对话下一轮走到新 provider。

### Slice 6：CLI 薄扫描（它不落库的部分）

- Intent：引擎页显示安装/版本/更新。
- Steps：
  1. 静态注册表（工具 → 二进制名 / 包名 / 包管理器 / 版本参数）
  2. 本地探测用 `shutil.which` + `--version`；更新检查按需触发，用官方命令
     （`brew outdated --json=v2`、`npm outdated -g --json`、`uv tool list --outdated`）
  3. 结果缓存，不阻塞页面加载
- Verify：对 codex/claude/opencode 三个真实工具，版本与手敲 `--version` 一致。

### Slice 7：规则更新

- Files：`AGENTS/gateway.md`、`AGENTS/truth-sources.md`、`AGENTS/dependencies.md`、`AGENTS/architecture.md`。
- Steps：把「网关 = cc-switch 代理核心（独立进程）」写入 gateway.md；truth-sources 增补
  `~/.cc-switch/` 为**外部只读数据源**并说明其 schema 不由我们拥有；dependencies 记录 fork 与升级策略。
- Verify：规则描述与实现一致（`check_layers` 与文档复核）。

## Risks

1. **依赖常驻进程**：cc-switch 代理不在则所有 CLI 引擎不可用 → Slice 2 的健康检查与自启动是必需的，不是可选项。
2. **第三方私有 schema**：`~/.cc-switch/cc-switch.db` 随其版本演进 → 全部读取包在 schema 校验里，失配降级。
3. **上游演进**：141k⭐ 活跃项目，我们 fork 会与上游分叉 → 记录 upstream commit 与改动清单，升级按清单重放。
4. **首次编译耗时**：tauri 作为依赖会一起编译（预计 10–20 分钟），Slice 1 要预留时间。
5. **路由粒度变化**：CLI 身体的 provider 从「每 Agent」变为「每 CLI 当前通道」，界面文案必须说清，否则用户困惑。
6. **回滚**：Slice 3 完成前 LiteLLM 保持可用；完成后保留 `gateway/` 一个版本周期再删。

## Done Criteria

- `ccsw-proxy` 可独立启动并通过健康检查；杀掉后我们能自动恢复或明确报错。
- 三个 CLI 引擎经代理完成真实对话；`llm` 身体仍走大脑池。
- 用量页显示真实历史数据（与 cc-switch GUI 抽查一致）。
- LiteLLM 不再参与运行路径，相关规则文件已更新。
- `uv run python scripts/check_layers.py` 与 `check_imports.py` 通过；新增模块已登记分层。

## 进度（对照实现，Updated: 2026-10-09）

| Slice | 状态 | 证据 / 说明 |
| --- | --- | --- |
| 1 headless 二进制 | 完成 | `gateway/cc-switch/`（vendor）+ `[[bin]] ccsw-proxy`，见 `NOTICE.md` |
| 2 Python 监督 | 完成 | `core/gateway_ccswitch.py`（`ensure_running` / `alive` / `proxy_up`）+ `api/crewchat_gateway.py`（start/stop） |
| 3 引擎指向代理 | 完成 | 网关适配器与控制面就绪；已退役并**删除 `gateway/litellm.yaml`** |
| 4 读 DB 接六类数据 | 部分 | usage：**口径已改为自研记账**（身体上报 → `.data/crewchat.db` `usage_events`，不读 cc-switch，只算本 app 发起的 CLI 身体）；provider 投影已接；MCP/Skill 仍读 `~/.agents` 约定、未接 cc-switch DB；Prompt 未接 |
| 5 provider 切换 | 完成 | `/api/crewchat/gateway/mode` → `Gateway.activate/deactivate`（走 cc-switch `mode::controller`） |
| 6 CLI 薄扫描 | 进行中 | `core/engines_ccswitch.py` + `api/crewchat_engines.py`；完成度待核 |
| 7 规则更新 | 完成 | `AGENTS/gateway.md`、`truth-sources.md`、`dependencies.md` 已改 |

计划外新增：**模型页**（provider/模型自动抓取、标签推导、批量启停），见提交 `e1293d2` 等。

下一步优先级：① MCP/Skill/Prompt 接入 cc-switch DB 或明确不做。（用量页已落地，口径为自研记账，见 Slice 4 行；LiteLLM 已删除。）
