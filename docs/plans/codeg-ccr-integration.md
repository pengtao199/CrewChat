# Codeg / CCR 集成：单软件工作台（统一模型 + 协作 + 状态 + 用量）

Updated: 2026-10-11 (Asia/Shanghai)
Status: In Progress。S1–S4（CCR 薄代理、模型页 v2、逐行开关 + 双栏 picker）已完成并提交；**CLI 页就绪卡（安装/启用/CCR 绑定 + 高级折叠）已完成**（`agent-readiness-card.tsx` + `use-agent-ccr-binding.ts`，已渲染实测）；**CLI 启动链路（scope 坑）已实测**，结论见下；Agent 绑定页已决定不做；Agents 页就绪徽章、启动注入、引擎状态页、用量页待做。

## Goal（单软件终态）

在一个 Codeg 桌面应用里完成全部事情：

1. **直接对话开发**：会话内与任意 CLI Agent 协作（`@` 委派、并行子会话、worktree 任务）。
2. **统一配置模型**：一个「模型」设置页管理全部 provider / 密钥 / 路由（内核是 CCR，UI 是 Codeg 风格）。
3. **配置 Agent**：两层——每个 CLI 的全局设置（上游「智能体」页）＋本仓库核心的「Agents」花名册
   （角色 = `Soul.md` 人格 / `Agents.md` 规则 ＋ 绑定的身体、模型、本地 Skill 与 MCP）。
4. **CLI 状态**：一个「引擎 / CLI」页显示：装没装、登录没登录、CCR profile 是否已应用、最近错误。
5. **用量分析**：请求级（provider / model / token / 成本）与会话级用量在同一处可见。

## 职责切分（唯一权威）

| 关注 | 权威 | 在 Codeg 内的呈现 |
| --- | --- | --- |
| 供应商 / 密钥 / 路由 / 回退 | `ccr/` | 模型页（读写 CCR 本地 API） |
| 每个 CLI 用哪个模型 | `ccr/` 的 Agent Profile | 模型页与 CLI 页就绪卡展示（只读）；缺 profile 时的「绑定」写入也在 CLI 页；不做单独的 Agent 绑定页 |
| CLI 安装 / 登录 / 可用 | Codeg 探测 + CCR 探活 | 引擎 / CLI 状态页（待做） |
| Agent 定义（角色 / 技能 / MCP / 权限） | Codeg；角色花名册为本仓库「Agents」页（`agent-roles.json`） | Agent 设置页 ＋ Agents 页 |
| 权限审批 | Codeg（每 CLI 启动策略 + 权限卡） | 会话内（Python 侧已退役，兜底放行） |
| 用量 | CCR 请求日志（请求级）+ Codeg 会话统计 | 用量页（待做） |
| 协作编排 | Codeg（`@` 委派 + Sub-agents） | 会话内 |
| Python 侧（`src/crewchat/`） | 待定 | 可选：编排作为自定义 ACP agent |

**禁止双写**：同一个 CLI 的模型配置只由 CCR 写；Codeg 的 Agent 模型保持 Default，模型管理 UI 一律走
`ccr_call` 代理（不产生第二真相源）。

## 分阶段

- **P0 构建与验证**（已完成）：Codeg 构建 / 全套测试通过；CCR 源码就位。
- **P1 模型层打通**（已完成）：CCR 开发实例（`.data/ccr`，网关 3456 / 管理面 3458）；Claude Code 与
  Codex 的 profile 已绑定 `mimo-v2.6-pro` 并经真实 CLI 验证；请求日志已开启。
  opencode / pi 待补；Gemini CLI / Hermes 需在 CCR 补 profile（`[crewchat]` 扩展）。
- **P2 单软件设置整合**：
  - **模型页（已完成）**：状态条 + 供应商增删改（预设 / 凭据池 / 双栏选模型）+ 模型清单逐行开关
    + 订阅用量展示；数据全部走 `ccr_call`。旧 Codeg 模型管理 UI 已删除（清单见 `codeg/NOTICE.md`）。
    提交：`30044d5`（v1）→ `15f8fc1`（v2）→ `9791894`（逐行开关 + 双栏 picker）。
  - **CLI 页就绪卡（已完成，2026-10-11）**：CLI 详情面板的默认视图收敛为就绪卡——①安装（版本 /
    安装 / 升级）②启用 ③模型通（CCR 绑定四态：已绑定 / 可绑定 / 暂不支持 / 不可达）＋「默认档」
    只读回显 ＋ 重新应用 / 绑定到 CCR / 重试；原 checks、环境变量、各 CLI 配置管理、原生配置编辑器
    收入默认收起的「高级」（内容与草稿状态原样保留）。绑定写入只做三件事：建缺失 profile（`model`
    留空、`scope: ccr`，与 CCR UI 新建一致）、apply、重试；已有 profile（含 CCR UI 手建的）一律不打扰。
    逻辑拆分为 `agent-readiness-card.tsx` + `use-agent-ccr-binding.ts`。经 headless Chromium 实测：
    已绑定态（Claude Code）、暂不支持态（Gemini）、不可达→重试恢复，均截图留证（en / zh 两语言）。
  - **Agent 绑定页：不做**（用户确认意义不大；模型页只含状态条 + 供应商 + 模型 + 说明）。
  - 引擎状态页、用量页（待做）。
- **P3 协作纵深**：打开 delegation；把「实现 → 异模型审查」固化成 skills；评估 Python 编排以自定义 ACP agent 接入。
- **P4 打包分发**：Codeg 打包（dmg）+ CCR 伴生进程管理（随机 token 注入）；首次运行向导。

### CLI 启动链路实测（2026-10-11，「scope 坑」结论）

