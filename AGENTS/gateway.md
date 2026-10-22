# 模型接入（模型层）

## 适用范围

改动模型接入、上游端点、模型页后端接线、Agent 的模型注入，或遇到「某 CLI 不支持某端点 / 某端点的工具格式被拒」类问题时。

## 现状（2026-10-22）

模型统一管理由 **`model-layer/`**（Rust，二进制 `crewchat-model`）承担。运行时有两侧：

- **控制面**：`127.0.0.1:3458`，`POST /api/ccr/rpc`（头 `x-ccr-web-auth`）；
- **网关**：`127.0.0.1:3456`，`/v1/models`、`/v1/messages`、`/v1/chat/completions`、`/v1/responses`。

codeg 仍经 `ccr_call` / `ccr_health` 操作它（`codeg/src-tauri/src/ccr/`），RPC 形状保持 CCR 兼容；AgentOS 在启动 `codeg-server` 前拉起模型层，3456/3458 被占用时自动改用空闲端口，并把 `CCR_WEB_URL` / `CCR_WEB_AUTH_TOKEN` / `CODEG_CCR_SUPERVISE=0` 传给核心。

**`ccr/` 是 legacy 回退**：源码与配置保留一个版本，用于回滚，不再是运行时依赖。切换与回滚脚本在 `model-layer/scripts/`。

### 模型与 Agent 的关系

模型层**不感知 Agent，也不绑定 CLI**。Agent 的 `brain_provider` + `brain_model` 存在 `codeg.db` 的 `custom_agent`；每个会话由 `agent_host/overlay` 建独立 home 注入模型，网关只按请求里的 `model` 路由：

| 形态 | 例子 | 谁发 |
| --- | --- | --- |
| Claude 虚拟 ID | `anthropic/claude-ccr-h<hex>[1m]` | Claude Code |
| 人读 label | `OpenCode Go/mimo-v2.6-flash` | 全局配置 / Agent 页 |
| `crewchat/<model>` | `crewchat/mimo-v2.6-flash` | OpenCode overlay |
| 裸 model | `mimo-v2.6-flash` | Codex overlay；仅唯一 provider 时接受 |

**同一个 CLI 的多个 Agent 可以并发跑不同模型。** 不要引入「当前模型」或「CLI → 模型」的全局状态。

## 原则

- **单一写入者**：模型数据只经模型层；CCR 与模型层不同时写同一份配置。
- **协议差异集中在模型层**：上游不支持某 CLI 的线格式时在翻译层解决，不进入业务代码。
- **端点是配置不是代码**：新增推理端点 = 模型页加 provider / 模型；不改 Swift 壳。
- **密钥只落一处**：provider key 与 caller key 在模型层 SQLite（0600）；不写进代码、日志或前端。
- **Agent 绑定只在花名册**：模型层不知道 Agent 存在；会话注入归核心。

## 具体规则

- 模型页写供应商走核心 `ccr_provider_save/delete/probe`：核心做 `getConfig → 合并原始 JSON → saveConfig`，保留 `transformer` / `modelMetadata` / `account` 等壳不认识字段；不在 Swift 里拼整份 config。
- 供应商改名暂不开放：Claude 虚拟 ID 编码的是 `Provider 显示名/model`，改名会留下悬空 Agent。删除前核心检查引用。
- 模型删除保留 tombstone；Agent 页显示「模型不可用」，不静默回退。
- `[1m]` 是 Claude 可见 ID 的后缀：store 里记 `one_million` 布尔，转发上游时剥掉，不建两条模型。
- `crewchat/<model>` 是 OpenCode 配置里的 provider 名，网关路由前剥掉；裸 model 命中多个 provider 返回 400。
- 会话模型注入只用 `agent_host/overlay`；`applyProfile` 只服务「用户在终端裸跑 CLI」，不是会话前置条件。
- `applyProfile` 写 `~/.claude/settings.json`（含我们自己的 `apiKeyHelper`）与 provider key；`MODEL_LAYER_APPLY_PROFILE=1` 时 `saveConfig` 也会顺带应用，默认关闭以免 side-by-side 期间移动用户的全局 CLI。
- 额度走 `account.connectors` 的 `http-json` mapping（受限 JSONPath + 四则运算）；拉不到显示缓存/错误，不伪造。
- 改模型层代码前读 `model-layer/README.md` 与 `model-layer/NOTICE.md`；改 `ccr/` 只用于回退修复。

## 禁止项

- 不在 Swift 壳里做模型存储、路由或协议翻译。
- 不写「CLI → 模型」的全局绑定；绑定只属于 Agent。
- 不绕过模型层直连端点「验证一下」然后把结果写进代码。
- 不把网关暴露到非回环地址。
- 不让会话依赖全局 `~/.claude/settings.json` 或 `applyProfile`。

## 检查

- 这次改动落在模型层、模型页接线，还是 Agent 注入？该在哪一层？
- 换一个新的 OpenAI 兼容端点，需要改几处？理想是只在模型页加一条 provider + 模型。
- 是否引入了跨请求的「当前模型 / 当前凭据」状态？（不应有）
- 密钥是否只出现在模型层 DB（0600）里？
