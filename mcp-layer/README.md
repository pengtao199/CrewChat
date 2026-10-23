# crewchat-mcp

CrewChat 的 **MCP 层**：全局 MCP 注册表、每个 Agent 的网关身份，以及 ACP 会话配置。
对外统一叫 `mcp-layer`；运行时网关是一个独立 sidecar，内部细节不暴露给前端。

## 形态

```
AgentOS / cli-layer
  → mcp-layer (127.0.0.1:3461)
      ├── ~/.agents/mcp/servers/<id>.json
      ├── ~/.agents/mcp/registry.sqlite
      └── 拉起 gateway sidecar (127.0.0.1:3462)
            └── /mcp + per-Agent client token / allowlist
```

- Agent 的 `mcps` 是 allowlist；空 = 没有 MCP。
- `mcp-layer` 为每个 Agent 建一个网关 client token；CLI 会话拿 token 连网关。
- 旧 CLI MCP 配置**不迁移、不读取**。

## 构建

```sh
# Rust control plane
cargo build

# Gateway sidecar（首次需要 Go 依赖）
./scripts/build-gateway.sh

# 默认打印阶段、模块下载命令和包编译进度；安静模式：
VERBOSE=0 ./scripts/build-gateway.sh
```

`MCP_LAYER_GATEWAY_BIN` 可覆盖 sidecar 路径；默认找
`mcp-layer/bin/crewchat-mcp-gateway`。

### 构建网络问题

当前构建依赖 `proxy.golang.org`。如果下载大模块（例如 SQLite）时出现
`i/o timeout` 或 `unexpected EOF`，这是网络传输问题，不是依赖冲突。

优先在稳定网络下重跑；成功一次后 Go cache 会复用。若目标机器长期无法访问：

1. 在有网络的机器上执行 `go mod download`，再执行 `go mod vendor`，把
   `vendor/gateway/vendor/` 一起提交；构建时自动走离线 vendor。
2. 或使用同版本预构建 sidecar，并校验来源/哈希。
3. 不建议为此重写整个网关；如确需 Rust 实现，只做覆盖 CrewChat 需求的最小
   gateway，而不是复刻上游全部能力。

## 运行

```sh
cargo run
```

默认 `127.0.0.1:3461`，运行时文件 `~/.agents/mcp/runtime.json`（0600）。

环境变量：

| 变量 | 默认 | 说明 |
| --- | --- | --- |
| `MCP_LAYER_ROOT` | `~/.agents/mcp` | 注册表与运行时根目录 |
| `MCP_LAYER_HOST` | `127.0.0.1` | 只允许回环 |
| `MCP_LAYER_PORT` | `3461` | 控制端口 |
| `MCP_LAYER_TOKEN` | 随机生成 | 控制面 Bearer token |
| `MCP_LAYER_GATEWAY_PORT` | `3462` | 网关端口 |
| `MCP_LAYER_GATEWAY_BIN` | 见上 | sidecar 二进制 |
| `MCP_LAYER_NPX` | PATH 中的 npx | stdio bridge 的绝对路径 |

## API（后端核心）

所有请求：`Authorization: Bearer <runtime.json token>`。

| 方法 | 路径 | 作用 |
| --- | --- | --- |
| `GET` | `/v1/health` | 控制面 + 网关状态 |
| `GET` | `/v1/servers` | MCP 库 |
| `GET` | `/v1/servers/{id}` | 单个 MCP |
| `PUT` | `/v1/servers/{id}` | 新建/更新（写 `servers/<id>.json` 并同步网关） |
| `DELETE` | `/v1/servers/{id}` | 删除（同时清理 Agent allowlist） |
| `POST` | `/v1/servers/{id}/enable` / `disable` | 启停 |
| `GET` | `/v1/agents` | Agent 网关身份列表 |
| `GET` | `/v1/agents/{id}` | 单个 Agent 身份 |
| `PUT` | `/v1/agents/{id}` | 设置 Agent allowlist，返回会话配置 |
| `DELETE` | `/v1/agents/{id}` | 删除 Agent 身份/token |
| `GET` | `/v1/gateway` | 网关状态 |
| `POST` | `/v1/gateway/start` / `stop` / `sync` | 生命周期与 reconcile |

### Agent 会话返回

`PUT /v1/agents/{id}` body：

```json
{ "serverIds": ["backend-api", "postgres"] }
```

返回：

```json
{
  "agentId": "role-1",
  "clientName": "agent-role-1",
  "serverIds": ["backend-api", "postgres"],
  "endpoint": "http://127.0.0.1:3462/mcp",
  "token": "<agent token>",
  "http": { "type": "http", "name": "mcp-layer", "url": "...", "headers": [...] },
  "stdio": { "type": "stdio", "name": "mcp-layer", "command": "/abs/npx", "args": [...] },
  "mcpServers": [ /* stdio 优先；无 npx 时回退 http */ ]
}
```

- `mcpServers` 是 ACP `session/new` 可直接消费的形状。
- stdio 用开源 `mcp-remote` 桥接网关；所有 ACP Agent 都支持 stdio。
- 任一 server 缺失 → `409 server_missing`，不部分绑定。
- 空 `serverIds` → 删除网关 client，返回空 `mcpServers`。

## 当前状态

- 控制面、注册表、Agent token/allowlist、网关 sidecar 生命周期已实现。
- 6 个测试通过（registry / spec / Agent session / fake gateway），clippy 通过。
- 网关 sidecar 复用固定版本的开源实现，内部路径 `vendor/gateway`；许可证见
  `vendor/gateway/NOTICE.md`。
- 本机 `go build` 下载依赖时网络被重置，sidecar 二进制尚未构建；网络恢复后运行
  `./scripts/build-gateway.sh`。
- 前端、cli-layer 的 `agent_mcps_set` / `session/new.mcpServers` 接线不在本 crate 内。
- 不做旧数据迁移。
