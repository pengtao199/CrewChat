# Vendored: Codeg

本目录是 [Codeg](https://github.com/spacering-net/codeg) 的**完整应用源码**，
按需固化进本仓库，作为协作工作台的二次开发基座，**不依赖外部克隆**。

- **Upstream**：`spacering-net/codeg`（Apache-2.0）
- **Vendored commit**：`e21eb6b38eea9405cdfa3b2d99df216d101d1344`（v0.35.1，2026-10-11）
- 许可见 `LICENSE`（原样保留）。
- 本目录的 `AGENTS.md` / `CLAUDE.md` 是**上游仓库自己的**开发指引（描述本目录的构建、检查方式），不是 CrewChat 的规则文件。

## 取舍

| 内容 | 处理 | 原因 |
| --- | --- | --- |
| 应用源码（`src/` Next.js、`src-tauri/` Rust/Tauri、`browser-agent/`、`public/`、`scripts/`、配置文件、`docs/` 文档） | 固化 | 二次开发的全部对象 |
| `.git/`（上游历史） | 不搬 | 与 cc-switch 的固化方式一致；升级用上游快照 diff 重放 |
| `docs/images/`（约 48MB 演示截图与 gif） | 不搬 | 与开发无关的体积 |
| 构建产物（`node_modules/`、`src-tauri/target/`、`.next/`、`out/`、`dist/`、`public/vs`、`src-tauri/binaries/`） | 由本目录自带的 `.gitignore`（含 `src-tauri/.gitignore`）忽略 | 产物不入库 |
| 测试夹具 `src-tauri/tests/fixtures/**/*.db` | 例外入库 | 是测试的组成部分；已在根 `.gitignore` 用 `!` 放行 |

## 二次开发的定位（记录当前决策）

- 本目录用于**替换 CrewChat 现有的 `ui/` 外壳**：协作界面采用 Codeg 的 `@` 委派、
  并行子会话、Sub-agents 面板、worktree 任务等形态，不再继续补齐 AionUi fork 的细节。
- **模型统一管理：UI 在本目录内、内核在 `ccr/`**。设置页「模型」按 Codeg 风格重做
  （`[crewchat]`），数据与动作全部经后端薄代理 `ccr_call` 访问 CCR；Codeg 自身
  **不写入**任何 CLI 的模型配置（对应 Agent 的模型一律留 Default）。
- 本仓库 Python 侧（`src/crewchat/`）的编排与网关内核暂时保留；若后续成为 Codeg 的
  自定义 ACP agent 或旁路服务，另立计划。

## 我们对上游做的改动

全部带 `[crewchat]` 标记，`rg '\[crewchat\]' codeg/` 可列出，便于升级时重放：

| 文件 | 改动 | 原因 |
| --- | --- | --- |
| `src/test-setup.ts` | 文件末尾追加 `[crewchat]` 兜底：Node ≥ 25 下为 `localStorage` / `sessionStorage` 提供可用的 `Storage` 实例 | 本机 Node 26 上 Node 自带 Web Storage 全局与 vitest/jsdom 冲突，751 个用例失败（Node 24 基线全绿）；上游 CI 固定 Node 24，暂未处理 |
| `src-tauri/src/commands/ccr.rs`（新增） | CCR 管理 RPC 薄代理：`ccr_call` / `ccr_health`，转发 `POST /api/ccr/rpc` | 模型层唯一内核是 CCR；Codeg 只做转发、不做模型写入 |
| `src-tauri/src/web/handlers/ccr.rs`（新增） | web 模式处理器（`/api/ccr_health`、`/api/ccr_call`） | 同上（Codeg 双模式规范） |
| `src-tauri/src/commands/mod.rs`、`src-tauri/src/web/handlers/mod.rs`、`src-tauri/src/web/router.rs`、`src-tauri/src/lib.rs` | 注册上述命令与路由 | 同上 |
| `src/lib/api.ts` | 新增 `ccrCall` / `ccrHealth` 前端通道 | 同上 |

## 构建与开发

前置：Node ≥ 22（本机 26.x）、**pnpm 11.9.0**（`package.json` 的 `packageManager` 锁定；
本机全局是 9.15.9，直接 `pnpm install` 会报 `packages field missing or empty`，用
`npx -y pnpm@11.9.0` 或先安装 pnpm 11）、Rust stable（本机 1.96.1）。

```bash
cd codeg
npx -y pnpm@11.9.0 install   # 前端依赖（postinstall 会把 monaco 拷到 public/vs）

# 前端（Next.js 静态导出）
pnpm dev                # 开发服务器
pnpm build              # 静态导出构建
pnpm lint && pnpm test  # 检查与测试

# 桌面应用（Tauri）
pnpm tauri dev          # 开发运行（beforeDevCommand 先起 Next）
pnpm tauri build        # 打包

# 服务器模式
pnpm server:build
pnpm server:dev
```

Rust 侧检查（在 `codeg/src-tauri/` 下执行）：

```bash
cargo check
cargo test --features test-utils
cargo clippy --all-targets --features test-utils -- -D warnings
cargo check --no-default-features --features server-bin --bin codeg-server
```

## 升级上游

1. 记录新的 upstream commit（`spacering-net/codeg`）；
2. 以相同排除项（`.git/`、`docs/images/`）获取源码快照，与本目录 `diff`；
3. 重放上表所有 `[crewchat]` 改动；
4. 更新本文件的 Vendored commit；
5. 跑 `pnpm lint && pnpm test`（前端）与 `cargo check`（Rust），确认仍可构建。
