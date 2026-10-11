# Codeg / CCR 集成：单软件工作台（统一模型 + 协作 + 状态 + 用量）

Updated: 2026-10-11 (Asia/Shanghai)
Status: In Progress。源码固化、旧前端移除、Codeg 构建与测试验证已完成；运行联调与设置页集成未完。

## Goal（单软件终态）

在一个 Codeg 桌面应用里完成全部事情：

1. **直接对话开发**：会话内与任意 CLI Agent 协作（`@` 委派、并行子会话、worktree 任务）。
2. **统一配置模型**：一个「模型」设置页管理全部 provider / 密钥 / 路由（数据与写入都归 CCR）。
3. **配置 Agent**：每个 Agent（CLI）可直接看到并设置它用的模型、技能、MCP、权限。
4. **CLI 状态**：一个「引擎 / CLI」页显示：装没装、登录没登录、CCR profile 是否已应用、最近错误。
5. **用量分析**：请求级（provider / model / token / 成本）与会话级用量在同一处可见。

## 职责切分（唯一权威）

| 关注 | 权威 | 在 Codeg 内的呈现 |
| --- | --- | --- |
| 供应商 / 密钥 / 路由 / 回退 | `ccr/` | 模型页（读写 CCR 本地 API） |
| 每个 CLI 用哪个模型 | `ccr/` 的 Agent Profile | Agent 页的「模型」列 |
| CLI 安装 / 登录 / 可用 | Codeg 探测 + CCR 探活 | 引擎 / CLI 状态页 |
| Agent 定义（角色 / 技能 / MCP / 权限） | Codeg | Agent 设置页 |
| 用量 | CCR 请求日志（请求级）+ Codeg 会话统计 | 用量页（合并展示） |
| 协作编排 | Codeg（`@` 委派 + Sub-agents） | 会话内 |
| Python 侧（`src/crewchat/`） | 待定 | 可选：编排作为自定义 ACP agent |

**禁止双写**：同一个 CLI 的模型配置只由 CCR 写；Codeg 自带的模型 provider 面板在我们 fork 里隐藏或改为只读呈现。

## 分阶段

- **P0 构建与验证**（已完成）：Codeg 前端 `pnpm build` 通过；全套测试通过（Node ≥ 25 的 Web Storage
  差异由 `[crewchat]` 兜底处理，见 `codeg/NOTICE.md`）；CCR 源码就位。
- **P1 模型层打通**：CCR 起网关；给 claude / codex / opencode / pi 应用 profile；验证 Codeg 拉起的 CLI 走 CCR；
  给 Gemini CLI / Hermes 补 profile（CCR 目前没有这两类 agent profile，需 `[crewchat]` 扩展或手动配置）。
- **P2 单软件设置整合**：Codeg 内新增或改造三页——模型（读写 CCR API）、引擎状态（Codeg 探测 + CCR）、
  用量（合并）；Agent 页接 CCR 的模型选择。原则：新增文件 + API 调用，尽量少改上游文件。
- **P3 协作纵深**：打开 delegation；把「实现 → 异模型审查」固化成 skills；评估 Python 编排以自定义 ACP agent 接入。
- **P4 打包分发**：Codeg 打包（dmg）；CCR 作为伴生进程随包（或首启引导安装）；首次运行向导
  （检测 CLI → 配 provider → 选模型 → 开 delegation）。

## Current Facts

- `codeg/`：v0.35.1 已固化；`pnpm install` / `build` / `lint` / 全套测试通过；`pnpm tauri dev` 桌面编译未跑。
- `ccr/`：v3.1.3 已固化；未运行、未配置。
- `ui/`（AionUi fork）已移除；Python 侧静态托管已删；旧计划已归档。
- 本机 CLI：claude / codex / opencode / gemini / pi / hermes。

## Next steps

1. 跑起 Codeg 桌面（`pnpm tauri dev`），确认界面可用（首次 Rust 编译较久）。
2. 起 CCR（`ccr ui` 或构建桌面版），配 provider，应用 agent profile。
3. 按 P1 验证一轮「Codeg 拉起 CLI → 走 CCR → 日志可见」。
4. 进 P2 做设置页集成。

## Done Criteria

- 一轮真实任务在 Codeg 内用 `@` 委派跨 CLI 完成，全程走 CCR 路由。
- 至少 3 个 CLI 在 Codeg 的同一处能看到「状态 + 模型」；用量能聚合到 provider / model。
- 旧前端无残留引用（归档与变更说明除外）。
