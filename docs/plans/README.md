# 计划索引

本目录只放**正在执行**的计划。完成、放弃或被取代的计划移入 `archive/`，不要按归档文件里的 Current Facts 或未完成切片开工。

| 计划 | 状态 | 适用边界 |
| --- | --- | --- |
| [Codeg / CCR 集成](codeg-ccr-integration.md) | In Progress | 工作台换成 `codeg/`、模型统一管理用 `ccr/`；源码固化、旧前端与旧网关（cc-switch）移除已完成；引擎状态页、用量页未完 |

## 归档

| 计划 | 归档原因 |
| --- | --- |
| [多 Agent 单会话](archive/multi-agent-conversations.md) | 协作界面迁到 `codeg/`；Python 编排若以自定义 ACP agent 复活，另开计划 |
| [权限系统](archive/permissions.md) | Python 侧权限管道已退役（2026-10-11；审批归 Codeg，自跑身体兜底放行） |
| [设置页：Agents、引擎、模型](archive/settings-pages.md) | 旧前端（`ui/`）已移除，页面基座迁到 `codeg/` |
| [CLI 输出噪音](archive/cli-output-noise.md) | 旧前端已移除；若 Python 身体复现同类噪音再开 |
| [CrewChat 执行计划](archive/crewchat.md) | Slice 1–4 已跑通。协作循环不再当活跃任务 |
| [CLI 模型绑定](archive/cli-model-binding.md) | 已落地。九个 CLI 的对话都已实测，含 MiniMax Code |
| [cc-switch 替换 LiteLLM](archive/cc-switch-core-integration.md) | 换核已完成（该网关后由 CCR 取代并移除）。用量、技能、MCP 的落点以 `AGENTS/truth-sources.md` 为准 |
| [多 Agent 协作核心](archive/multi-agent-collaboration.md) | 阶段 1 已在 `orchestration/group.py`。阶段 2、3 已由 `archive/crewchat.md` Slice 3、4 执行 |
| [CLI 经 cc-switch 反代：调研](archive/gateway-cli-routing-research.md) | 调研记录。实施已并入模型绑定，随后网关被 CCR 取代并移除 |
