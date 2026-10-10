# 计划索引

本目录存放进行中的实施方案。计划完成、放弃或被取代后，应移入 `archive/` 并把本表相应行更新为归档状态（见 `AGENTS.md` 第 6 条）。

| 计划 | 状态 | 适用边界 |
| --- | --- | --- |
| [多 Agent 协作核心](multi-agent-collaboration.md) | 阶段 1 已落地 | 共享上下文在 `orchestration/group.py`；路由、委派、规划者未做 |
| [用 cc-switch 核心替换 LiteLLM](cc-switch-core-integration.md) | 进行中 | 网关换核（headless `ccsw-proxy`）、读 cc-switch DB 接六类数据、provider 切换、CLI 扫描 |
| [CLI 模型绑定](cli-model-binding.md) | 已落地（OpenCode 逐请求未验证） | 按进程写 Claude / Codex / OpenCode 配置，网关按模型名转发，不改 `~/.claude` 与 `~/.codex` |
| [所有 CLI 经 cc-switch 反代：调研](gateway-cli-routing-research.md) | 调研记录 | 官方分档与 opencode 实测；实施不跟本文，跟模型绑定计划 |
