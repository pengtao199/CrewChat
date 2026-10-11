# Vendored: Claude Code Router (CCR)

本目录是 [claude-code-router](https://github.com/musistudio/claude-code-router) 的**完整源码**，
按需固化进本仓库，作为 CrewChat 的**统一模型管理层**基座，**不依赖外部克隆**。

- **Upstream**：`musistudio/claude-code-router`（MIT）
- **Vendored commit**：`057abaa1f95519ab8faf999e1ccef2b80ccdeb9e`（v3.1.3，2026-10-10）
- 许可见 `LICENSE`（原样保留）。

## 定位

- 本地模型网关与控制面：默认网关 `http://127.0.0.1:3456`，管理界面 `http://127.0.0.1:3458`。
- 为所有编码 Agent（Claude Code / Codex / OpenCode / Pi / Grok CLI / Kimi CLI 等）统一
  provider、模型、路由、密钥池、日志与用量（Providers → Server → Agent Config）。
- 与 `codeg/` 分工：**Codeg 管协作**（ACP 会话与 `@` 委派），**CCR 管模型**（端点与路由）。
- 与 `gateway/cc-switch/` 不冲突：cc-switch 服务于 CrewChat Python 侧的隔离式路由；
  CCR 面向全局 CLI 配置。同一个 CLI 不要让两边同时改它的配置。

## 取舍

| 内容 | 处理 | 原因 |
| --- | --- | --- |
| `packages/`（core / cli / electron / ui）、`build/`、`scripts/`、`docs/`、`extension/`、`docker/`、配置文件 | 固化 | 二次开发对象 |
| `.git/`（上游历史） | 不搬 | 与 cc-switch / codeg 一致；升级用上游快照 diff 重放 |
| `blog/`、`cdn/`（约 8MB 营销与 CDN 站点源码） | 不搬 | 与开发无关 |
| 构建产物（`node_modules/`、`dist/`、`release/`、`release-local/`、`logs/`、`tmp/` 等） | 由本目录自带 `.gitignore` 忽略 | 产物不入库 |

## 我们对上游做的改动

全部带 `[crewchat]` 标记，`rg '\[crewchat\]' ccr/` 可列出，便于升级时重放：

| 文件 | 改动 | 原因 |
| --- | --- | --- |
| （暂无） | —— | 尚未开始改动 |

## 构建与开发

前置：Node ≥ 22（本机 26.x）。桌面应用用 npm 构建：

```bash
cd ccr
npm ci
npm run build:app:mac        # 产出 release-local/ 下的 DMG/ZIP
```

CLI / 浏览器管理界面：

```bash
npm install -g @musistudio/claude-code-router
ccr ui                       # 管理界面 http://127.0.0.1:3458；网关 127.0.0.1:3456
```

网络提示：registry 直连超时时，按本项目惯例用国内镜像：

```bash
npm ci --registry=https://registry.npmmirror.com
```

## 升级上游

1. 记录新的 upstream commit（`musistudio/claude-code-router`）；
2. 以相同排除项（`.git/`、`blog/`、`cdn/`）获取源码快照，与本目录 `diff`；
3. 重放上表所有 `[crewchat]` 改动；
4. 更新本文件的 Vendored commit；
5. 跑它自己的测试确认仍可构建（命令以它 `package.json` 为准）。
