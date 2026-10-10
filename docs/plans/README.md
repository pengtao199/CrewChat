# 计划索引

本目录只放**正在执行**的计划。完成、放弃或被取代的计划移入 `archive/`，不要按归档文件里的 Current Facts 或未完成切片开工。

| 计划 | 状态 | 适用边界 |
| --- | --- | --- |
| [多 Agent 单会话](multi-agent-conversations.md) | In Progress | Guid 多选进入同一会话；内置管理员规划、委派、汇总。不重做已归档的协作循环 |
| [设置页：Agents、引擎、模型](settings-pages.md) | Implemented | 这三个设置页的界面和交互。代码已落地，浏览器逐项点击还没走完 |

## 归档

| 计划 | 归档原因 |
| --- | --- |
| [CrewChat 执行计划](archive/crewchat.md) | Slice 1–4 已跑通。协作循环不再当活跃任务 |
| [CLI 模型绑定](archive/cli-model-binding.md) | 已落地。九个 CLI 的对话都已实测，含 MiniMax Code |
| [cc-switch 替换 LiteLLM](archive/cc-switch-core-integration.md) | 换核已完成。用量、技能、MCP 的落点以 `AGENTS/truth-sources.md` 为准 |
| [多 Agent 协作核心](archive/multi-agent-collaboration.md) | 阶段 1 已在 `orchestration/group.py`。阶段 2、3 已由 `archive/crewchat.md` Slice 3、4 执行 |
| [CLI 经 cc-switch 反代：调研](archive/gateway-cli-routing-research.md) | 调研记录。实施已并入模型绑定，不再单独执行 |
