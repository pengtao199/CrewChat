# 计划索引

本目录存放进行中的实施方案。计划完成、放弃或被取代后，应移入 `archive/` 并把本表相应行更新为归档状态（见 `AGENTS.md` 第 6 条）。

| 计划 | 状态 | 适用边界 |
| --- | --- | --- |
| [多 Agent 协作核心](multi-agent-collaboration.md) | 计划中 | 会话内多 Agent 协作：共享上下文、路由、委派、规划者（L3 `orchestration/group.py`） |
| [用 cc-switch 核心替换 LiteLLM](cc-switch-core-integration.md) | 进行中 | 网关换核（headless `ccsw-proxy`）、读 cc-switch DB 接六类数据、provider 切换、CLI 扫描 |
