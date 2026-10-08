# 来源与改动说明

本目录的前端代码 fork 自 [AionUi](https://github.com/iOfficeAI/AionUi)（Apache-2.0），
遵循其许可证保留原始版权声明。`LICENSE` 为其原始许可证副本。

## 我们改了什么

- `packages/desktop/src/renderer` 与 `common`：原样取出，作为独立 web 应用构建
- 移除 Electron 主进程目标，改由 `vite.config.ts` 只构建 renderer
- `electron` / `@sentry/electron/renderer` 在 web 构建下指向 `stubs/` 中的空实现
  （上游调用点本就有 `window.electronAPI` 守卫）
- 依赖从 155 个精简到 52 + 10（移除 Electron、机器人、Office、Web 服务端、测试工具等
  与保留页面无关的部分）
- 开发期由 Vite proxy 将 `/api` 与 `/ws` 转发到我们的 Python 后端（生产由后端直接服务静态文件）

## 改动过的上游文件

按 `AGENTS/upstream.md`，对上游文件的改动均带 `[crewchat]` 注释，可用 `rg "\[crewchat\]" ui/` 定位：

| 文件 | 改动 |
| --- | --- |
| `renderer/components/layout/Router.tsx` | 新增 `/settings/agents` 路由；默认重定向改到它 |
| `renderer/pages/settings/components/SettingsSider.tsx` | 导航新增 `agents`（Agent 编排），原 `agent` 改名 `cli`（CLI 身体） |
| `renderer/pages/settings/components/SettingsPageWrapper.tsx` | 同步上一条（上游在此重复定义导航，漏改会导致整页崩溃） |

新增文件（不属于上游）：

- `vite.config.ts`、`stubs/`（web 构建与 Electron 隔离）
- `scripts/generate-i18n-types.js`（取自上游，用于生成 i18n key 类型）
- `renderer/pages/settings/AgentComposer/{index.tsx,AgentComposerContent.tsx,api.ts,types.ts}`
- `renderer/components/base/{SettingsCardList,SettingsCardRow,StatusBadge,EmptyState}.tsx`
- `renderer/components/settings/DataLocationNote.tsx`

## 上游同步

参考仓库以 sparse clone 放在 `Reference/`（已 gitignore）。升级时按上表逐条重放，不整体覆盖。