本机 CCR profile 全部为 `scope: "ccr"`（wrapper 模式）：apply 写
`~/.claude-code-router/profiles/<slug>/…` 并生成 wrapper 到 `~/.claude-code-router/bin/ccr-*-wrapper-*`。
**该 `bin/` 目录不在 PATH**，Codeg 按 PATH / 安装位置拉起各 CLI（`opencode`、`codex-acp`、`pi-acp`、
`grok-build`…），因此绕过 wrapper：

| CLI | Codeg 拉起时读到的配置 | 是否经 CCR |
| --- | --- | --- |
| claude | `~/.claude/settings.json`（含 `ANTHROPIC_BASE_URL=http://127.0.0.1:3456`，CCR apply 会同步已有网关 env） | ✅ |
| codex | `~/.codex/config.toml`（仍是自定义端点） | ❌ |
| opencode | `~/.config/opencode/opencode.json`（自定义端点；CCR profile 还处于 disabled） | ❌ |
| grok / kimi / pi / kilo | 各自默认配置（wrapper 才注入隔离 home / 模型参数） | ❌ |

→ 阶段 2「启动注入」二选一：把支持 global 的 profile 切全局接管（claude / codex / opencode / kilo），
或拉起时一并注入 CCR env / wrapper 环境（grok `GROK_HOME`、pi `PI_CODING_AGENT_DIR` /
`--provider claude-code-router`、kimi `KIMI_CODE_HOME`、claude/codex 的 BASE_URL/TOKEN 等）。
验收工具现成：CCR request-log。注意绑定卡显示的「已绑定」= CCR 侧档案就绪，Codeg 会话内生效依赖上述注入。

## Current Facts

- `codeg/`：v0.35.1 已固化；构建与全套测试通过；`[crewchat]` 改动：CCR 薄代理（`ccr_call` / `ccr_health`）、
  模型页（`CcrModelSettings`）、CLI 页就绪卡（`agent-readiness-card.tsx` / `use-agent-ccr-binding.ts`）、
  测试环境 Web Storage 兜底；server 模式可用（3080，静态目录 = `codeg/out`，本次渲染验证即用它——
  验证后进程已停）。
- `ccr/`：v3.1.3 已固化；开发实例（CLI 版，装在 `.data/ccr/`；token 在 `.data/ccr/dev-token`；home 为
  `~/.claude-code-router/`）。当前 provider：OpenCode Go (Chat Completions)；7 个 profile 均为
  `scope: ccr`（claude-code / codex / grok / kimi / pi / kilo / opencode[disabled]），默认档 `mimo-v2.6-flash`。
- 链路验证（2026-10-11）：`claude -p` 与 `codex exec` 均经 CCR 返回「正常」；请求日志
  （`observability.requestLogs=true`）可经 `getRequestLogs` 读到：model / provider / status / tokens。
- 模型页已验证：eslint / i18n 一致性 / next build 通过；`/settings/model-providers` 200。
- CLI 页就绪卡已验证：lint / 全量测试（602 文件 / 8903 用例）通过；headless Chromium 实测四态渲染
  （已绑定 / 暂不支持 / 不可达→重试恢复；en / zh-CN）与「高级」默认收起 / 展开。
`ui/`（AionUi fork）已移除；Python 侧静态托管已删；旧计划已归档；cc-switch 网关（Python 侧）已整体移除——身体不再注入模型，模型统一管理全走 CCR（见 `AGENTS/gateway.md`）。
- Python 客户端权限管道已退役（`core/permissions.py` / 确认端点 / `permission` 帧 / 开关，本地 `permissions.jsonc` 已删）；审批归 Codeg，自跑身体兜底放行。
- `[crewchat]`「Agents」花名册页已完成（设置 → Agents）：角色（`Soul.md` / `Agents.md`）绑定身体（CLI）、
  模型、本地 Skill 与 MCP；存储 `.data/codeg/agent-roles.json`（ID 为服务端 UUID）。按角色启动会话 /
  作为委派目标消费这些配置是下一步。
- 本机 CLI（页面探测）：claude / codex / opencode / gemini / pi / hermes / grok / openclaw；kimi、kilo 未安装（CCR 有它们的 profile，属于「绑定在、CLI 不在」）。

## Next steps

1. （紧接）Agents 页就绪徽章 + 未就绪引导：复用 CLI 页的绑定状态查询（`ccrGetProfiles` / `ccrAgentKindForBody`），
   花名册列表一眼看出哪些 Agent 就绪；选到未绑定 CLI 时模型区置灰并跳 CLI 页。
2. （阶段 2）启动注入 + request-log 端到端验收，见「CLI 启动链路实测」：先按 CLI 逐个验证注入通道
   （claude `ANTHROPIC_MODEL`、codex `-m`/config override、grok/pi/kimi 的 wrapper env 或切 global），
   再经 Codeg 会话拉起确认 request-log 有记录。
3. （待做）CLI 覆盖补齐：opencode / pi 的 CCR profile；Gemini CLI / Hermes 的 profile 连接器。
4. 引擎状态页与用量页。
5. （待做）把 Agents 花名册接入执行：按角色启动会话（身体 / 模型 / Skill / MCP 生效）并作为委派目标。

## Done Criteria

- 一轮真实任务在 Codeg 内用 `@` 委派跨 CLI 完成，全程走 CCR 路由。
- 模型页可完成：看状态、加/改/删供应商、浏览模型、逐行开关。
- CLI 页可完成：一眼看出每个 CLI 能否跑（安装 / 启用）与 CCR 绑定状态；缺 profile 可一键绑定；已有 profile 不被覆盖。
- 用量能聚合到 provider / model。
- 旧前端无残留引用（归档与变更说明除外）。
