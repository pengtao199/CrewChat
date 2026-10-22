# 模型接入（CCR）

## 适用范围

改动模型接入、上游端点、CCR profile / transform、Codeg 模型页的后端接线，或遇到"某 CLI 不支持某端点 / 某端点的工具格式被拒"类问题时。

## 现状

> 计划中（未实施）：模型层将以 Rust sidecar 取代 CCR，见 [`docs/plans/model-layer-remove-ccr.md`](../docs/plans/model-layer-remove-ccr.md)。切换前本文件仍是运行时规则。

模型统一管理由 **CCR（claude-code-router）** 承担，源码固化进本仓库 `ccr/`。运行时有两侧：

- **控制面**：CCR 管理进程（默认 `127.0.0.1:3458`），RPC 入口 `POST /api/ccr/rpc`（头 `x-ccr-web-auth`）；
- **网关**：本地代理（默认 `127.0.0.1:3456`），按 profile 把请求转给上游。

Codeg 工作台的模型页经薄代理操作 CCR：后端 `ccr_call` / `ccr_health`（`codeg/src-tauri/src/commands/ccr.rs`，传输在 `codeg/src-tauri/src/ccr/client.rs`），前端 `codeg/src/lib/ccr.ts` 与 `codeg/src/components/settings/ccr/` 下的组件。

**CCR 进程由 Codeg 托管（5.1）。** 管理进程本身是独立进程，以前要用户手动起。现在 `codeg/src-tauri/src/ccr/supervisor.rs` 负责：探活（TCP，不看 token）→ 已在运行就直接接管（不打扰）→ 不可达且满足条件才 `ccr start`（CCR 自带 daemon：写 `service.json`、幂等复用）→ 守护循环按退避重试。**只起不停**，停是用户决定。环境契约：

| 变量 | 作用 |
| --- | --- |
| `CCR_WEB_URL` / `CCR_WEB_AUTH_TOKEN` | 管理面地址与 token。**两者都有发现回退**：CCR 自己写 `<ccr home>/service.json`（`url` 带 `?ccr_web_token=…`），Codeg 读它，所以双击启动的桌面应用无需任何环境变量；env 仍然优先（CI / 远程管理地址 / 故意覆盖 token）。token 仍建议固定，否则本地拉起的实例每次用随机 token（Codeg 能从 service.json 重新读到，但历史日志里的旧 token 就作废了） |
| `CCR_BIN` | `ccr` CLI 路径（打包时必需；不设则从 `PATH` 找） |
| `CODEG_CCR_SUPERVISE=0` | 关掉托管（自己管 CCR 的用户） |

托管只在回环地址 + token 已固定 + 能解析出 `ccr` CLI 时生效，否则只在状态卡上报「无法自启」的原因。

### 本机常驻（launchd，2026-10-19）

本机 `ccr` **不在 PATH**、`CCR_BIN` 也没设，所以 Codeg 只能"接管"不能"自启"；而手工 `ccr start` 起的实例
会随终端/会话结束被清理——**这就是"CCR 一直不稳定"的真因**（CCR 死时 `service.json` 也没了，Codeg 连发现
都无从谈起）。改为 **launchd 常驻**，Codeg 只负责接管：

- 文件 `~/Library/LaunchAgents/com.crewchat.ccr.plist`：`node …/@musistudio/claude-code-router/dist/main/cli.js
  start --daemon --no-open`，固定 `CCR_WEB_AUTH_TOKEN`（落进 `service.json`，Codeg 据此发现；**token 只存在
  plist 里，不进仓库**），`RunAtLoad` + `StartInterval=120`。
- **必须直接调 node**：`ccr` 是 `#!/usr/bin/env node` 脚本，launchd 的 PATH 里没有 node（`exit 127`）。
- **不用 `KeepAlive`**：`ccr start` 会 daemon 化后立刻退出，KeepAlive 会无限重启；`StartInterval` 才是对的
  ——`ccr start` 幂等（已在跑就打印 `already running` 并以 0 退出），120s 一次正好当 watchdog。
