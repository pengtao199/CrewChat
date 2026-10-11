# 界面（Codeg 基座）

## 适用范围

改动 `codeg/` 下的界面（Next.js + React + Tailwind + shadcn/ui）或其交互、文案、样式时。

## 背景

AionUi fork（原 `ui/`）已移除；协作界面改为 Codeg 的二次开发（`codeg/`）。
Codeg 自带开发规范：`codeg/AGENTS.md` 与 `codeg/CLAUDE.md`（构建、检查、测试命令以它们为准），
本文件只约束「我们的改动方式」。

## 我们的规则

- **上游优先**：能用上游已有能力或配置解决的不改代码；必要改动带 `[crewchat]` 标记，
  并登记到 `codeg/NOTICE.md` 的改动表。
- **国际化**：新增文案走它已有的 next-intl（`codeg/src/i18n/messages/*.json`）；
  改动时同步 zh-CN（默认中文界面）。
- **业务不出界**：模型与路由归 `ccr/`，编排归 Python 侧或 Codeg 自身；界面只做展示与交互。
- **密钥零容忍**：不把密钥、端点写死进前端。
- **可升级**：不整体重排上游目录与样式体系；不为单页另起一套视觉语言。

## 检查

- 改动是否带 `[crewchat]` 标记？`codeg/NOTICE.md` 是否同步？
- `cd codeg && npx -y pnpm@11.9.0 lint && npx -y pnpm@11.9.0 test` 过了吗？界面改动是否实际渲染看过？
- 中文文案是否和 en 一起更新？
