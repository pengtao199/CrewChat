# 模型接入（模型层）

## 适用范围

改动模型接入、上游端点、模型页后端接线、Agent 的模型注入，或遇到「某 CLI 不支持某端点 / 某端点的工具格式被拒」类问题时。

## 现状

模型统一管理由 **`model-layer/`**（Rust，二进制 `crewchat-model`）承担：

- **控制面**：`127.0.0.1:3458`，`POST /api/model/rpc`（头 `x-model-web-auth`）。
- **网关**：`127.0.0.1:3456`，`/v1/models`、`/v1/messages`、`/v1/chat/completions`、`/v1/responses`。
- AgentOS 启动 `crewchat-cli` 前拉起模型层，把 `MODEL_LAYER_URL` / `MODEL_LAYER_GATEWAY_URL` / `MODEL_LAYER_TOKEN` 传给 cli-layer。
- cli-layer 通过 `model_*` 命令代理模型页；模型页的真相源是模型层。

模型层不感知 Agent，也不绑定 CLI。Agent 的 `model_provider` + `model` 存在 `cli-layer.db` 的 `agent_roles`；每个会话由 `cli-layer` 建独立 CLI home 注入模型，网关只按请求里的 `model` 路由：

| 形态 | 例子 | 谁发 |
| --- | --- | --- |
| Claude 虚拟 ID | `anthropic/claude-crewchat-h<hex>[1m]` | Claude Code |
| 人读 label | `OpenCode Go/mimo-v2.6-flash` | 全局配置 / Agent 页 |
| `crewchat/<model>` | `crewchat/mimo-v2.6-flash` | OpenCode overlay |
| 裸 model | `mimo-v2.6-flash` | Codex overlay；仅唯一 provider 时接受 |

**同一个 CLI 的多个 Agent 可以并发跑不同模型。** 不要引入「当前模型」或「CLI → 模型」的全局状态。

## 原则

- **单一写入者**：模型数据只经模型层；模型页通过 cli-layer `model_*` 命令写入。
- **协议差异集中在模型层**：上游不支持某 CLI 的线格式时在翻译层解决，不进入业务代码。
- **端点是配置不是代码**：新增推理端点 = 模型页加 provider / 模型；不改 Swift 壳。
- **密钥只落一处**：provider key 与 caller key 在模型层 Keychain（service `com.crewchat.model-layer`），SQLite 只存引用；不写进代码、日志或前端。
- **Agent 绑定只在花名册**：模型层不知道 Agent 存在；会话注入归 cli-layer。

## 具体规则

- 模型页写供应商走 `model_provider_save/delete/probe`；cli-layer 做 `getConfig → 合并原始 JSON → saveConfig`，保留 `transformer` / `modelMetadata` / `account` 等壳不认识字段；不在 Swift 里拼整份 config。
- 供应商改名暂不开放：Claude 虚拟 ID 编码的是 `Provider 显示名/model`，改名会留下悬空 Agent。删除前核心检查引用。
- 模型删除保留 tombstone；Agent 页显示「模型不可用」，不静默回退。
- `[1m]` 是 Claude 可见 ID 的后缀：store 里记 `one_million` 布尔，转发上游时剥掉，不建两条模型。
- `crewchat/<model>` 是 OpenCode 配置里的 provider 名，网关路由前剥掉；裸 model 命中多个 provider 返回 400。
- 会话模型注入只用 cli-layer overlay；`applyProfile` 只服务「用户在终端裸跑 CLI」，不是会话前置条件。
- 额度走 `account.connectors` 的 `http-json` mapping（受限 JSONPath + 四则运算）；拉不到显示缓存/错误，不伪造。
- 改模型层代码前读 `model-layer/README.md` 与 `model-layer/NOTICE.md`。

## 禁止项

- 不在 Swift 壳里做模型存储、路由或协议翻译。
- 不写「CLI → 模型」的全局绑定；绑定只属于 Agent。
- 不绕过模型层直连端点「验证一下」然后把结果写进代码。
- 不把网关暴露到非回环地址。
- 不让会话依赖全局 `~/.claude/settings.json` 或 `applyProfile`。

## 检查

- 模型页是否仍然只经 cli-layer 代理？
- 密钥是否只在 Keychain/模型层？
- 新端点是否通过配置而不是代码硬编码？
- 会话的模型是否来自 Agent，而不是 CLI 全局配置？
