# 计划索引

本目录放**正在执行、调研中或明确暂停**的计划。完成、放弃或被取代的计划移入 `archive/`，不要按归档文件里的 Current Facts 或未完成切片开工。

| 计划 | 状态 | 适用边界 |
| --- | --- | --- |
| [Agents 页面与团队会话](agents-page-and-team.md) | Proposed | **核心设计**：Agents 页组装 / readiness / Test Run；单 Agent + Leader 中介团队、委派、团队快照、权限与事件归属；团队留在 cli-layer 的模块化单体，MCP 独立成层 |
| [Agents 核心协作](agents-core-collaboration.md) | Superseded | Codeg 时代的历史方案；保留作实现参考，不再作为运行时依据 |
| [AgentOS 壳（macOS 前端）](agentos-shell.md) | In Progress | `AgentOS/` 下的 SwiftUI 壳：拉起并消费 `codeg-server` 的 HTTP/WS 契约，落地会话 / 流式 / 委派可见性 / 审批 / 花名册与打包 |
| [Swift 原生运行时迁移](swift-runtime-migration.md) | Proposed | 去掉 `codeg-server` / CCR：Swift 自己拥有 ACP、持久化、模型网关、权限与编排；外部 CLI/adapter 保留。模型层已拆出，见下 |
| [模型层：移除 CCR](model-layer-remove-ccr.md) | In Progress | Rust 模型层 sidecar：控制面 3458 + 网关 3456、CCR 数据导入、Anthropic→OpenAI 翻译、并发多模型路由；模型层完成后建 Agents |
| [CLI Layer：移除 Codeg](cli-layer-migration.md) | In Progress | 新建 `cli-layer/` 取代 codeg：ACP 宿主、引擎/会话/权限/花名册/持久化；copy 官方 rust-sdk + OpenAB 的 ACP 部分，codeg 降级为只读参考与回退 |
| [cli-layer 架构](cli-layer-architecture.md) | Proposed | cli-layer 的模块化单体边界：api / app / domain / runtime / infra，port 隔离 model / skill / MCP；团队留在核心，资源域独立 |
| [Skill 层：统一管理](skill-layer.md) | Paused | 全局一份技能库；Agents 页按 Agent 选择；会话级 home 只软链选中的技能；后端核心已完成，等待 CLI 真实会话验收 |
| [MCP 层：统一管理](mcp-layer.md) | Research / Decision Draft | 最终形态是完整本地网关；复用 MCPJungle（vendor/fork）做运行时，`mcp-layer` 只做注册表、Agent token/allowlist 与 ACP 注入；不做旧数据兼容 |

## 归档

| 计划 | 归档原因 |
| --- | --- |
| [Codeg / CCR 集成](archive/codeg-ccr-integration.md) | 未完成续作并入「Agents 核心协作」（2026-10-12）；已完成部分（CCR 薄代理、模型页 v2、CLI 页就绪卡、scope 坑实测）保留记录 |
| [多 Agent 单会话](archive/multi-agent-conversations.md) | 协作界面迁到 `codeg/`；Python 编排若以自定义 ACP agent 复活，另开计划 |
| [权限系统](archive/permissions.md) | Python 侧权限管道已退役（2026-10-11；审批归 Codeg，自跑身体兜底放行） |
| [设置页：Agents、引擎、模型](archive/settings-pages.md) | 旧前端（`ui/`）已移除，页面基座迁到 `codeg/` |
| [CLI 输出噪音](archive/cli-output-noise.md) | 旧前端已移除；若 Python 身体复现同类噪音再开 |
| [CrewChat 执行计划](archive/crewchat.md) | Slice 1–4 已跑通。协作循环不再当活跃任务 |
| [CLI 模型绑定](archive/cli-model-binding.md) | 已落地。九个 CLI 的对话都已实测，含 MiniMax Code |
| [cc-switch 替换 LiteLLM](archive/cc-switch-core-integration.md) | 换核已完成（该网关后由 CCR 取代并移除）。用量、技能、MCP 的落点以 `AGENTS/truth-sources.md` 为准 |
| [多 Agent 协作核心](archive/multi-agent-collaboration.md) | 阶段 1 已在 `orchestration/group.py`。阶段 2、3 已由 `archive/crewchat.md` Slice 3、4 执行 |
| [CLI 经 cc-switch 反代：调研](archive/gateway-cli-routing-research.md) | 调研记录。实施已并入模型绑定，随后网关被 CCR 取代并移除 |
