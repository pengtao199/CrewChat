# Codeg / CCR 集成：换工作台与统一模型管理

Updated: 2026-10-11 (Asia/Shanghai)
Status: In Progress。源码固化与旧前端移除已完成；配置、联调与构建验证未完。

## Goal

协作界面由自研维护的 AionUi fork 换成 Codeg 基座（`codeg/`）的二次开发；模型统一管理由 CCR
基座（`ccr/`）承担。Python 侧（`src/crewchat/`）保留，作为「大脑池 + 编排」的既有能力，
后续再决定以自定义 ACP agent 或旁路服务的形式接入。

## Current Facts

- `codeg/`：已固化 Codeg v0.35.1 源码（upstream commit `e21eb6b`）；`pnpm install` 完成
  （pnpm 11.9.0 + 国内镜像）；构建验证进行中。取舍与升级见 `codeg/NOTICE.md`。
- `ccr/`：已固化 v3.1.3 源码（upstream commit `057abaa`）；尚未安装与配置。见 `ccr/NOTICE.md`。
- `ui/`（AionUi fork）已移除；Python 侧静态托管与相关注释已清理；三个旧计划已归档。
- `gateway/cc-switch/` 保持不变：服务于 Python 侧的隔离式路由；CCR 面向全局 CLI 配置，两者不冲突。

## Next steps

1. Codeg 构建验证（`pnpm lint` / `pnpm test` / `pnpm build`），按需修 `[crewchat]` 问题。
2. CCR 安装与配置：Providers → Server（`127.0.0.1:3456`）→ Agent Config 给每个 CLI 应用 profile。
3. Codeg 侧启用本机已装 CLI（claude / codex / opencode / gemini / pi / hermes），开启多 Agent 委派，
   跑一轮真实任务的 `@` 跨 CLI 协作。
4. 决定 Python 侧去向：自定义 ACP agent（把 `group.py` 的编排暴露给 Codeg）或旁路服务。

## Done Criteria

- Codeg 能从源码构建并跑起桌面端；一轮真实任务用 `@` 委派跨 CLI 完成。
- 至少 3 个 CLI 的模型走 CCR 统一路由，`Logs` 能看到解析后的 provider / model。
- 旧前端在仓库内无残留引用（`rg -i aionui` 无命中）。
