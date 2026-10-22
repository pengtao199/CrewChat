# crewchat-model

CrewChat 的模型层：一个只监听回环地址的 Rust sidecar，取代 CCR 的运行时，同时保留
codeg 已经依赖的 CCR 兼容面。

- **网关** `127.0.0.1:3456`：`/v1/models`、`/v1/messages`、`/v1/chat/completions`、`/v1/responses`
- **控制面** `127.0.0.1:3458`：`POST /api/ccr/rpc`（`getConfig` / `saveConfig` / `probeProvider` / …）

它不感知 Agent，也不绑定 CLI。Agent → `(engine, provider, model)` 的绑定在
`codeg.db` 的 `custom_agent`；每个会话由 `agent_host/overlay` 注入自己的模型。
网关只按请求里的 `model` 路由，所以同一个 CLI 的多个 Agent 可以并发跑不同模型。

设计背景见 [`docs/plans/model-layer-remove-ccr.md`](../docs/plans/model-layer-remove-ccr.md)。

## 构建

```sh
cd model-layer
cargo build --release
```

### 装进 AgentOS（开发用）

```sh
model-layer/scripts/install-dev.sh
```

脚本会把 `crewchat-model` 放进 Debug 的 `AgentOS.app/Contents/MacOS/` 并重新 ad-hoc 签名。
重启 AgentOS 后，壳会在启动 `codeg-server` 之前拉起模型层，并把
`CCR_WEB_URL` / `CCR_WEB_AUTH_TOKEN` / `CODEG_CCR_SUPERVISE=0` 传给核心。

如果 `3456` / `3458` 已被 CCR 占用，壳会自动选空闲端口并把 codeg 指过去，
所以模型层和 CCR 可以并排跑，先验证再接替。壳必须在**自己启动核心**时才会拉起模型层：
已经完全退出 AgentOS（包括旧的 core）再重启。

### 切换与回退

```sh
# 停 CCR、重启 AgentOS（模型层接管 3456/3458）、把全局 claude 指过来
model-layer/scripts/cutover.sh

# 回退：恢复 settings 备份 + 启动 CCR
model-layer/scripts/rollback.sh
```

`cutover.sh` 只杀命令行里带 `claude-code-router` / `ccr` 的监听进程；端口被别的
进程占着会直接退出，不会误杀。

## 运行

```sh
MODEL_LAYER_CONTROL_TOKEN=... ./target/release/crewchat-model
```

启动时会：

1. 从**运行中的 CCR**（`getConfig`，默认 `127.0.0.1:3458`）导入目录；CCR 不在时回退读
   `~/.claude-code-router/config.sqlite`。
2. 用 `local-gateway` key 读一次源网关的 `/v1/models`（Claude UA），把 `[1m]` 标记写进库。
3. 合并 codeg 的 `ccr-disabled-models.json`（如果存在）。
4. 写 `<data dir>/model-layer.json`（0600），含两个 URL、控制面 token、PID。

不会覆盖 CCR 的 `service.json`，也不会写任何全局 CLI 配置。

### 环境变量

| 变量 | 默认 | 说明 |
| --- | --- | --- |
| `MODEL_LAYER_DATA_DIR` | `~/Library/Application Support/AgentOS/model` | `model.db` 与运行时文件 |
| `MODEL_LAYER_HOST` | `127.0.0.1` | 只允许回环 |
| `MODEL_LAYER_GATEWAY_PORT` | `3456` | 网关 |
| `MODEL_LAYER_CONTROL_PORT` | `3458` | 控制面 |
| `MODEL_LAYER_CONTROL_TOKEN` | `CCR_WEB_AUTH_TOKEN` 或随机 | 控制面 `x-ccr-web-auth` |
| `CCR_HOME` | `~/.claude-code-router` | 导入源目录 |
| `CCR_WEB_URL` | 从 `service.json` 发现 | 运行中的 CCR 控制面 |
| `MODEL_LAYER_DISABLED_MODELS` | `$CODEG_DATA_DIR/ccr-disabled-models.json` | 停用清单 |
| `MODEL_LAYER_IMPORT_ON_START` | `1` | 设为 `0` 跳过导入 |
| `MODEL_LAYER_APPLY_PROFILE` | `0` | 设为 `1` 时每次 `saveConfig` 也写 `~/.claude/settings.json` |

