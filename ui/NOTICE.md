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

## 上游同步

如需跟进上游修复，可在本仓库添加 remote 并 cherry-pick 相关提交。
