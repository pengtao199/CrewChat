# 模型接入（CCR）

## 适用范围

改动模型接入、上游端点、CCR profile / transform、Codeg 模型页的后端接线，或遇到"某 CLI 不支持某端点 / 某端点的工具格式被拒"类问题时。

## 现状

模型统一管理由 **CCR（claude-code-router）** 承担，源码固化进本仓库 `ccr/`。运行时有两侧：

- **控制面**：CCR 管理进程（默认 `127.0.0.1:3458`），RPC 入口 `POST /api/ccr/rpc`（头 `x-ccr-web-auth`）；
- **网关**：本地代理（默认 `127.0.0.1:3456`），按 profile 把请求转给上游。

Codeg 工作台的模型页经薄代理操作 CCR：后端 `ccr_call` / `ccr_health`（`codeg/src-tauri/src/commands/ccr.rs`），前端 `codeg/src/lib/ccr.ts` 与 `codeg/src/components/settings/ccr/` 下的组件。

**Python 侧不再有网关。** 旧的 cc-switch 代理核心（`gateway/cc-switch/`、`ccsw-proxy`）已移除：Python 身体不再注入模型，改为使用各自 CLI 的登录与用户配置（见 `AGENTS/bodies.md`）。模型统一管理只走 CCR。

## 原则

- **单一写入者**：模型接入只经 CCR。Codeg 侧 Agent 的模型保持 Default（机制 1：由 CCR 写各 CLI 的全局配置）。
- **协议差异集中在 CCR**：上游不支持某 CLI 的原生线格式时，在 CCR 的 profile / transform 里解决，不进入业务代码。
- **端点是配置不是代码**：新增推理端点 = 在 CCR 加 provider，并由 profile 引用；不写进 Python 或 Codeg 代码。
- **密钥只落一处**：端点 API key 存在 CCR 的供应商配置里（落点见 `AGENTS/truth-sources.md`）；不写进代码、日志或前端。

## 具体规则

- 新增推理端点：只改 CCR 配置（模型页操作或直接改 CCR 配置），不动 Python / Codeg 代码。
- 某 CLI 拿不到某端点时：先确认该 CLI 在 CCR 里有 profile，再看端点是否要 transform，最后才怀疑端点。
- 改 CCR 自身代码前读 `ccr/NOTICE.md`（变更清单）与 `AGENTS/upstream.md`（同步规则）；我们的改动带 `[crewchat]` 标记。

## 禁止项

- 不在 Python 侧重造模型路由 / 反代（旧 cc-switch 正是因此移除）。
- 不在身体里手写线格式转换（Responses ↔ Chat ↔ Messages）；不在客户端发现"工具不被接受"后用 `--disable` 之类开关绕过。
- 不绕过 CCR 直连端点去"验证一下能不能用"然后写进代码。
- 不把 CCR 网关暴露到非本机地址而不设鉴权。

## 检查

- 这次改动是否只落在 CCR 配置（而不是代码）？
- 换一个新的 OpenAI 兼容端点，需要改几处？理想是只在 CCR 加一条 provider + profile。
- Python 侧是否又出现了模型注入的痕迹？（不应有）