### 让 codeg 接过来（切换阶段）

```sh
CCR_WEB_URL=http://127.0.0.1:3458 \
CCR_WEB_AUTH_TOKEN=<MODEL_LAYER_CONTROL_TOKEN> \
CODEG_CCR_SUPERVISE=0 \
  codeg-server ...
```

codeg 的模型页、`body_model_face` 和 overlay 不需要改代码。

## 端点

| 端点 | 作用 |
| --- | --- |
| `GET /v1/models` | Claude UA → `anthropic/claude-ccr-h<hex>[1m]`；其他 UA → `Provider/model` |
| `POST /v1/messages` | Anthropic Messages → Chat（流式与非流式）；`anthropic_messages` 供应商直接透传 |
| `POST /v1/messages/count_tokens` | 估算 |
| `POST /v1/chat/completions` | 透传 |
| `POST /v1/responses` | 透传 |
| `POST /api/ccr/rpc` | CCR 兼容控制面 |

模型解析顺序：Claude 虚拟 ID → `crewchat/<model>` → `Provider/model` → 裸 model
（只有唯一 provider 服务时才接受，否则 400）。路由同时接受 `models` 与
`autoFetchKnownModels`（Codex/OpenCode 的模型面会列出两者）；`/v1/models` 仍只列
`models`，保持 Claude 的 38 个 ID 不变。

## 当前状态（2026-10-22）

已完成：

- Store + 从运行中的 CCR 导入；原始 provider JSON 整段保留。
- 控制面：`getConfig` / `saveConfig`（revision 409）/ `probeProvider` /
  `getProviderPresets` / `getGatewayStatus` / 启停 no-op；模型/凭据开关走
  `saveConfig`，实时生效。
- `/v1/models` 两种 UA；实测 38 个 ID、8 个 `[1m]` 与现网一致。
- `/v1/chat/completions`、`/v1/responses` 透传。
- `/v1/messages` → Chat 翻译：文本、图片、工具调用、工具结果、流式 SSE、stop_reason、usage。
- 请求级凭据选择（priority + weight），无全局「当前凭据」。
- 额度：`account.connectors` 的 `http-json` mapping（受限 JSONPath + 四则运算），
  实测 OpenCode Go 2 条凭据 × 3 个 meter；失败返回缓存 + 错误，不伪造。
- `applyProfile`：写 `~/.claude/settings.json` + 自己的 `apiKeyHelper`；
  Agent 会话不依赖它。
- AgentOS 接线（启动 / 端口回退 / 健康检查）与 `cutover.sh` / `rollback.sh`。

未完成 / 下一步：

- Responses 面的 Anthropic 翻译（目前 Claude 走 Chat；OpenCode Go 的 Chat 端点可用）。
- 正式打包的 copy phase（开发用 `install-dev.sh` / scheme 已可用）。
- Keychain 迁移（现在沿用 0600 SQLite）。

## 测试

```sh
cargo test          # 模型解析、store 往返、翻译、SSE 状态机
cargo clippy -- -D warnings
```

本地冒烟（不碰真实模型）：

```sh
# 用另一组端口 + 临时数据目录，只读导入现网 CCR
MODEL_LAYER_DATA_DIR=/tmp/crewchat-model \
MODEL_LAYER_GATEWAY_PORT=3466 MODEL_LAYER_CONTROL_PORT=3468 \
MODEL_LAYER_CONTROL_TOKEN=smoke-token \
  cargo run
```

## 许可

MIT。协议映射的参考实现与署名见 [`NOTICE.md`](NOTICE.md)。
