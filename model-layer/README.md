# crewchat-model

CrewChat 的 **模型层**：本机模型网关 + 控制面，取代旧的外置路由进程。

## 端口

- **网关** `127.0.0.1:3456`：`/v1/models`、`/v1/messages`、`/v1/chat/completions`、`/v1/responses`
- **控制面** `127.0.0.1:3458`：`POST /api/model/rpc`，头 `x-model-web-auth`

## 运行

```sh
cargo run
```

AgentOS 在启动 `crewchat-cli` 前拉起本服务，并把以下环境传给它：

```
MODEL_LAYER_URL=http://127.0.0.1:3458
MODEL_LAYER_GATEWAY_URL=http://127.0.0.1:3456
MODEL_LAYER_TOKEN=<control token>
```

环境变量：

| 变量 | 默认 | 说明 |
| --- | --- | --- |
| `MODEL_LAYER_DATA_DIR` | `~/Library/Application Support/AgentOS/model` | SQLite / 快照 / runtime |
| `MODEL_LAYER_HOST` | `127.0.0.1` | 只允许回环 |
| `MODEL_LAYER_GATEWAY_PORT` | `3456` | 网关端口 |
| `MODEL_LAYER_CONTROL_PORT` | `3458` | 控制面端口 |
| `MODEL_LAYER_CONTROL_TOKEN` | 随机 | 控制面 token |
| `MODEL_LAYER_APPLY_PROFILE` | `0` | 是否同步写全局 Claude 配置 |

## 数据

- `model.db`：供应商元数据、模型目录、调用方 key 引用
- `claude-models.json`：Claude 可见模型快照（含 `[1m]`）
- `model-layer.json`：pid / 端口 / token（0600）
- 密钥：macOS Keychain（service `com.crewchat.model-layer`）

## 模型 ID

- Claude 虚拟 ID：`anthropic/claude-crewchat-h<hex>[1m]`。
- 人读 label：`Provider/model`
- OpenCode：`crewchat/<model>`
- 裸 model：仅唯一 provider 时接受

## 测试

```sh
cargo test
cargo clippy --all-targets -- -D warnings
```