- **一条命令装 / 重载 / 自检**：`bash codeg/scripts/ccr-agent-mac.sh`（生成 plist → `launchctl bootstrap`
  → 打印 3458/3456 与 `service.json`）。token 从 `~/.config/crewchat/ccr-token` 读，首次自动生成，
  **仓库外、不进 git**。手工等价命令：
  ```bash
  launchctl bootout gui/$UID ~/Library/LaunchAgents/com.crewchat.ccr.plist 2>/dev/null
  launchctl bootstrap gui/$UID ~/Library/LaunchAgents/com.crewchat.ccr.plist
  launchctl print gui/$UID/com.crewchat.ccr | head
  tail -f ~/Library/Logs/crewchat-ccr.log
  ```
- 校验（管理面 3458 / 网关 3456 / 发现文件）：
  ```bash
  lsof -nP -iTCP -sTCP:LISTEN | grep -E "3456|3458"
  cat ~/.claude-code-router/service.json      # url 里带 ?ccr_web_token=…
  ```

**Python 侧不再有网关。** 旧的 cc-switch 代理核心（`gateway/cc-switch/`、`ccsw-proxy`）已移除：Python 身体不再注入模型，改为使用各自 CLI 的登录与用户配置（见 `AGENTS/bodies.md`）。模型统一管理只走 CCR。

## 原则

- **单一写入者**：模型接入只经 CCR。Codeg 侧 Agent 的模型保持 Default（机制 1：由 CCR 写各 CLI 的全局配置）。
- **协议差异集中在 CCR**：上游不支持某 CLI 的原生线格式时，在 CCR 的 profile / transform 里解决，不进入业务代码。
- **端点是配置不是代码**：新增推理端点 = 在 CCR 加 provider，并由 profile 引用；不写进 Python 或 Codeg 代码。
- **密钥只落一处**：端点 API key 存在 CCR 的供应商配置里（落点见 `AGENTS/truth-sources.md`）；不写进代码、日志或前端。

## 具体规则

- 新增推理端点：只改 CCR 配置（模型页操作或直接改 CCR 配置），不动 Python / Codeg 代码。
- 模型页写供应商只能走核心的 `ccr_provider_save/delete/probe`：核心做 `getConfig → 合并原始 JSON → saveConfig`，保留 `transformer` / `modelMetadata` / `account` 等壳不认识字段；**不要**在 Swift 里拼整份 config 再回写。
- 供应商改名暂不开放：CCR 的 profile 用 `providerName` / `providerId` 引用它，改名会留下悬空档案。删除前核心会检查引用并拒绝，错误里带上档案名。
- 某 CLI 拿不到某端点时：先确认该 CLI 在 CCR 里有 profile，再看端点是否要 transform，最后才怀疑端点。
- 改 CCR 自身代码前读 `ccr/NOTICE.md`（变更清单）与 `AGENTS/upstream.md`（同步规则）；我们的改动带 `[crewchat]` 标记。
- 改托管逻辑：进程管理交给 `ccr start/stop`，不要在 Codeg 里重造 daemon / pidfile；探活判据（回环 / token / CLI）在 `supervisor.rs` 的纯函数里，改判据先补用例。

## 禁止项

- 不在 Python 侧重造模型路由 / 反代（旧 cc-switch 正是因此移除）。
- 不在身体里手写线格式转换（Responses ↔ Chat ↔ Messages）；不在客户端发现"工具不被接受"后用 `--disable` 之类开关绕过。
- 不绕过 CCR 直连端点去"验证一下能不能用"然后写进代码。
- 不把 CCR 网关暴露到非本机地址而不设鉴权。

## 检查

- 这次改动是否只落在 CCR 配置（而不是代码）？
- 换一个新的 OpenAI 兼容端点，需要改几处？理想是只在 CCR 加一条 provider + profile。
- Python 侧是否又出现了模型注入的痕迹？（不应有）
