# Codeg / CCR 集成：单软件工作台（统一模型 + 协作 + 状态 + 用量）

Updated: 2026-10-11 (Asia/Shanghai)
Status: In Progress。S1（CCR 薄代理）与 S2（模型页）已完成并提交；Agent 绑定页与用量页待做。

## Goal（单软件终态）

在一个 Codeg 桌面应用里完成全部事情：

1. **直接对话开发**：会话内与任意 CLI Agent 协作（`@` 委派、并行子会话、worktree 任务）。
2. **统一配置模型**：一个「模型」设置页管理全部 provider / 密钥 / 路由（内核是 CCR，UI 是 Codeg 风格）。
3. **配置 Agent**：每个 Agent（CLI）可直接看到并设置它用的模型、技能、MCP、权限（单独页面，形态待定）。
4. **CLI 状态**：一个「引擎 / CLI」页显示：装没装、登录没登录、CCR profile 是否已应用、最近错误。
5. **用量分析**：请求级（provider / model / token / 成本）与会话级用量在同一处可见。

## 职责切分（唯一权威）

| 关注 | 权威 | 在 Codeg 内的呈现 |
| --- | --- | --- |
| 供应商 / 密钥 / 路由 / 回退 | `ccr/` | 模型页（读写 CCR 本地 API） |
| 每个 CLI 用哪个模型 | `ccr/` 的 Agent Profile | 模型页展示；绑定管理归未来的 Agent 页 |
| CLI 安装 / 登录 / 可用 | Codeg 探测 + CCR 探活 | 引擎 / CLI 状态页（待做） |
| Agent 定义（角色 / 技能 / MCP / 权限） | Codeg | Agent 设置页 |
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
  - **模型页（已完成，commit `30044d5`）**：状态条 + 供应商增删改 + 模型清单 + 说明；
    旧 Codeg 模型管理 UI 已删除（清单见 `codeg/NOTICE.md`）；数据全部走 `ccr_call`。
  - 引擎状态页、用量页（待做）。
- **P3 协作纵深**：打开 delegation；把「实现 → 异模型审查」固化成 skills；评估 Python 编排以自定义 ACP agent 接入。
- **P4 打包分发**：Codeg 打包（dmg）+ CCR 伴生进程管理（随机 token 注入）；首次运行向导。

## Current Facts

- `codeg/`：v0.35.1 已固化；构建与全套测试通过；`[crewchat]` 改动：CCR 薄代理（`ccr_call` / `ccr_health`）、
  模型页（`CcrModelSettings`）、测试环境 Web Storage 兜底；server 模式运行中（3080，静态目录 = `codeg/out`）。
- `ccr/`：v3.1.3 已固化；开发实例运行中（CLI 版，装在 `.data/ccr/`；token 在 `.data/ccr/dev-token`）。
  当前 provider：OpenCode Go (Chat Completions)；profile：claude-code / codex 绑定 `mimo-v2.6-pro`，grok 未绑定。
- 链路验证（2026-10-11）：`claude -p` 与 `codex exec` 均经 CCR 返回「正常」；请求日志
  （`observability.requestLogs=true`）可经 `getRequestLogs` 读到：model / provider / status / tokens。
- 模型页已验证：eslint / i18n 一致性 / next build 通过；`/settings/model-providers` 200。
- `ui/`（AionUi fork）已移除；Python 侧静态托管已删；旧计划已归档。
- 本机 CLI：claude / codex / opencode / gemini / pi / hermes。

## Next steps

1. （待做）经 Codeg 会话拉起 CLI，确认同样走 CCR（会话级验证）。
2. （进行中）Agent 绑定页形态决策：单独页 / 沿用协作界面。
3. 引擎状态页与用量页。

## Done Criteria

- 一轮真实任务在 Codeg 内用 `@` 委派跨 CLI 完成，全程走 CCR 路由。
- 模型页可完成：看状态、加/改/删供应商、浏览模型；（绑定在 Agent 页完成后闭环）。
- 用量能聚合到 provider / model。
- 旧前端无残留引用（归档与变更说明除外）。
